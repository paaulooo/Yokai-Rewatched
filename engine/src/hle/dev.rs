//! Ferramentas de desenvolvedor (G6): janelas egui sobre o jogo, abertas com F3 quando ligadas (`dev.ferramentas` no
//! settings.toml ou `YW_DEV=1`; `YW_DEV=0` desliga mesmo com o arquivo ligado). Não pausam o jogo.
//!
//! - **Ganchos**: `hooks::resumo` (registrados, ativos, chamadas), relido a cada 0,5 s.
//! - **Memória**: endereço → hex (256 bytes, só páginas mapeadas) e valores vigiados (`dev.vigiados`, "0xEND:tipo").
//! - **Arquivos**: aberturas pelo caminho (`FUN_0047359c`, `FUN_0047365c` com descompressão, `FUN_00473728`
//!   "existe?"), por ganchos "antes" só de leitura (convivem com a substituição dos mods, `mods::arquivos`).
//! - **Câmera**: último envio de cada câmera ao renderizador (`FUN_00325c18(_, câmera, olho)`, mesma leitura de
//!   `etiquetas::camera`: view 3×4 em câmera+0x44/+0x130/+0x21c, olho = −Σ tₖ·eixoₖ [H: convenção]) e os cortes que o
//!   G1 detectou (`etiquetas::N_CORTES_CAM`).
//! - **Desempenho**: quadros, draws por quadro, VBlanks, turbo, Vulkan (pipelines, VRAM, texturas).
//!
//! Os ganchos são registrados na primeira vez que as ferramentas ficam ligadas (no boot, se já estavam), e ficam.
//! Sem janela, `YW_DEV=1` registra os mesmos ganchos e o fim da rodada imprime um resumo `[dev]` (`log_resumo`).

use super::hooks::{self, Tipo};
use super::mods::arquivos;
use super::settings::Settings;
use egui::{Align2, RichText, Vec2};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering, Ordering::Relaxed};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// `YW_DEV` (1/0) vence o arquivo; sem ela, `dev.ferramentas`.
pub fn ligado(s: &Settings) -> bool {
    match std::env::var("YW_DEV").ok().as_deref() {
        Some("0") => false,
        Some(_) => true,
        None => s.dev.ferramentas,
    }
}

/// Uma abertura de arquivo vista pelos ganchos.
#[derive(Clone, Debug)]
pub struct Abertura {
    pub q: u64,
    pub caminho: String,
    /// "abrir", "abrir+descomprimir", "existe?"
    pub tipo: &'static str,
}

#[derive(Default)]
struct Arquivos {
    recentes: VecDeque<Abertura>,
    contagem: HashMap<String, u32>,
    total: u64,
}

/// Último envio de uma câmera.
#[derive(Clone, Copy, Debug, Default)]
pub struct Cam {
    pub cam: u32,
    /// deslocamento da view usado (0x44 modo 0/1, 0x130 modo 2, 0x21c modo 3)
    pub off: u32,
    pub q: u64,
    pub olho: [f32; 3],
    pub eixos: [f32; 9],
    pub envios: u64,
}

static ARQ: Mutex<Option<Arquivos>> = Mutex::new(None);
static CAMS: Mutex<Vec<Cam>> = Mutex::new(Vec::new());
static INSTALADO: AtomicBool = AtomicBool::new(false);
/// Ligadas desde o início (janela: `window::Config::load`; sem janela: `hle::boot`): o boot instala os ganchos.
pub static PEDIDO: AtomicBool = AtomicBool::new(false);
const MAX_RECENTES: usize = 300;

fn arquivo(tipo: &'static str, c: &yw1_rt::Cpu) {
    let Some(caminho) = arquivos::ler_cstr(c.r[1], 512) else { return };
    let q = super::VBLANKS.load(Relaxed);
    let mut g = ARQ.lock().unwrap_or_else(|e| e.into_inner());
    let a = g.get_or_insert_with(Arquivos::default);
    a.total += 1;
    *a.contagem.entry(caminho.clone()).or_insert(0) += 1;
    if a.recentes.len() >= MAX_RECENTES {
        a.recentes.pop_front();
    }
    a.recentes.push_back(Abertura { q, caminho, tipo });
}

