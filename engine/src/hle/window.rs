//! Janela do jogo recompilado: winit + egui desenhado com wgpu (Vulkan; reserva DX12). As duas telas do 3DS
//! (`gpu::screen_rgba`) viram texturas do egui e o menu (Esc/F1, `menu.rs`) é desenhado por cima.
//!
//! Entrada: teclado (padrão da janela anterior, remapeável no menu — ver `settings::Teclado`), gamepads via `gilrs`
//! (disposição Xbox/Nintendo, remapeável), mouse esquerdo na tela de baixo = toque em qualquer layout/escala
//! (`present::touch_point`). Menu: Esc/F1, ou Start + Select juntos no controle (Guide quando o backend o envia).
//! F11 = tela cheia. Com o menu aberto o console fica pausado (`hle::set_paused`, pausa parcial: ver hle/mod.rs) e o
//! jogo não recebe entrada.
//! Configurações em `%APPDATA%/yw1/settings.toml` (`settings.rs`); `YW_RENDERER`, `YW_SCALE`, `YW_PAD_LAYOUT` e
//! `YW_VOLUME` continuam valendo por cima do arquivo. `WGPU_BACKEND=dx12` força o DX12 na apresentação.

use super::input::{self, Sample};
use super::menu::{self, Acao, Menu};
use super::fsr;
use super::present;
use super::services::dsp::output as audio;
use super::services::hid::{BUTTONS, STICK, TOUCH};
use super::settings::{self, Apresentacao, EnvOverrides, Filtro, LimiteFps, Renderizador, Settings};
use super::{crash_message, gpu};
use crate::render::vulkan::{Group, HiRes};
use egui_wgpu::winit::Painter;
use std::collections::HashSet;
use std::num::NonZeroU32;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, OwnedDisplayHandle};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Fullscreen, Window, WindowId};

/// Configuração lida antes do `boot()`: o que precisa valer desde o início (renderizador, idioma, volume).
pub struct Config {
    file: Settings,
    env: EnvOverrides,
    current: Settings,
}

impl Config {
    /// Lê o arquivo + ambiente e aplica o que vale desde o início. Chamar **antes** de `hle::boot()`.
    pub fn load() -> Config {
        let file = settings::load();
        let env = EnvOverrides::from_env();
        let current = env.apply(&file);
        // renderizador do PICA: só mexe se o ambiente não escolheu (render::backend() lê YW_RENDERER sozinho) e se
        // a escolha não é o padrão (Vulkan, iniciado sob demanda pelo próprio render)
        if !env.renderer_set && current.graficos.renderizador == Renderizador::Soft {
            crate::render::set_backend(crate::render::Backend::Soft);
        }
        super::services::misc::set_language(current.emulacao.idioma.codigo());
        audio::set_volume(current.som.volume);
        audio::set_muted(current.som.mudo);
        // YW_ANISO / YW_SCALE / YW_FXAA (ambiente) mandam sobre o arquivo
        if std::env::var_os("YW_ANISO").is_none() {
            crate::render::set_aniso(current.graficos.anisotropico);
        }
        if std::env::var_os("YW_SCALE").is_none() {
            crate::render::set_scale(current.graficos.escala_interna);
        }
        if std::env::var_os("YW_FXAA").is_none() {
            crate::render::set_fxaa(current.graficos.fxaa);
        }
        if std::env::var_os("YW_INTERP").is_none() {
            crate::render::set_interp(current.graficos.interpolacao);
        }
        // renderização assíncrona (YW_VK_ASYNC manda) e 3D estéreo (YW_STEREO manda; o jogo decide no boot)
        if std::env::var_os("YW_VK_ASYNC").is_none() {
            crate::render::set_async(current.graficos.renderizacao == settings::Renderizacao::Assincrona);
        }
        super::set_stereo_from_settings(current.graficos.estereo_3d);
        Config { file, env, current }
    }
}

/// O que a textura de uma tela representa hoje (para saber quando refazê-la).
#[derive(Clone, Copy, PartialEq)]
struct TexState {
    swaps: u64,
    filtro: Filtro,
    n: usize,
    fsr: bool,
    /// identidade da imagem em alta resolução em uso (0 = nativa)
    hi: usize,
    at: Option<Instant>,
}

impl Default for TexState {
    fn default() -> Self {
        TexState { swaps: u64::MAX, filtro: Filtro::Nitido, n: 1, fsr: false, hi: 0, at: None }
    }
}

/// Amplia uma imagem RGBA por um fator inteiro `n` com vizinho mais próximo (cada pixel vira n×n).
fn ampliar(w: usize, h: usize, px: &[u8], n: usize) -> (usize, usize, Vec<u8>) {
    let (nw, nh) = (w * n, h * n);
    let mut out = vec![0u8; nw * nh * 4];
    for y in 0..h {
        let linha = &mut out[(y * n) * nw * 4..(y * n + 1) * nw * 4];
        for x in 0..w {
            let p = &px[(y * w + x) * 4..(y * w + x) * 4 + 4];
            for k in 0..n {
                linha[(x * n + k) * 4..(x * n + k) * 4 + 4].copy_from_slice(p);
            }
        }
        for k in 1..n {
            out.copy_within((y * n) * nw * 4..(y * n + 1) * nw * 4, (y * n + k) * nw * 4);
        }
    }
    (nw, nh, out)
}

