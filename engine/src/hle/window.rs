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
use super::present;
use super::services::dsp::output as audio;
use super::services::hid::{BUTTONS, STICK, TOUCH};
use super::settings::{self, EnvOverrides, Filtro, Renderizador, Settings};
use super::{crash_message, gpu};
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

/// Intervalo mínimo entre quadros da janela (o vsync normalmente já segura antes disso).
const FRAME: Duration = Duration::from_micros(8_333);

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
        Config { file, env, current }
    }
}

struct Gfx {
    win: Arc<Window>,
    painter: Painter,
    egui: egui_winit::State,
}

struct App {
    cfg: Config,
    /// último conteúdo gravado no arquivo
    saved: Settings,
    display: OwnedDisplayHandle,
    ctx: egui::Context,
    gfx: Option<Gfx>,
    failed: bool,
    tex: [Option<egui::TextureHandle>; 2],
    tex_state: [(u64, Filtro, Option<Instant>); 2],
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
        cfg,
        saved,
        display: event_loop.owned_display_handle(),
        ctx: egui::Context::default(),
        gfx: None,
        failed: false,
        tex: [None, None],
        tex_state: [(u64::MAX, Filtro::Nitido, None); 2],
        held: HashSet::new(),
        cursor: None,
        mouse_down: false,
        gil: gilrs::Gilrs::new().map_err(|e| eprintln!("[janela] controles indisponíveis: {e}")).ok(),
        combo_pad: false,
        menu: Menu::default(),
        info: menu::Info { renderer_do_ambiente, ..menu::Info::default() },
        fps: (Instant::now(), 0, 0.0),
        next_frame: Instant::now(),
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
        }
        let egui = egui_winit::State::new(
            self.ctx.clone(),
            egui::ViewportId::ROOT,
            el,
            Some(win.scale_factor() as f32),
            None,
            painter.max_texture_side(),
        );
        self.gfx = Some(Gfx { win, painter, egui });
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

    fn update_textures(&mut self) {
        let filtro = self.cfg.current.graficos.filtro;
        let opts = match filtro {
            Filtro::Nitido => egui::TextureOptions::NEAREST,
            Filtro::Suave => egui::TextureOptions::LINEAR,
        };
        let now = Instant::now();
        for s in 0..2 {
            let swaps = super::services::gsp::SWAPS[s].load(Ordering::Relaxed);
            let (last, f, at) = self.tex_state[s];
            // tela nova, filtro trocado, ou de tempos em tempos (o jogo pode trocar o framebuffer sem "swap")
            let stale = at.is_none_or(|t| now.duration_since(t) > Duration::from_millis(250));
            if swaps == last && f == filtro && !stale {
                continue;
            }
            self.tex_state[s] = (swaps, filtro, Some(now));
            let Some((w, h, px)) = gpu::screen_rgba(s) else { continue };
            let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &px);
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

        // entrada do jogo (nada com o menu aberto ou depois de parar)
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
        BUTTONS.store(b, Ordering::Relaxed);
        STICK.store(stick, Ordering::Relaxed);
        TOUCH.store(touch, Ordering::Relaxed);
        super::set_paused(self.menu.open);
        if self.menu.open {
            // o jogo já desenhou antes do menu abrir, então o backend já foi escolhido (não inicia nada aqui)
            self.info.renderer_em_uso = Some(crate::render::backend());
        }

        self.update_textures();
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
        let out = self.ctx.run_pass(raw, |ctx| {
            let ppp = ctx.pixels_per_point();
            let p = ctx.layer_painter(egui::LayerId::background());
            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            for (r, t) in [(top, &tex[0]), (bottom, &tex[1])] {
                if let (Some(r), Some(t)) = (r, t) {
                    let rect = egui::Rect::from_min_size(egui::pos2(r.x / ppp, r.y / ppp), egui::vec2(r.w / ppp, r.h / ppp));
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
        if cur.som != before.som {
            audio::set_volume(cur.som.volume);
            audio::set_muted(cur.som.mudo);
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
            }
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
        let Some(gfx) = self.gfx.as_ref() else { return };
        let now = Instant::now();
        if now >= self.next_frame {
            // janela minimizada: sem quadros, mas continua lendo a entrada devagar
            let minimized = gfx.win.is_minimized().unwrap_or(false) || gfx.win.inner_size().width == 0;
            self.next_frame = now + if minimized { Duration::from_millis(50) } else { FRAME };
            gfx.win.request_redraw();
        }
        el.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
    }
}