/// Endereço do envio de câmera ao renderizador (ver `etiquetas.rs`).
const ENVIO_CAMERA: u32 = 0x325c18;

fn camera(c: &yw1_rt::Cpu) {
    let cam = c.r[1];
    if cam == 0 || !super::mem::is_mapped(cam) || !super::mem::is_mapped(cam + 0x2d0) || yw1_rt::r8(cam + 0x2d0) > 1 {
        return;
    }
    let off = match c.r[2] {
        2 => 0x130,
        3 => 0x21c,
        _ => 0x44,
    };
    let v = cam + off;
    if !super::mem::is_mapped(v) || !super::mem::is_mapped(v + 47) {
        return;
    }
    let f = |i: u32| f32::from_bits(yw1_rt::r32(v + 4 * i));
    let eixos: [f32; 9] = std::array::from_fn(|i| f(i as u32));
    let t = [f(9), f(10), f(11)];
    let olho: [f32; 3] = std::array::from_fn(|k| -(t[0] * eixos[k] + t[1] * eixos[3 + k] + t[2] * eixos[6 + k]));
    let q = super::VBLANKS.load(Relaxed);
    let mut g = CAMS.lock().unwrap_or_else(|e| e.into_inner());
    match g.iter_mut().find(|x| x.cam == cam && x.off == off) {
        Some(x) => {
            x.envios += 1;
            (x.q, x.olho, x.eixos) = (q, olho, eixos);
        }
        None => {
            if g.len() >= 8 {
                // a mais antiga sai (câmeras de cenas que já acabaram)
                if let Some(i) = g.iter().enumerate().min_by_key(|(_, x)| x.q).map(|(i, _)| i) {
                    g.remove(i);
                }
            }
            g.push(Cam { cam, off, q, olho, eixos, envios: 1 });
        }
    }
}

/// Registra os ganchos de leitura (uma vez). Seguro com o jogo rodando: o registro liga o byte da função depois de a
/// lista estar no lugar (`hooks::registrar`).
pub fn instalar() {
    if INSTALADO.swap(true, Ordering::SeqCst) {
        return;
    }
    let r = [
        hooks::registrar(arquivos::ABRIR, Tipo::antes(|c| arquivo("abrir", c)), "dev: arquivo aberto", 100, false),
        hooks::registrar(
            arquivos::ABRIR_DESCOMPRIMIR,
            Tipo::antes(|c| arquivo("abrir+descomprimir", c)),
            "dev: arquivo aberto (descomprime)",
            100,
            false,
        ),
        hooks::registrar(arquivos::EXISTE, Tipo::antes(|c| arquivo("existe?", c)), "dev: arquivo existe?", 100, false),
        hooks::registrar(ENVIO_CAMERA, Tipo::antes(|c| camera(c)), "dev: câmera (FUN_00325c18)", 100, false),
    ];
    let erros: Vec<String> = r.into_iter().filter_map(Result::err).collect();
    for e in &erros {
        eprintln!("[dev] {e}");
    }
    eprintln!("[dev] ferramentas de desenvolvedor ligadas (F3 na janela){}", if erros.is_empty() { "" } else { " — com erros" });
}

/// Fim da rodada (com as ferramentas ligadas): resumo das aberturas e câmeras.
pub fn log_resumo() {
    if !INSTALADO.load(Relaxed) {
        return;
    }
    let g = ARQ.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(a) = g.as_ref() {
        let mut v: Vec<(&String, &u32)> = a.contagem.iter().collect();
        v.sort_by(|x, y| y.1.cmp(x.1).then(x.0.cmp(y.0)));
        eprintln!(
            "[dev] arquivos: {} aberturas, {} caminhos distintos; mais abertos: {}",
            a.total,
            v.len(),
            v.iter().take(8).map(|(c, n)| format!("{c} ×{n}")).collect::<Vec<_>>().join(", ")
        );
    }
    for c in CAMS.lock().unwrap_or_else(|e| e.into_inner()).iter() {
        eprintln!(
            "[dev] câmera {:#x}+{:#x}: {} envios, último q={} olho ({:.1}, {:.1}, {:.1})",
            c.cam, c.off, c.envios, c.q, c.olho[0], c.olho[1], c.olho[2]
        );
    }
}