/// Cadência dos quadros interpolados de uma tela, como no Zelda 64 Recompiled (RT64) e no Ship of Harkinian: um quadro do
/// jogo de atraso, os intermediários em ordem, cada um pelo mesmo tempo (`period_ms / steps`), e por fim o quadro real.
///
/// O relógio de cada grupo começa quando a janela o vê pela primeira vez — a troca de framebuffer, que vem um VBlank
/// depois do DisplayTransfer — e não no DisplayTransfer. Começar lá fazia o primeiro intermediário vencer antes de
/// aparecer (pulado ou mostrado por poucos ms) e o quadro real ficar parado o resto do período: passos desiguais, que o
/// olho (que acompanha o movimento em velocidade constante) lê como a imagem indo e voltando. Dentro de um grupo o
/// índice nunca volta; o grupo seguinte é sempre mais novo.
#[derive(Default)]
struct Cadencia {
    /// grupo em exibição (endereço do `Arc`; 0 = nenhum)
    grupo: usize,
    inicio: Option<Instant>,
    idx: usize,
    passos: usize,
    /// último índice mostrado e quantos índices distintos deste grupo já foram mostrados
    ultimo: Option<usize>,
    mostrados: usize,
    /// última vez que esta tela teve imagem em alta resolução válida
    ultimo_hi: Option<Instant>,
    // medidas da janela de 5 s (linha `[janela]` do log)
    /// redesenhos que seguraram a imagem anterior esperando a nova (ver `montar_texturas`) e que caíram na nativa
    segurou: u32,
    nativa: u32,
    grupos: u32,
    passos_total: u32,
    imagens: u32,
    pulados: u32,
    atraso_ms: f64,
}

impl Cadencia {
    /// Imagem do grupo `g` a mostrar agora e o prazo do próximo passo (`None` no quadro real: fica até o grupo seguinte).
    fn escolher(&mut self, g: &Arc<Group>, now: Instant) -> (Arc<HiRes>, Option<Instant>) {
        let id = Arc::as_ptr(g) as usize;
        if id != self.grupo {
            self.encerrar();
            self.grupo = id;
            self.inicio = Some(now);
            self.idx = 0;
            self.passos = g.steps.max(1) as usize;
            self.grupos += 1;
            self.passos_total += self.passos as u32;
            self.atraso_ms += now.duration_since(g.t0).as_secs_f64() * 1000.0;
        }
        let inicio = self.inicio.unwrap_or(now);
        let passo = (g.period_ms as f64 / self.passos as f64).max(0.1) / 1000.0;
        let alvo = ((now.duration_since(inicio).as_secs_f64() / passo) as usize).min(self.passos - 1);
        self.idx = self.idx.max(alvo);
        if self.ultimo != Some(self.idx) {
            self.ultimo = Some(self.idx);
            self.mostrados += 1;
            self.imagens += 1;
        }
        let img = g.mids.get(self.idx).cloned().unwrap_or_else(|| g.real.clone());
        let prazo = (self.idx + 1 < self.passos).then(|| inicio + Duration::from_secs_f64(passo * (self.idx + 1) as f64));
        (img, prazo)
    }

    /// A tela deixou de mostrar o grupo (outro grupo, quadro sem interpolação, imagem nativa): conta os passos que não
    /// chegaram a aparecer.
    fn encerrar(&mut self) {
        if self.grupo != 0 {
            self.pulados += self.passos.saturating_sub(self.mostrados) as u32;
        }
        self.grupo = 0;
        self.ultimo = None;
        self.mostrados = 0;
    }

    /// Resumo para o log, zerando as medidas.
    fn resumo(&mut self, dt: f64) -> Option<String> {
        let mut r = Vec::new();
        if self.grupos > 0 {
            r.push(format!(
                "interpolação: {:.0} imagens/s, {:.1}% dos passos pulados, troca de framebuffer {:.1} ms depois do DisplayTransfer",
                self.imagens as f64 / dt,
                100.0 * self.pulados as f64 / self.passos_total.max(1) as f64,
                self.atraso_ms / self.grupos as f64
            ));
        }
        if self.segurou + self.nativa > 0 {
            r.push(format!("alta resolução atrasada: segurou a anterior {}×, caiu na nativa {}×", self.segurou, self.nativa));
        }
        (self.grupos, self.passos_total, self.imagens, self.pulados, self.atraso_ms, self.segurou, self.nativa) = (0, 0, 0, 0, 0.0, 0, 0);
        (!r.is_empty()).then(|| r.join(" · "))
    }
}

struct Gfx {
    win: Arc<Window>,
    painter: Painter,
    egui: egui_winit::State,
}

struct App {
    /// modos de apresentação que a GPU/janela aceitam (consultados uma vez, ao criar a janela)
    modos: Vec<wgpu::PresentMode>,
    /// quadros do jogo (soma das duas telas) vistos no último redesenho: mudou → apresenta já
    ultimos_swaps: u64,
    /// (início da janela de 5 s, redesenhos, quadros do jogo) para a linha `[janela]` do log
    medida: (Instant, u32, u64),
    /// imagens de tela enviadas e tempo gasto montando-as (cópia + conversão + textura) na janela de 5 s
    envio: (u32, Duration),
    /// ritmo das imagens NOVAS da tela de cima (conteúdo diferente do anterior): último instante e intervalos (ms) da
    /// janela de 5 s — o que o olho vê como movimento regular ou "vai e volta"
    ritmo: (Option<Instant>, Vec<f32>),
    cfg: Config,
    /// último conteúdo gravado no arquivo
    saved: Settings,
    display: OwnedDisplayHandle,
    ctx: egui::Context,
    gfx: Option<Gfx>,
    failed: bool,
    tex: [Option<egui::TextureHandle>; 2],
    tex_state: [TexState; 2],
    /// imagem de cada tela que vai pela GPU da janela (FSR ou o filtro no shader), a versão dela e o modo
    gpu_img: [Option<(Arc<fsr::ScreenImage>, u64, fsr::Modo)>; 2],
    img_ver: u64,
    held: HashSet<KeyCode>,
    cursor: Option<(f32, f32)>,
    mouse_down: bool,
    gil: Option<gilrs::Gilrs>,
    /// Start + Select do controle seguros juntos (atalho do menu): só dispara de novo depois de soltar
    combo_pad: bool,
    menu: Menu,
    info: menu::Info,
    fps: (Instant, u64, f32),
    next_frame: Instant,
    /// prazo do próximo passo da interpolação (a janela redesenha nele, qualquer que seja o teto de redesenhos)
    prazo: Option<Instant>,
    cad: [Cadencia; 2],
    /// interpolação efetivamente pedida ao renderizador (escolha do menu limitada à taxa do monitor)
    interp_aplicada: Option<u32>,
    aberta_em: Instant,
    crash: Option<String>,
    title: String,
}

pub fn run(cfg: Config) {
    let event_loop = match EventLoop::new() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[janela] não consegui criar o laço de eventos: {e}");
            return;
        }
    };
    let saved = cfg.env.to_save(&cfg.current, &cfg.file);
    let renderer_do_ambiente = cfg.env.renderer_set;
    let mut app = App {
        modos: Vec::new(),
        ultimos_swaps: 0,
        medida: (Instant::now(), 0, 0),
        envio: (0, Duration::ZERO),
        ritmo: (None, Vec::new()),
        cfg,
        saved,
        display: event_loop.owned_display_handle(),
        ctx: egui::Context::default(),
        gfx: None,
        failed: false,
        tex: [None, None],
        tex_state: [TexState::default(), TexState::default()],
        gpu_img: [None, None],
        img_ver: 0,
        held: HashSet::new(),
        cursor: None,
        mouse_down: false,
        gil: gilrs::Gilrs::new().map_err(|e| eprintln!("[janela] controles indisponíveis: {e}")).ok(),
        combo_pad: false,
        menu: Menu::default(),
        info: menu::Info { renderer_do_ambiente, ..menu::Info::default() },
        fps: (Instant::now(), 0, 0.0),
        next_frame: Instant::now(),
        prazo: None,
        cad: [Cadencia::default(), Cadencia::default()],
        interp_aplicada: None,
        aberta_em: Instant::now(),
        crash: None,
        title: String::new(),
    };
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("[janela] erro no laço de eventos: {e}");
    }
    super::set_paused(false);
    app.save();
}

/// Configuração do wgpu: Vulkan de preferência, DX12 de reserva (ou o que `WGPU_BACKEND` pedir).
fn wgpu_config(display: OwnedDisplayHandle) -> egui_wgpu::WgpuConfiguration {
    let mut setup = egui_wgpu::WgpuSetupCreateNew::from_display_handle(display);
    setup.instance_descriptor.backends =
        wgpu::Backends::from_env().unwrap_or(wgpu::Backends::VULKAN | wgpu::Backends::DX12);
    setup.native_adapter_selector = Some(Arc::new(|adapters: &[wgpu::Adapter], surface: Option<&wgpu::Surface<'_>>| {
        let rank = |a: &wgpu::Adapter| {
            let i = a.get_info();
            let b = match i.backend {
                wgpu::Backend::Vulkan => 0,
                wgpu::Backend::Dx12 => 1,
                _ => 2,
            };
            let t = match i.device_type {
                wgpu::DeviceType::DiscreteGpu => 0,
                wgpu::DeviceType::IntegratedGpu => 1,
                wgpu::DeviceType::VirtualGpu => 2,
                wgpu::DeviceType::Cpu => 4,
                wgpu::DeviceType::Other => 3,
            };
            (b, t)
        };
        adapters
            .iter()
            .filter(|a| surface.is_none_or(|s| a.is_surface_supported(s)))
            .min_by_key(|a| rank(a))
            .cloned()
            .ok_or_else(|| "nenhum adaptador Vulkan/DX12 compatível com a janela".to_string())
    }));
    egui_wgpu::WgpuConfiguration {
        surface: egui_wgpu::SurfaceConfig::LOW_LATENCY,
        wgpu_setup: setup.into(),
        ..Default::default()
    }
}

impl App {
    fn save(&mut self) {
        let s = self.cfg.env.to_save(&self.cfg.current, &self.cfg.file);
        if s != self.saved {
            settings::save(&s);
            self.cfg.file = s.clone();
            self.saved = s;
        }
    }

    /// Modo de apresentação efetivo para a escolha do menu, dado o que a GPU aceita.
    fn modo_efetivo(&self) -> wgpu::PresentMode {
        use wgpu::PresentMode as P;
        let tem = |m: P| self.modos.contains(&m);
        // YW_PRESENT=vsync|mailbox|imediato manda sobre o arquivo (testes)
        let escolha = match std::env::var("YW_PRESENT").ok().as_deref() {
            Some("vsync") => Apresentacao::Vsync,
            Some("mailbox") => Apresentacao::Caixa,
            Some("imediato") => Apresentacao::SemSincronia,
            _ => self.cfg.current.graficos.apresentacao,
        };
        match escolha {
            Apresentacao::Vsync => P::AutoVsync,
            Apresentacao::Caixa if tem(P::Mailbox) => P::Mailbox,
            Apresentacao::Caixa => P::AutoVsync,
            Apresentacao::SemSincronia if tem(P::Immediate) => P::Immediate,
            Apresentacao::SemSincronia => P::AutoNoVsync,
        }
    }

    /// Passa à superfície o modo e a profundidade da fila escolhidos no menu (o `Painter` só reconfigura se mudou).
    fn aplicar_apresentacao(&mut self) {
        let modo = self.modo_efetivo();
        let n = self.cfg.current.graficos.quadros_em_voo.clamp(1, 3);
        let Some(gfx) = self.gfx.as_mut() else { return };
        let novo = egui_wgpu::SurfaceConfig { present_mode: modo, desired_maximum_frame_latency: Some(n) };
        let texto = format!("{modo:?}, {n} quadro(s) em voo");
        if gfx.painter.surface_config() != novo {
            gfx.painter.set_surface_config(novo);
        }
        if self.info.modo_em_uso != texto {
            eprintln!("[janela] apresentação: {texto}");
            self.info.modo_em_uso = texto;
        }
    }