/// Tipo de um valor vigiado.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tipo32 {
    U8,
    U16,
    U32,
    I32,
    F32,
}

/// "0x5fe028:u32" → (endereço, tipo). Tipo ausente = u32.
pub fn parse_vigiado(s: &str) -> Option<(u32, Tipo32)> {
    let (a, t) = s.trim().split_once(':').unwrap_or((s.trim(), "u32"));
    let a = parse_end(a)?;
    let t = match t.trim().to_ascii_lowercase().as_str() {
        "u8" => Tipo32::U8,
        "u16" => Tipo32::U16,
        "u32" | "" => Tipo32::U32,
        "i32" => Tipo32::I32,
        "f32" => Tipo32::F32,
        _ => return None,
    };
    Some((a, t))
}

/// Endereço em hexadecimal (com ou sem 0x).
pub fn parse_end(s: &str) -> Option<u32> {
    let s = s.trim();
    let h = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s);
    u32::from_str_radix(h, 16).ok()
}

/// Lê `n` bytes em `a` (None nos bytes de páginas não mapeadas).
fn ler(a: u32, n: usize) -> Vec<Option<u8>> {
    (0..n as u32)
        .map(|i| {
            let x = a.wrapping_add(i);
            super::mem::is_mapped(x).then(|| yw1_rt::r8(x) as u8)
        })
        .collect()
}

fn valor(a: u32, t: Tipo32) -> String {
    let n = match t {
        Tipo32::U8 => 1,
        Tipo32::U16 => 2,
        _ => 4,
    };
    let b = ler(a, n);
    if b.iter().any(Option::is_none) {
        return "(não mapeado)".into();
    }
    let mut w = [0u8; 4];
    for (i, x) in b.iter().enumerate() {
        w[i] = x.unwrap();
    }
    let u = u32::from_le_bytes(w);
    match t {
        Tipo32::U8 | Tipo32::U16 | Tipo32::U32 => format!("{u} ({u:#x})"),
        Tipo32::I32 => format!("{}", u as i32),
        Tipo32::F32 => format!("{}", f32::from_bits(u)),
    }
}

/// Estado das janelas (lado da janela do jogo).
pub struct Dev {
    pub aberto: bool,
    ganchos: bool,
    memoria: bool,
    arquivos: bool,
    camera: bool,
    desempenho: bool,
    end_txt: String,
    end: u32,
    vig_txt: String,
    filtro_arq: String,
    cache_ganchos: (Option<Instant>, String),
    /// (instante, quadros, draws, VBlanks) da última amostra e as taxas calculadas
    amostra: (Instant, u64, u64, u64),
    taxas: (f64, f64, f64),
}

impl Default for Dev {
    fn default() -> Self {
        Dev {
            aberto: false,
            ganchos: true,
            memoria: true,
            arquivos: true,
            camera: true,
            desempenho: true,
            end_txt: "0x5fe028".into(),
            end: 0x5f_e028,
            vig_txt: String::new(),
            filtro_arq: String::new(),
            cache_ganchos: (None, String::new()),
            amostra: (Instant::now(), 0, 0, 0),
            taxas: (0.0, 0.0, 0.0),
        }
    }
}

impl Dev {
    pub fn toggle(&mut self) {
        self.aberto = !self.aberto;
    }