    /// Interpolação efetiva: a escolha do menu limitada à taxa do monitor em que a janela está (como o "igualar à taxa do
    /// monitor" do Ship of Harkinian e do RT64). Mais imagens que o monitor mostra só enchem a fila do V-Sync e atrasam
    /// ou desigualam os passos. `YW_INTERP` (ambiente, testes) manda sobre tudo.
    fn aplicar_interp(&mut self) {
        let sel = self.cfg.current.graficos.interpolacao;
        let monitor = self
            .gfx
            .as_ref()
            .and_then(|g| g.win.current_monitor())
            .and_then(|m| m.refresh_rate_millihertz())
            .map(|mhz| (mhz as f32 / 1000.0).round() as u32)
            .filter(|&hz| hz >= 40);
        let efetivo = match monitor {
            Some(hz) if sel > 0 => sel.min(hz),
            _ => sel,
        };
        self.info.interp = (efetivo, monitor.unwrap_or(0));
        if std::env::var_os("YW_INTERP").is_some() || self.interp_aplicada == Some(efetivo) {
            return;
        }
        crate::render::set_interp(efetivo);
        self.interp_aplicada = Some(efetivo);
        eprintln!("[janela] interpolação: {efetivo} Hz (escolhida {sel}, monitor {})", monitor.map_or("?".into(), |m| format!("{m} Hz")));
    }

    fn window_size(&self) -> PhysicalSize<u32> {
        let g = &self.cfg.current.graficos;
        let (w, h) = present::base_size(g.layout);
        PhysicalSize::new(w * g.escala_janela, h * g.escala_janela)
    }

    fn create(&mut self, el: &ActiveEventLoop) -> Result<(), String> {
        let attrs = Window::default_attributes()
            .with_title("Yo-kai Watch")
            .with_inner_size(self.window_size())
            .with_min_inner_size(PhysicalSize::new(160u32, 120u32))
            .with_fullscreen(self.cfg.current.graficos.tela_cheia.then_some(Fullscreen::Borderless(None)));
        let win = Arc::new(el.create_window(attrs).map_err(|e| e.to_string())?);
        let mut painter = pollster::block_on(Painter::new(
            self.ctx.clone(),
            wgpu_config(self.display.clone()),
            false,
            egui_wgpu::RendererOptions::default(),
        ));
        pollster::block_on(painter.set_window(egui::ViewportId::ROOT, Some(win.clone()))).map_err(|e| e.to_string())?;
        if let Some(rs) = painter.render_state() {
            let i = rs.adapter.get_info();
            self.info.apresentacao = format!("wgpu {:?} · {}", i.backend, i.name);
            eprintln!("[janela] {}", self.info.apresentacao);
            // modos de apresentação aceitos (uma superfície temporária só para perguntar à GPU)
            if let Ok(surf) = rs.instance.create_surface(win.clone()) {
                self.modos = surf.get_capabilities(&rs.adapter).present_modes;
            }
            eprintln!("[janela] modos de apresentação: {:?}", self.modos);
        }
        self.info.modos_presente = self.modos.iter().map(|m| format!("{m:?}")).collect();
        let egui = egui_winit::State::new(
            self.ctx.clone(),
            egui::ViewportId::ROOT,
            el,
            Some(win.scale_factor() as f32),
            None,
            painter.max_texture_side(),
        );
        self.gfx = Some(Gfx { win, painter, egui });
        self.aplicar_interp();
        Ok(())
    }

    fn toggle_menu(&mut self) {
        if self.crash.is_some() {
            return;
        }
        self.menu.toggle();
        if !self.menu.open {
            self.save();
        }
    }

    /// Lê teclado, controles e toque e entrega ao HID. Roda a cada acordada do laço (≈1 ms), não só nos redesenhos: com
    /// os redesenhos desligados a entrada continua com a mesma latência.
    fn entrada(&mut self) {
        let Some(size) = self.gfx.as_ref().map(|g| g.win.inner_size()) else { return };
        let g = &self.cfg.current.graficos;
        let (_, bottom) = present::place(g.layout, size.width as f32, size.height as f32, g.escala_inteira);
        let playing = !self.menu.open && self.crash.is_none();
        let mut sample = if playing { input::from_keys(&self.held, &self.cfg.current.teclado) } else { Sample::default() };
        self.poll_pads(playing.then_some(&mut sample));
        let playing = !self.menu.open && self.crash.is_none();
        if !playing {
            sample = Sample::default();
        } else if self.mouse_down {
            sample.touch = self.cursor.and_then(|(x, y)| present::touch_point(bottom, x, y));
        }
        let (b, stick, touch) = input::encode(&sample);
        // roteiro de entrada ativo (`window_run.py --input`): ele manda nos botões e no toque
        if super::script::ACTIVE.load(Ordering::Relaxed) {
            super::set_paused(self.menu.open);
            return;
        }
        BUTTONS.store(b, Ordering::Relaxed);
        STICK.store(stick, Ordering::Relaxed);
        TOUCH.store(touch, Ordering::Relaxed);
        super::set_paused(self.menu.open);
    }

    /// Lê os controles: eventos (captura no menu; Guide ou Start + Select = menu) e o estado atual somado em `s`.
    /// O Guide nem sempre chega (o backend WGI do gilrs no Windows não o mapeia), por isso o atalho Start + Select.
    fn poll_pads(&mut self, s: Option<&mut Sample>) {
        let Some(g) = self.gil.as_mut() else { return };
        let mut pressed = Vec::new();
        let mut changed = false;
        while let Some(ev) = g.next_event() {
            match ev.event {
                gilrs::EventType::ButtonPressed(b, _) => pressed.push(b),
                gilrs::EventType::Connected | gilrs::EventType::Disconnected => changed = true,
                _ => {}
            }
        }
        if changed || (self.menu.open && self.info.pads.is_empty() && g.gamepads().next().is_some()) {
            self.info.pads = g.gamepads().map(|(_, p)| p.name().to_string()).collect();
        }
        let c = &self.cfg.current.controle;
        let combo = g.gamepads().any(|(_, p)| p.is_pressed(c.start) && p.is_pressed(c.select));
        if let Some(s) = s {
            for (_, pad) in g.gamepads() {
                let axes = (pad.value(gilrs::Axis::LeftStickX), pad.value(gilrs::Axis::LeftStickY));
                input::add_pad(s, c, |b| pad.is_pressed(b), axes);
            }
        }
        let mut toggle = false;
        for b in pressed {
            if self.menu.capture_pad(&mut self.cfg.current, b) {
                continue;
            }
            toggle |= b == gilrs::Button::Mode;
        }
        // Start + Select juntos (fora de uma captura): borda de subida
        if combo && !self.combo_pad && self.menu.captura.is_none() {
            toggle = true;
        }
        self.combo_pad = combo;
        if toggle {
            self.toggle_menu();
        }
    }

    /// `alvos`: retângulo (em pixels da janela) de cada tela, para o filtro "Misto" escolher o fator inteiro e para o
    /// FSR 1 saber se está ampliando.
    fn update_textures(&mut self, alvos: [Option<present::Rect>; 2]) {
        let t0 = Instant::now();
        self.montar_texturas(alvos);
        self.envio.1 += t0.elapsed();
    }

    fn montar_texturas(&mut self, alvos: [Option<present::Rect>; 2]) {
        let filtro = self.cfg.current.graficos.filtro;
        // YW_FSR=1/0 manda sobre o arquivo (testes)
        let fsr_on = match std::env::var("YW_FSR").ok().as_deref() {
            Some("0") => false,
            Some(_) => true,
            None => self.cfg.current.graficos.fsr,
        };
        let opts = match filtro {
            Filtro::Nitido => egui::TextureOptions::NEAREST,
            Filtro::Suave | Filtro::Misto => egui::TextureOptions::LINEAR,
        };
        let now = Instant::now();
        self.prazo = None;
        let gpu_ok = self.gfx.as_ref().is_some_and(|g| g.painter.render_state().is_some());
        for s in 0..2 {
            let swaps = super::services::gsp::SWAPS[s].load(Ordering::Relaxed);
            let st = self.tex_state[s];
            // imagem em alta resolução (resolução interna / FXAA) do framebuffer que o jogo mostra, se válida; com
            // interpolação, o passo do grupo que a cadência manda mostrar agora
            let hi = match gpu::screen_hires(s) {
                Some(sh) => match &sh.group {
                    Some(g) => {
                        let (img, prazo) = self.cad[s].escolher(g, now);
                        if let Some(p) = prazo {
                            self.prazo = Some(self.prazo.map_or(p, |q| q.min(p)));
                        }
                        Some(img)
                    }
                    None => {
                        self.cad[s].encerrar();
                        Some(sh.real)
                    }
                },
                // A troca de framebuffer (thread do VBlank) pode chegar enquanto a thread da GPU ainda monta a imagem
                // em alta resolução (e os intermediários) desse quadro: a memória nativa já tem o quadro N, a imagem
                // publicada ainda é a antiga, o hash não bate. Mostrar a nativa ali seria mostrar N e logo depois os
                // intermediários entre N−1 e N — a imagem voltando. Então a tela segura a imagem que já está nela por
                // até 60 ms, tentando de novo a cada 2 ms; passado isso (o jogo pôs outra coisa no framebuffer), nativa.
                None if crate::render::hires_on() && self.cad[s].ultimo_hi.is_some_and(|t| now.duration_since(t) < Duration::from_millis(60)) => {
                    self.cad[s].segurou += 1;
                    let p = now + Duration::from_millis(2);
                    self.prazo = Some(self.prazo.map_or(p, |q| q.min(p)));
                    continue;
                }
                None => {
                    if crate::render::hires_on() && self.cad[s].ultimo_hi.is_some() {
                        self.cad[s].nativa += 1;
                        self.cad[s].ultimo_hi = None;
                    }
                    self.cad[s].encerrar();
                    None
                }
            };
            if hi.is_some() {
                self.cad[s].ultimo_hi = Some(now);
            }
            let hi_id = hi.as_ref().map_or(0, |h| Arc::as_ptr(h) as usize);
            let base_w = hi.as_ref().map_or(if s == 0 { 400.0 } else { 320.0 }, |h| h.w as f32);
            // o FSR só entra quando a janela é maior que a imagem (ele amplia; reduzir fica com o filtro comum)
            let use_fsr = fsr_on && alvos[s].is_some_and(|r| r.w > base_w * 1.01);
            // fator inteiro do vizinho mais próximo (só no filtro Misto): piso da escala até o tamanho da janela
            let n = match (filtro, alvos[s]) {
                (Filtro::Misto, Some(r)) if !use_fsr => ((r.w / base_w).floor() as usize).clamp(1, 8),
                _ => 1,
            };
            // tela nova, filtro/fator trocado, ou de tempos em tempos (o jogo pode trocar o framebuffer sem "swap")
            let stale = st.at.is_none_or(|t| now.duration_since(t) > Duration::from_millis(250));
            if swaps == st.swaps && filtro == st.filtro && n == st.n && use_fsr == st.fsr && hi_id == st.hi && !stale {
                continue;
            }
            // imagem nova de verdade (não só reenvio por filtro trocado ou pelos 250 ms): conta o intervalo
            let nova = if hi.is_some() { hi_id != st.hi } else { swaps != st.swaps };
            if s == 0 && nova {
                if let Some(t) = self.ritmo.0 {
                    self.ritmo.1.push(now.duration_since(t).as_secs_f32() * 1000.0);
                }
                self.ritmo.0 = Some(now);
            }
            self.tex_state[s] = TexState { swaps, filtro, n, fsr: use_fsr, hi: hi_id, at: Some(now) };
            self.envio.0 += 1;
            let (w, h, rgba) = match &hi {
                Some(h) => (h.w, h.h, fsr::Bytes::Hi(h.clone())),
                None => match gpu::screen_rgba(s) {
                    Some((w, h, v)) => (w, h, fsr::Bytes::Own(v)),
                    None => continue,
                },
            };
            if s == 0 && nova {
                dump_tela(if hi.is_some() { 'H' } else { 'N' }, w, h, &rgba);
            }
            if gpu_ok {
                // pela GPU da janela (FSR ou o filtro no shader): sem cópia nem conversão aqui; a imagem em alta
                // resolução vai compartilhada
                self.img_ver += 1;
                let modo = if use_fsr {
                    fsr::Modo::Fsr
                } else {
                    let f = match filtro {
                        Filtro::Nitido => 0,
                        Filtro::Suave => 1,
                        Filtro::Misto => 2,
                    };
                    fsr::Modo::Direto { filtro: f, n: n as u32 }
                };
                self.gpu_img[s] = Some((Arc::new(fsr::ScreenImage { w, h, rgba }), self.img_ver, modo));
                continue;
            }
            // reserva (sem estado do wgpu): textura do egui
            self.gpu_img[s] = None;
            let px = rgba.to_vec();
            let (w, h, px) = if n > 1 { ampliar(w as usize, h as usize, &px, n) } else { (w as usize, h as usize, px) };
            let img = egui::ColorImage::from_rgba_unmultiplied([w, h], &px);
            match &mut self.tex[s] {
                Some(t) => t.set(img, opts),
                None => self.tex[s] = Some(self.ctx.load_texture(if s == 0 { "tela_cima" } else { "tela_baixo" }, img, opts)),
            }
        }
    }