    /// Desenha as janelas abertas. `s` = configurações (valores vigiados ficam em `dev.vigiados`).
    pub fn ui(&mut self, ctx: &egui::Context, s: &mut Settings) {
        if !self.aberto {
            return;
        }
        let area = ctx.content_rect();
        let max_h = (area.height() - 40.0).max(80.0);
        egui::Window::new("Ferramentas (F3)")
            .id(egui::Id::unique("yw_dev_barra"))
            .collapsible(true)
            .resizable(false)
            .anchor(Align2::RIGHT_BOTTOM, Vec2::new(-6.0, -6.0))
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.toggle_value(&mut self.ganchos, "Ganchos");
                    ui.toggle_value(&mut self.memoria, "Memória");
                    ui.toggle_value(&mut self.arquivos, "Arquivos");
                    ui.toggle_value(&mut self.camera, "Câmera");
                    ui.toggle_value(&mut self.desempenho, "Desempenho");
                });
            });
        if self.ganchos {
            let agora = Instant::now();
            if self.cache_ganchos.0.is_none_or(|t| agora.duration_since(t) > Duration::from_millis(500)) {
                self.cache_ganchos = (Some(agora), hooks::resumo());
            }
            let txt = &self.cache_ganchos.1;
            janela("Ganchos", max_h, &mut self.ganchos).show(ctx, |ui| {
                egui::ScrollArea::both().max_height(max_h - 40.0).show(ui, |ui| {
                    ui.add(egui::Label::new(RichText::new(txt).monospace().small()).extend());
                });
            });
        }
        if self.memoria {
            let mut aberto = true;
            janela("Memória", max_h, &mut aberto).show(ctx, |ui| self.ui_memoria(ui, s));
            self.memoria = aberto;
        }
        if self.arquivos {
            let mut aberto = true;
            janela("Arquivos abertos", max_h, &mut aberto).show(ctx, |ui| self.ui_arquivos(ui, max_h));
            self.arquivos = aberto;
        }
        if self.camera {
            janela("Câmera", max_h, &mut self.camera).show(ctx, |ui| {
                let cams = CAMS.lock().unwrap_or_else(|e| e.into_inner()).clone();
                let q = super::VBLANKS.load(Relaxed);
                ui.label(format!(
                    "cortes de câmera detectados (G1): {}",
                    super::etiquetas::N_CORTES_CAM.load(Relaxed)
                ));
                if cams.is_empty() {
                    ui.label(RichText::new("nenhum envio de câmera ainda").weak());
                }
                for c in cams.iter().rev() {
                    ui.separator();
                    ui.label(
                        RichText::new(format!(
                            "câmera {:#010x} +{:#x} · {} envios · último há {} VBlanks",
                            c.cam,
                            c.off,
                            c.envios,
                            q.saturating_sub(c.q)
                        ))
                        .monospace(),
                    );
                    ui.label(RichText::new(format!("olho  ({:9.2}, {:9.2}, {:9.2})", c.olho[0], c.olho[1], c.olho[2])).monospace());
                    for k in 0..3 {
                        let e = &c.eixos[3 * k..3 * k + 3];
                        ui.label(RichText::new(format!("eixo{k} ({:6.3}, {:6.3}, {:6.3})", e[0], e[1], e[2])).monospace());
                    }
                }
            });
        }
        if self.desempenho {
            self.amostrar();
            let (fps, draws, vbs) = self.taxas;
            janela("Desempenho", max_h, &mut self.desempenho).show(ctx, |ui| {
                ui.label(format!("{fps:.1} quadros do jogo/s · {draws:.0} draws por quadro · {vbs:.1} VBlanks/s"));
                ui.label(format!(
                    "turbo: {} ({}×; ligado {} vez(es), {} VBlanks em turbo)",
                    if super::turbo::ativo() { "LIGADO" } else { "solto" },
                    super::turbo::mult(),
                    super::turbo::ACIONAMENTOS.load(Relaxed),
                    super::turbo::VBLANKS_TURBO.load(Relaxed)
                ));
                ui.label(format!(
                    "VBlanks {} · tempo do jogo {:.1} s · relógio do VBlank {:.1} s",
                    super::VBLANKS.load(Relaxed),
                    super::VBLANKS.load(Relaxed) as f64 / super::vblank_hz(),
                    super::relogio_vblank_s()
                ));
                if crate::render::backend() == crate::render::Backend::Vulkan {
                    let st = crate::render::vulkan::stats();
                    let (vram, orc, host, tex) = crate::render::vulkan::mem_report();
                    let mb = |b: u64| b as f64 / (1024.0 * 1024.0);
                    ui.label(format!(
                        "Vulkan: pipelines {} · compilações síncronas {} · VRAM {:.0}/{:.0} MB · host {:.0} MB · texturas em cache {:.0} MB",
                        st[4],
                        st[20],
                        mb(vram),
                        mb(orc),
                        mb(host),
                        mb(tex)
                    ));
                }
                ui.label(format!("interpolação pedida: {} Hz", crate::render::interp_target()));
            });
        }
    }

    fn amostrar(&mut self) {
        let agora = Instant::now();
        let dt = agora.duration_since(self.amostra.0).as_secs_f64();
        if dt < 1.0 {
            return;
        }
        let sw = super::services::gsp::SWAPS[0].load(Relaxed);
        let dr = crate::pica::DRAWS.load(Relaxed);
        let vb = super::VBLANKS.load(Relaxed);
        let (_, s0, d0, v0) = self.amostra;
        let q = sw.saturating_sub(s0);
        self.taxas = (q as f64 / dt, dr.saturating_sub(d0) as f64 / q.max(1) as f64, vb.saturating_sub(v0) as f64 / dt);
        self.amostra = (agora, sw, dr, vb);
    }

    fn ui_memoria(&mut self, ui: &mut egui::Ui, s: &mut Settings) {
        ui.horizontal(|ui| {
            ui.label("endereço");
            let r = ui.add(egui::TextEdit::singleline(&mut self.end_txt).desired_width(90.0).font(egui::TextStyle::Monospace));
            if r.changed() {
                if let Some(a) = parse_end(&self.end_txt) {
                    self.end = a;
                }
            }
            for (t, d) in [("−0x100", -0x100i64), ("−0x10", -0x10), ("+0x10", 0x10), ("+0x100", 0x100)] {
                if ui.small_button(t).clicked() {
                    self.end = (self.end as i64 + d).clamp(0, u32::MAX as i64) as u32;
                    self.end_txt = format!("{:#x}", self.end);
                }
            }
        });
        let base = self.end & !0xf;
        let bytes = ler(base, 256);
        let mut txt = String::new();
        for l in 0..16 {
            txt.push_str(&format!("{:08x} ", base + l * 16));
            let linha = &bytes[l as usize * 16..l as usize * 16 + 16];
            for (i, b) in linha.iter().enumerate() {
                if i == 8 {
                    txt.push(' ');
                }
                match b {
                    Some(b) => txt.push_str(&format!(" {b:02x}")),
                    None => txt.push_str(" ··"),
                }
            }
            txt.push_str("  ");
            for b in linha {
                txt.push(match b {
                    Some(b) if b.is_ascii_graphic() || *b == b' ' => *b as char,
                    Some(_) => '.',
                    None => ' ',
                });
            }
            txt.push('\n');
        }
        ui.add(egui::Label::new(RichText::new(txt).monospace().small()).extend());
        ui.separator();
        ui.label("Valores vigiados (\"0xEND:tipo\", tipo u8 u16 u32 i32 f32):");
        ui.horizontal(|ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut self.vig_txt).desired_width(150.0).font(egui::TextStyle::Monospace));
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (ui.button("vigiar").clicked() || enter) && parse_vigiado(&self.vig_txt).is_some() {
                s.dev.vigiados.push(self.vig_txt.trim().to_string());
                self.vig_txt.clear();
            }
        });
        let mut tirar = None;
        egui::Grid::new("yw_dev_vig").num_columns(3).striped(true).show(ui, |ui| {
            for (i, v) in s.dev.vigiados.iter().enumerate() {
                ui.label(RichText::new(v).monospace());
                match parse_vigiado(v) {
                    Some((a, t)) => ui.label(RichText::new(valor(a, t)).monospace()),
                    None => ui.label(RichText::new("inválido").weak()),
                };
                if ui.small_button("×").clicked() {
                    tirar = Some(i);
                }
                ui.end_row();
            }
        });
        if let Some(i) = tirar {
            s.dev.vigiados.remove(i);
        }
    }

    fn ui_arquivos(&mut self, ui: &mut egui::Ui, max_h: f32) {
        let g = ARQ.lock().unwrap_or_else(|e| e.into_inner());
        let Some(a) = g.as_ref() else {
            ui.label(RichText::new("nenhuma abertura ainda").weak());
            return;
        };
        ui.label(format!(
            "{} aberturas · {} caminhos distintos · trocados por mods: {}",
            a.total,
            a.contagem.len(),
            arquivos::TROCAS.load(Relaxed)
        ));
        ui.horizontal(|ui| {
            ui.label("filtro");
            ui.add(egui::TextEdit::singleline(&mut self.filtro_arq).desired_width(160.0));
        });
        let f = self.filtro_arq.to_ascii_lowercase();
        egui::ScrollArea::vertical().max_height(max_h - 90.0).show(ui, |ui| {
            for ab in a.recentes.iter().rev().filter(|x| f.is_empty() || x.caminho.to_ascii_lowercase().contains(&f)) {
                ui.label(
                    RichText::new(format!(
                        "q={:<7} {:<18} {} (×{})",
                        ab.q,
                        ab.tipo,
                        ab.caminho,
                        a.contagem.get(&ab.caminho).copied().unwrap_or(0)
                    ))
                    .monospace()
                    .small(),
                );
            }
        });
    }
}