    fn frame(&mut self, el: &ActiveEventLoop) {
        if self.crash.is_none() {
            if let Some(m) = crash_message() {
                self.crash = Some(m);
                self.menu.open = false;
                self.menu.captura = None;
            }
        }
        let Some(size) = self.gfx.as_ref().map(|g| g.win.inner_size()) else {
            return;
        };
        let before = self.cfg.current.clone();
        let g = self.cfg.current.graficos.clone();
        let (top, bottom) = present::place(g.layout, size.width as f32, size.height as f32, g.escala_inteira);

        // entrada do jogo (nada com o menu aberto ou depois de parar); também lida a cada acordada do laço
        self.entrada();
        if self.menu.open {
            // o jogo já desenhou antes do menu abrir, então o backend já foi escolhido (não inicia nada aqui)
            self.info.renderer_em_uso = Some(crate::render::backend());
        }

        self.update_textures([top, bottom]);
        // a cada 5 s: redesenhos da janela e quadros de imagem nova do jogo por segundo (vai para o log)
        self.medida.1 += 1;
        let dt = self.medida.0.elapsed().as_secs_f64();
        if dt >= 5.0 {
            let sw = super::services::gsp::SWAPS[0].load(Ordering::Relaxed);
            let interp = self.cad[0].resumo(dt).map_or(String::new(), |r| format!(" · {r}"));
            let _ = self.cad[1].resumo(dt);
            let (n, t) = self.envio;
            eprintln!(
                "[janela] {:.0} redesenhos/s · {:.1} quadros do jogo/s ({}) · telas enviadas {:.0}/s, {:.1} ms/s ({:.2} ms cada){interp}",
                self.medida.1 as f64 / dt,
                sw.wrapping_sub(self.medida.2) as f64 / dt,
                self.info.modo_em_uso,
                n as f64 / dt,
                t.as_secs_f64() * 1000.0 / dt,
                if n > 0 { t.as_secs_f64() * 1000.0 / n as f64 } else { 0.0 }
            );
            self.envio = (0, Duration::ZERO);
            // intervalos entre imagens novas da tela de cima, em faixas de VBlank (16,7 ms): a 30 quadros/s regulares
            // quase todos caem em "≈33"; "≈50" e "≈17" misturados com "≈33" são o tropeço que se vê ao andar
            let vb: Vec<u32> = super::services::gsp::SWAP_VB.iter().map(|c| c.swap(0, Ordering::Relaxed)).collect();
            let tot = vb.iter().sum::<u32>().max(1) as f32;
            let adiadas = super::services::gsp::SWAPS_ADIADAS.swap(0, Ordering::Relaxed);
            if vb.iter().sum::<u32>() >= 10 {
                eprintln!(
                    "[janela] ritmo do jogo (VBlanks entre trocas da tela de cima): 1 → {:.0}% · 2 → {:.0}% · 3 → {:.0}% · 4+ → {:.0}% · trocas adiadas (buffer ainda na fila da GPU) {adiadas}",
                    100.0 * vb[0] as f32 / tot,
                    100.0 * vb[1] as f32 / tot,
                    100.0 * vb[2] as f32 / tot,
                    100.0 * vb[3] as f32 / tot
                );
            }
            let iv = std::mem::take(&mut self.ritmo.1);
            if iv.len() >= 10 {
                let n = iv.len() as f32;
                let faixa = |lo: f32, hi: f32| 100.0 * iv.iter().filter(|&&v| v >= lo && v < hi).count() as f32 / n;
                let media = iv.iter().sum::<f32>() / n;
                let desvio = (iv.iter().map(|v| (v - media).powi(2)).sum::<f32>() / n).sqrt();
                eprintln!(
                    "[janela] ritmo da tela de cima: {} imagens novas, intervalo médio {media:.1} ms (desvio {desvio:.1}) · <10 ms {:.0}% · ≈17 {:.0}% · ≈33 {:.0}% · ≈50 {:.0}% · maior {:.0}%",
                    iv.len(),
                    faixa(0.0, 10.0),
                    faixa(10.0, 25.0),
                    faixa(25.0, 42.0),
                    faixa(42.0, 59.0),
                    faixa(59.0, f32::MAX)
                );
            }
            self.medida = (Instant::now(), 0, sw);
        }
        let now = Instant::now();
        let dt = now.duration_since(self.fps.0).as_secs_f32();
        if dt >= 0.5 {
            let sw = super::services::gsp::SWAPS[0].load(Ordering::Relaxed);
            self.fps = (now, sw, (sw.wrapping_sub(self.fps.1)) as f32 / dt);
        }

        let gfx = self.gfx.as_mut().unwrap();
        let raw = gfx.egui.take_egui_input(&gfx.win);
        let mut acao = Acao::Nada;
        let mut sair_crash = false;
        let (menu, cfg, info, tex, crash, fps) =
            (&mut self.menu, &mut self.cfg, &self.info, &self.tex, &self.crash, self.fps.2);
        let gpu_img = self.gpu_img.clone();
        let fsr_format = gfx.painter.render_state().map(|rs| rs.target_format);
        // nitidez 0-100 % -> fator do RCAS 2^(-paradas): 100 % = 1,0 (maxima), 0 % = 0,25
        let fsr_sharp = (-(1.0 - cfg.current.graficos.fsr_nitidez as f32 / 100.0) * 2.0).exp2();
        let out = self.ctx.run_pass(raw, |ctx| {
            let ppp = ctx.pixels_per_point();
            let p = ctx.layer_painter(egui::LayerId::background());
            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            for (i, (r, t)) in [(top, &tex[0]), (bottom, &tex[1])].into_iter().enumerate() {
                let Some(r) = r else { continue };
                let rect = egui::Rect::from_min_size(egui::pos2(r.x / ppp, r.y / ppp), egui::vec2(r.w / ppp, r.h / ppp));
                // a tela vai por um callback da GPU (FSR 1: EASU + RCAS; ou o filtro direto no shader)
                if let (Some((img, ver, modo)), Some(format)) = (&gpu_img[i], fsr_format) {
                    let dst = [r.w.round().max(1.0) as u32, r.h.round().max(1.0) as u32];
                    let cb = match *modo {
                        fsr::Modo::Fsr => egui_wgpu::Callback::new_paint_callback(
                            rect,
                            fsr::Fsr { id: i as u8, img: img.clone(), version: *ver, dst, sharp: fsr_sharp, format },
                        ),
                        fsr::Modo::Direto { filtro, n } => egui_wgpu::Callback::new_paint_callback(
                            rect,
                            fsr::Direta { id: i as u8, img: img.clone(), version: *ver, filtro, n, format },
                        ),
                    };
                    p.add(egui::Shape::Callback(cb));
                } else if let Some(t) = t {
                    p.image(t.id(), rect, uv, egui::Color32::WHITE);
                }
            }
            if menu.open || crash.is_some() {
                p.rect_filled(ctx.content_rect(), 0.0, egui::Color32::from_black_alpha(150));
            }
            if let Some(m) = crash {
                sair_crash = menu::crash_window(ctx, m);
            } else {
                acao = menu.ui(ctx, &mut cfg.current, info);
            }
            if cfg.current.graficos.mostrar_fps && crash.is_none() {
                menu::fps_overlay(ctx, fps);
            }
        });
        gfx.egui.handle_platform_output(&gfx.win, out.platform_output);
        let prims = self.ctx.tessellate(out.shapes, out.pixels_per_point);
        let mut delta = out.textures_delta;
        gfx.painter.paint_and_update_textures(
            egui::ViewportId::ROOT,
            out.pixels_per_point,
            [0.0, 0.0, 0.0, 1.0],
            &prims,
            &mut delta,
            Vec::new(),
            &gfx.win,
        );

        // título
        let title = match (&self.crash, self.menu.open) {
            (Some(m), _) => format!("Yo-kai Watch — PAROU: {}", m.lines().next().unwrap_or("")),
            (None, true) => "Yo-kai Watch — pausado".to_string(),
            (None, false) => "Yo-kai Watch".to_string(),
        };
        if title != self.title {
            gfx.win.set_title(&title);
            self.title = title;
        }

        // efeitos das mudanças feitas no menu
        let cur = &self.cfg.current;
        let cheia = gfx.win.fullscreen().is_some();
        if cur.graficos.tela_cheia != cheia {
            gfx.win.set_fullscreen(cur.graficos.tela_cheia.then_some(Fullscreen::Borderless(None)));
        }
        if (cur.graficos.layout != before.graficos.layout || cur.graficos.escala_janela != before.graficos.escala_janela)
            && !gfx.win.is_maximized()
            && !cheia
            && !cur.graficos.tela_cheia
        {
            let (w, h) = present::base_size(cur.graficos.layout);
            let k = cur.graficos.escala_janela;
            let _ = gfx.win.request_inner_size(PhysicalSize::new(w * k, h * k));
        }
        self.aplicar_apresentacao();
        let cur = &self.cfg.current;
        if cur.graficos.anisotropico != before.graficos.anisotropico && std::env::var_os("YW_ANISO").is_none() {
            crate::render::set_aniso(cur.graficos.anisotropico);
        }
        if cur.graficos.escala_interna != before.graficos.escala_interna && std::env::var_os("YW_SCALE").is_none() {
            crate::render::set_scale(cur.graficos.escala_interna);
        }
        if cur.graficos.fxaa != before.graficos.fxaa && std::env::var_os("YW_FXAA").is_none() {
            crate::render::set_fxaa(cur.graficos.fxaa);
        }
        if cur.som != before.som {
            audio::set_volume(cur.som.volume);
            audio::set_muted(cur.som.mudo);
        }
        if self.cfg.current.graficos.interpolacao != before.graficos.interpolacao {
            self.aplicar_interp();
        }
        // grava quando algo mudou (inclui teclas remapeadas fora deste quadro); `save` só escreve se mudou
        if self.menu.open || acao != Acao::Nada || self.cfg.current != before {
            self.save();
        }
        if acao == Acao::Sair || sair_crash {
            el.exit();
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.gfx.is_some() || self.failed {
            return;
        }
        if let Err(e) = self.create(el) {
            eprintln!("[janela] não consegui abrir a janela com wgpu (Vulkan/DX12): {e}");
            self.failed = true;
            el.exit();
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(gfx) = self.gfx.as_mut() else { return };
        // esperando uma tecla nova no remapeamento: a tecla vai SÓ para a captura (o egui não pode tratá-la como
        // clique no botão focado nem como Tab/setas de navegação). A soltura segue o caminho normal.
        if let WindowEvent::KeyboardInput { event: ke, .. } = &event {
            if let (PhysicalKey::Code(code), ElementState::Pressed, false) = (ke.physical_key, ke.state, ke.repeat) {
                if self.menu.capture_key(&mut self.cfg.current, code) {
                    return;
                }
            }
        }
        let _ = gfx.egui.on_window_event(&gfx.win, &event);
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Resized(sz) => {
                if let (Some(w), Some(h)) = (NonZeroU32::new(sz.width), NonZeroU32::new(sz.height)) {
                    gfx.painter.on_window_resized(egui::ViewportId::ROOT, w, h);
                }
                gfx.win.request_redraw();
            }
            // outro monitor pode ter outra taxa (teto da interpolação)
            WindowEvent::Moved(_) | WindowEvent::ScaleFactorChanged { .. } => self.aplicar_interp(),
            WindowEvent::KeyboardInput { event, .. } => {
                let PhysicalKey::Code(code) = event.physical_key else { return };
                if event.state == ElementState::Pressed {
                    if !event.repeat {
                        if matches!(code, KeyCode::Escape | KeyCode::F1) {
                            self.toggle_menu();
                        }
                        if code == KeyCode::F11 {
                            let g = &mut self.cfg.current.graficos;
                            g.tela_cheia = !g.tela_cheia;
                            self.save();
                        }
                    }
                    self.held.insert(code);
                } else {
                    self.held.remove(&code);
                }
            }
            WindowEvent::Focused(false) => {
                self.held.clear();
                self.mouse_down = false;
            }
            WindowEvent::CursorMoved { position, .. } => self.cursor = Some((position.x as f32, position.y as f32)),
            WindowEvent::CursorLeft { .. } => self.cursor = None,
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                self.mouse_down = state == ElementState::Pressed;
            }
            WindowEvent::RedrawRequested => self.frame(el),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        if self.gfx.is_none() {
            return;
        }
        let now = Instant::now();
        // testes com janela: `YW_WINDOW_SECS=N` fecha sozinho depois de N segundos (grava as configurações como sempre)
        static SECS: std::sync::OnceLock<Option<f64>> = std::sync::OnceLock::new();
        if let Some(lim) = *SECS.get_or_init(|| std::env::var("YW_WINDOW_SECS").ok().and_then(|v| v.parse().ok())) {
            if now.duration_since(self.aberta_em).as_secs_f64() >= lim {
                el.exit();
                return;
            }
        }
        let menu_antes = self.menu.open;
        self.entrada();
        let Some(gfx) = self.gfx.as_ref() else { return };
        let limite = self.cfg.current.graficos.limite_fps;
        let intervalo = limite.intervalo();
        // redesenhos sem imagem nova: pelo teto escolhido; "desligados" = nenhum (menos com o menu ou o aviso de parada
        // abertos, que precisam reagir ao mouse)
        let extras = limite != LimiteFps::Desligado || self.menu.open || self.crash.is_some();
        let minimized = gfx.win.is_minimized().unwrap_or(false) || gfx.win.inner_size().width == 0;
        // imagem nova: quadro novo do jogo (mostra já, sem esperar o próximo tique do teto — é isso que faz o Mailbox/
        // Imediato reduzirem a latência) ou o prazo do próximo passo da interpolação
        let swaps = super::services::gsp::SWAPS[0].load(Ordering::Relaxed)
            + super::services::gsp::SWAPS[1].load(Ordering::Relaxed);
        let novo = swaps != self.ultimos_swaps || self.prazo.is_some_and(|p| now >= p) || self.menu.open != menu_antes;
        if !minimized && novo {
            self.ultimos_swaps = swaps;
            self.prazo = None;
            self.next_frame = now + intervalo;
            gfx.win.request_redraw();
        } else if !minimized && extras && now >= self.next_frame {
            self.next_frame = now + intervalo;
            gfx.win.request_redraw();
        }
        // acorda a cada 1 ms: entrada, quadro novo do jogo e prazos da interpolação
        el.set_control_flow(ControlFlow::WaitUntil(now + Duration::from_millis(1)));
    }
}

#[cfg(test)]
mod testes_filtro {
    use super::ampliar;

    #[test]
    fn ampliar_repete_cada_pixel_n_por_n() {
        // 2×1: vermelho, azul → fator 3 = 6×3
        let px = [255, 0, 0, 255, 0, 0, 255, 255];
        let (w, h, o) = ampliar(2, 1, &px, 3);
        assert_eq!((w, h, o.len()), (6, 3, 6 * 3 * 4));
        for y in 0..3 {
            for x in 0..6 {
                let p = &o[(y * 6 + x) * 4..(y * 6 + x) * 4 + 4];
                assert_eq!(p, if x < 3 { &px[0..4] } else { &px[4..8] }, "({x},{y})");
            }
        }
    }
}

/// Diagnóstico (`YW_WIN_DUMP=pasta`, `YW_WIN_DUMP_FROM`/`YW_WIN_DUMP_TO` = trecho em s do console): grava cada imagem
/// nova da tela de cima exatamente como a janela a recebe (antes do FSR/filtro), em ordem, numa thread à parte.
/// Nome: `NNNNN_TIPO_tCCCCC_WxH.rgba` (H = alta resolução/intermediário, N = nativa; CCCCC = centésimos do console).
fn dump_tela(tipo: char, w: u32, h: u32, px: &[u8]) {
    use std::sync::mpsc::{channel, Sender};
    use std::sync::{Mutex, OnceLock};
    type Msg = (char, u32, u32, u32, Vec<u8>);
    static D: OnceLock<Option<(Mutex<Sender<Msg>>, f64, f64)>> = OnceLock::new();
    let d = D.get_or_init(|| {
        let dir = std::path::PathBuf::from(std::env::var_os("YW_WIN_DUMP")?);
        let _ = std::fs::create_dir_all(&dir);
        let f = |k: &str, v: f64| std::env::var(k).ok().and_then(|s| s.parse().ok()).unwrap_or(v);
        let (tx, rx) = channel::<Msg>();
        std::thread::Builder::new()
            .name("win-dump".into())
            .spawn(move || {
                for (n, (tipo, t, w, h, px)) in rx.into_iter().enumerate() {
                    let _ = std::fs::write(dir.join(format!("{n:05}_{tipo}_t{t:05}_{w}x{h}.rgba")), px);
                }
            })
            .ok()?;
        Some((Mutex::new(tx), f("YW_WIN_DUMP_FROM", 0.0), f("YW_WIN_DUMP_TO", 1e9)))
    });
    let Some((tx, from, to)) = d else { return };
    let t = super::VBLANKS.load(Ordering::Relaxed) as f64 / super::vblank_hz();
    if t >= *from && t < *to {
        let _ = tx.lock().unwrap().send((tipo, (t * 100.0) as u32, w, h, px.to_vec()));
    }
}