fn janela<'a>(titulo: &'a str, max_h: f32, aberto: &'a mut bool) -> egui::Window<'a> {
    egui::Window::new(titulo)
        .id(egui::Id::unique(("yw_dev", titulo)))
        .open(aberto)
        .collapsible(true)
        .resizable(true)
        .default_width(420.0)
        .max_height(max_h)
}

/// Indicador do turbo no canto de cima à direita ("▶▶ 3×").
pub fn turbo_overlay(ctx: &egui::Context, k: u32) {
    egui::Area::new(egui::Id::unique("yw_turbo"))
        .anchor(Align2::RIGHT_TOP, Vec2::new(-6.0, 6.0))
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(egui::Color32::from_black_alpha(170))
                .corner_radius(4.0)
                .inner_margin(egui::Margin::symmetric(8, 3))
                .show(ui, |ui| {
                    ui.label(RichText::new(format!("▶▶ {k}×")).strong().size(16.0).color(egui::Color32::from_rgb(255, 210, 80)));
                });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vigiado_parse() {
        assert_eq!(parse_vigiado("0x5fe028:u32"), Some((0x5fe028, Tipo32::U32)));
        assert_eq!(parse_vigiado("5fe028"), Some((0x5fe028, Tipo32::U32)));
        assert_eq!(parse_vigiado("0x10:f32"), Some((0x10, Tipo32::F32)));
        assert_eq!(parse_vigiado("0x10:zz"), None);
        assert_eq!(parse_vigiado("xyz"), None);
    }

    #[test]
    fn janelas_desenham_sem_jogo() {
        // egui sem GPU e sem o jogo rodando (memória não mapeada, sem câmeras/arquivos): não pode dar pane
        let ctx = egui::Context::default();
        // (sem "Desempenho": `render::backend()` iniciaria o Vulkan no teste)
        let mut d = Dev { aberto: true, desempenho: false, ..Dev::default() };
        let mut s = Settings::default();
        s.dev.vigiados = vec!["0x10:u32".into(), "ruim:zz".into()];
        for _ in 0..2 {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 480.0))),
                ..Default::default()
            };
            let _ = ctx.run_pass(raw, |ctx| {
                d.ui(ctx, &mut s);
                turbo_overlay(ctx, 3);
            });
        }
    }
}
