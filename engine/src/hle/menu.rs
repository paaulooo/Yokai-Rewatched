//! Menu sobre o jogo (egui): Continuar · Configurações · Sair. As configurações seguem as seções do Zakuro
//! (Gráficos, Som, Controles, Controle, Emulação) e o remapeamento é "clicar no botão do 3DS e apertar a tecla nova".
//! Opções que ainda não existem no port aparecem desabilitadas com "(em breve)".

use super::input::{key_name, pad_name};
use super::settings::{
    Apresentacao, Botao, Controle, Filtro, Idioma, Layout, LimiteFps, PadLayout, Proporcao, Renderizacao, Renderizador, Settings, Teclado,
};
use egui::{Align2, RichText, Vec2};
use winit::keyboard::KeyCode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pagina {
    Principal,
    Config,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Secao {
    Graficos,
    Som,
    Teclado,
    Controle,
    Emulacao,
    Mods,
}

impl Secao {
    const ALL: [Secao; 6] = [Secao::Graficos, Secao::Som, Secao::Teclado, Secao::Controle, Secao::Emulacao, Secao::Mods];
    fn nome(self) -> &'static str {
        match self {
            Secao::Graficos => "Gráficos",
            Secao::Som => "Som",
            Secao::Teclado => "Controles (teclado)",
            Secao::Controle => "Controle (gamepad)",
            Secao::Emulacao => "Emulação",
            Secao::Mods => "Mods",
        }
    }
}

/// Esperando a tecla / o botão novo de uma entrada.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Captura {
    /// tecla do teclado: entrada e posição na lista (posição = tamanho → acrescenta)
    Tecla(Botao, usize),
    Pad(Botao),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Acao {
    Nada,
    Continuar,
    Sair,
}

/// O que o menu mostra mas não controla.
#[derive(Default)]
pub struct Info {
    /// controles conectados (nomes)
    pub pads: Vec<String>,
    /// adaptador gráfico da janela (wgpu)
    pub apresentacao: String,
    /// modos de apresentação que a GPU aceita (nomes do `wgpu::PresentMode`)
    pub modos_presente: Vec<String>,
    /// modo e fila realmente em uso na superfície
    pub modo_em_uso: String,
    /// `YW_RENDERER` definido: o ambiente decide o renderizador
    pub renderer_do_ambiente: bool,
    /// renderizador do PICA que está de fato rodando (`render::backend()`; pode ter caído de Vulkan para Soft)
    pub renderer_em_uso: Option<crate::render::Backend>,
    /// interpolação em uso (Hz; 0 = desligada) e taxa do monitor da janela (0 = desconhecida)
    pub interp: (u32, u32),
    /// widescreen em efeito: tamanho da tela de cima na resolução interna (largura, altura); `None` = original
    pub wide: Option<(u32, u32)>,
}

pub struct Menu {
    pub open: bool,
    pagina: Pagina,
    secao: Secao,
    pub captura: Option<Captura>,
    /// mods listados na aba (os do boot; "procurar de novo" relê a pasta)
    mods_vistos: Option<Vec<super::mods::indice::ModDesc>>,
    /// mod cujo código o usuário quer ligar: esperando a confirmação do aviso
    confirmar_codigo: Option<String>,
}

impl Default for Menu {
    fn default() -> Self {
        Menu {
            open: false,
            pagina: Pagina::Principal,
            secao: Secao::Graficos,
            captura: None,
            mods_vistos: None,
            confirmar_codigo: None,
        }
    }
}

impl Menu {
    pub fn toggle(&mut self) {
        if self.captura.take().is_some() {
            return; // Esc durante a captura só cancela
        }
        self.open = !self.open;
        self.pagina = Pagina::Principal;
    }

    /// Tecla apertada enquanto o menu espera uma tecla nova. Devolve se usou a tecla.
    pub fn capture_key(&mut self, s: &mut Settings, key: KeyCode) -> bool {
        let Some(Captura::Tecla(b, slot)) = self.captura else { return false };
        match key {
            KeyCode::Escape => {}
            // reservadas (menu / ferramentas / tela cheia): continua esperando
            KeyCode::F1 | KeyCode::F3 | KeyCode::F11 => return true,
            k => s.teclado.assign(b, slot, k),
        }
        self.captura = None;
        true
    }

    /// Botão do controle apertado enquanto o menu espera um botão novo. Devolve se usou o botão.
    pub fn capture_pad(&mut self, s: &mut Settings, pad: gilrs::Button) -> bool {
        let Some(Captura::Pad(b)) = self.captura else { return false };
        if pad == gilrs::Button::Mode {
            return true; // Guide abre/fecha o menu
        }
        s.controle.assign(b, pad);
        self.captura = None;
        true
    }

    /// Desenha o menu. `s` é alterado diretamente.
    pub fn ui(&mut self, ctx: &egui::Context, s: &mut Settings, info: &Info) -> Acao {
        if !self.open {
            return Acao::Nada;
        }
        // (o jogo atrás do menu é escurecido pela janela, na mesma camada das telas)
        let area = ctx.content_rect();
        let mut acao = Acao::Nada;
        let titulo = match self.pagina {
            Pagina::Principal => "Yo-kai Watch — pausado",
            Pagina::Config => "Configurações",
        };
        // tamanho inicial 20 % maior que o fixo de antes (260 e 560 de largura); a borda redimensiona, e o egui guarda o
        // tamanho escolhido enquanto o jogo estiver aberto (cada página o seu)
        let max_w = (area.width() - 24.0).max(120.0);
        let max_h = (area.height() - 48.0).max(80.0);
        let (largura, altura) = match self.pagina {
            Pagina::Principal => (312.0f32, 0.0f32),
            Pagina::Config => (672.0f32, max_h),
        };
        let mut janela = egui::Window::new(titulo)
            .id(egui::Id::unique(("yw_menu", titulo)))
            .collapsible(false)
            .resizable(true)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .default_width(largura.min(max_w))
            .min_width(220.0f32.min(max_w))
            .max_width(max_w)
            .min_height(80.0f32.min(max_h))
            .max_height(max_h);
        if altura > 0.0 {
            janela = janela.default_height(altura);
        }
        janela
            .show(ctx, |ui| match self.pagina {
                Pagina::Principal => {
                    ui.vertical_centered_justified(|ui| {
                        ui.add_space(4.0);
                        if ui.button(RichText::new("Continuar").size(18.0)).clicked() {
                            acao = Acao::Continuar;
                        }
                        ui.add_space(4.0);
                        if ui.button(RichText::new("Configurações").size(18.0)).clicked() {
                            self.pagina = Pagina::Config;
                        }
                        ui.add_space(4.0);
                        if ui.button(RichText::new("Sair").size(18.0)).clicked() {
                            acao = Acao::Sair;
                        }
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(
                                "Voltar ao jogo: Esc / F1 · no controle, Start + Select juntos (ou Guide, se o \
                                 controle o enviar)",
                            )
                            .weak()
                            .small(),
                        );
                    });
                }
                Pagina::Config => self.config(ui, s, info),
            });
        if acao == Acao::Continuar {
            self.open = false;
            self.captura = None;
        }
        acao
    }

    fn config(&mut self, ui: &mut egui::Ui, s: &mut Settings, info: &Info) {
        ui.horizontal_wrapped(|ui| {
            for sec in Secao::ALL {
                if ui.selectable_value(&mut self.secao, sec, sec.nome()).clicked() {
                    self.captura = None;
                }
            }
        });
        ui.separator();
        let h = (ui.available_height() - 40.0).max(60.0);
        egui::ScrollArea::vertical().max_height(h).auto_shrink([false, true]).show(ui, |ui| match self.secao {
            Secao::Graficos => graficos(ui, s, info),
            Secao::Som => som(ui, s),
            Secao::Teclado => self.teclado(ui, &mut s.teclado),
            Secao::Controle => self.controle(ui, &mut s.controle, info),
            Secao::Emulacao => emulacao(ui, s),
            Secao::Mods => self.mods(ui, s),
        });
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("← Voltar").clicked() {
                self.pagina = Pagina::Principal;
                self.captura = None;
            }
            if ui.button("Restaurar padrão desta seção").clicked() {
                self.captura = None;
                let d = Settings::default();
                match self.secao {
                    Secao::Graficos => s.graficos = d.graficos,
                    Secao::Som => s.som = d.som,
                    Secao::Teclado => s.teclado = d.teclado,
                    Secao::Controle => s.controle = Controle::preset(s.controle.layout),
                    Secao::Emulacao => s.emulacao = d.emulacao,
                    Secao::Mods => {
                        s.mods = d.mods;
                        self.confirmar_codigo = None;
                    }
                }
            }
        });
    }

    /// Aba "Mods": lista da pasta de mods, ligar/desligar, ordem e o código nativo (com aviso). Vale ao reiniciar.
    fn mods(&mut self, ui: &mut egui::Ui, s: &mut Settings) {
        use super::mods::{self, indice};
        let est = mods::estado();
        let lista = self
            .mods_vistos
            .get_or_insert_with(|| {
                if est.mods.is_empty() {
                    mods::descobrir_de_novo()
                } else {
                    est.mods.iter().map(|m| m.desc.clone()).collect()
                }
            })
            .clone();
        let pasta = mods::pasta();
        ui.horizontal_wrapped(|ui| {
            ui.label("Pasta:");
            ui.label(RichText::new(pasta.display().to_string()).monospace());
        });
        ui.horizontal(|ui| {
            if ui.button("Abrir a pasta").clicked() {
                let _ = std::fs::create_dir_all(&pasta);
                #[cfg(windows)]
                let _ = std::process::Command::new("explorer").arg(&pasta).spawn();
            }
            if ui.button("Procurar de novo").clicked() {
                self.mods_vistos = Some(mods::descobrir_de_novo());
            }
        });
        if est.desligado_pelo_ambiente {
            ui.colored_label(egui::Color32::from_rgb(230, 170, 60), "YW_MODS=0 (ambiente): nenhum mod vale nesta execução.");
        }
        ui.label(
            RichText::new(
                "Mudanças valem ao reiniciar o jogo. Cada mod é uma pasta com mod.toml; romfs/<caminho do .fa> troca o \
                 arquivo do jogo. Quando dois mods trocam o mesmo arquivo, vale o de cima.",
            )
            .weak()
            .small(),
        );
        ui.separator();
        if lista.is_empty() {
            ui.label("Nenhum mod na pasta.");
            return;
        }
        let ordem = indice::ordem_efetiva(&lista, &s.mods);
        let ids: Vec<String> = ordem.iter().map(|&i| lista[i].id.clone()).collect();
        let mut mover: Option<(usize, usize)> = None;
        let vermelho = egui::Color32::from_rgb(230, 90, 80);
        for (pos, &i) in ordem.iter().enumerate() {
            let m = &lista[i];
            let sit = est.mods.iter().find(|x| x.desc.id == m.id);
            ui.horizontal(|ui| {
                if ui.add_enabled(pos > 0, egui::Button::new("↑").small()).on_hover_text("subir (vence os de baixo)").clicked() {
                    mover = Some((pos, pos - 1));
                }
                if ui.add_enabled(pos + 1 < ordem.len(), egui::Button::new("↓").small()).on_hover_text("descer").clicked() {
                    mover = Some((pos, pos + 1));
                }
                let mut on = !s.mods.desligados.contains(&m.id);
                let titulo = if m.versao.is_empty() { m.nome.clone() } else { format!("{} {}", m.nome, m.versao) };
                if ui.add_enabled(m.erro.is_none(), egui::Checkbox::new(&mut on, RichText::new(titulo).strong())).changed() {
                    s.mods.desligados.retain(|d| d != &m.id);
                    if !on {
                        s.mods.desligados.push(m.id.clone());
                    }
                }
                if !m.autor.is_empty() {
                    ui.label(RichText::new(format!("por {}", m.autor)).weak());
                }
            });
            ui.indent(("yw_mod", m.id.as_str()), |ui| {
                if !m.descricao.is_empty() {
                    ui.label(RichText::new(m.descricao.as_str()).small());
                }
                if let Some(e) = &m.erro {
                    ui.colored_label(vermelho, format!("ignorado: {e}"));
                }
                let mut det = format!("pasta {} · prioridade {} · {} arquivos", m.id, m.prioridade, m.arquivos.len());
                match sit {
                    Some(x) => det += &format!(" ({} valendo nesta execução)", x.arquivos_valendo),
                    None => det += " (achado depois do início: vale ao reiniciar)",
                }
                ui.label(RichText::new(det).weak().small());
                for a in &m.avisos {
                    ui.label(RichText::new(format!("aviso: {a}")).weak().small());
                }
                if let Some(dll) = &m.dll {
                    let mut cod = s.mods.codigo_ligado.contains(&m.id);
                    let nome_dll = dll.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let r = ui.add_enabled(m.erro.is_none(), egui::Checkbox::new(&mut cod, format!("Código nativo ({nome_dll})")));
                    if r.changed() {
                        if cod {
                            // não liga direto: pede a confirmação do aviso abaixo
                            self.confirmar_codigo = Some(m.id.clone());
                        } else {
                            s.mods.codigo_ligado.retain(|d| d != &m.id);
                            self.confirmar_codigo = None;
                        }
                    }
                    match sit.and_then(|x| x.codigo.as_ref()) {
                        Some(Ok(n)) => ui.label(RichText::new(format!("código carregado nesta execução ({n} ganchos)")).weak().small()),
                        Some(Err(e)) => ui.colored_label(vermelho, format!("código recusado: {e}")),
                        None => ui.label(RichText::new("código não carregado nesta execução").weak().small()),
                    };
                    if self.confirmar_codigo.as_deref() == Some(m.id.as_str()) {
                        egui::Frame::group(ui.style()).show(ui, |ui| {
                            ui.colored_label(
                                egui::Color32::from_rgb(230, 170, 60),
                                "Atenção: um mod de código é um programa (DLL) que roda dentro do jogo com confiança \
                                 total — pode ler e mudar qualquer coisa no seu PC, como qualquer programa. Só ligue \
                                 se você confia em quem fez o mod.",
                            );
                            ui.horizontal(|ui| {
                                if ui.button("Ligar o código mesmo assim").clicked() {
                                    if !s.mods.codigo_ligado.contains(&m.id) {
                                        s.mods.codigo_ligado.push(m.id.clone());
                                    }
                                    self.confirmar_codigo = None;
                                }
                                if ui.button("Cancelar").clicked() {
                                    self.confirmar_codigo = None;
                                }
                            });
                        });
                    }
                }
            });
            ui.add_space(4.0);
        }
        if let Some((a, b)) = mover {
            let mut v = ids;
            v.swap(a, b);
            s.mods.ordem = v;
        }
    }

    fn teclado(&mut self, ui: &mut egui::Ui, t: &mut Teclado) {
        ui.label(
            RichText::new(
                "Clique numa tecla e aperte a nova (botão direito remove; + acrescenta). Esc ou clicar de \
                 novo cancela. Esc, F1 (menu), F3 (ferramentas) e F11 (tela cheia) não podem ser usadas. \
                 Turbo: enquanto segurada, o jogo anda mais rápido (Emulação → Turbo).",
            )
            .weak(),
        );
        ui.add_space(4.0);
        egui::Grid::new("yw_teclado").num_columns(2).striped(true).spacing([12.0, 4.0]).show(ui, |ui| {
            for b in Botao::ALL {
                ui.label(b.nome());
                ui.horizontal(|ui| {
                    let keys = t.get(b).clone();
                    let mut remove = None;
                    for (i, k) in keys.iter().enumerate() {
                        let esperando = self.captura == Some(Captura::Tecla(b, i));
                        let txt = if esperando { "aperte…".to_string() } else { key_name(*k) };
                        let r = ui.add(egui::Button::new(txt).selected(esperando).min_size(Vec2::new(44.0, 0.0)));
                        if r.clicked() {
                            self.captura = if esperando { None } else { Some(Captura::Tecla(b, i)) };
                        }
                        if r.secondary_clicked() {
                            remove = Some(i);
                        }
                    }
                    let novo = self.captura == Some(Captura::Tecla(b, keys.len()));
                    if ui.add(egui::Button::new(if novo { "aperte…" } else { "+" }).selected(novo)).clicked() {
                        self.captura = if novo { None } else { Some(Captura::Tecla(b, keys.len())) };
                    }
                    if let Some(i) = remove {
                        t.get_mut(b).remove(i);
                        self.captura = None;
                    }
                });
                ui.end_row();
            }
        });
    }

    fn controle(&mut self, ui: &mut egui::Ui, c: &mut Controle, info: &Info) {
        let sem_pad = info.pads.is_empty();
        if sem_pad {
            ui.label(RichText::new("Nenhum controle conectado (conecte um para remapear).").weak());
        } else {
            for p in &info.pads {
                ui.label(format!("🎮 {p}"));
            }
        }
        ui.add_space(4.0);
        let mut layout = c.layout;
        egui::ComboBox::from_label("Disposição dos botões")
            .selected_text(layout.nome())
            .show_ui(ui, |ui| {
                for l in PadLayout::ALL {
                    ui.selectable_value(&mut layout, l, l.nome());
                }
            });
        if layout != c.layout {
            c.set_layout(layout);
        }
        ui.add(egui::Slider::new(&mut c.zona_morta, 0.0..=0.9).text("Zona morta do analógico"));
        ui.label(
            RichText::new(
                "Analógico esquerdo = Circle Pad. Clique num botão e aperte o novo no controle (clicar de novo \
                 cancela). Start + Select juntos abrem/fecham o menu.",
            )
            .weak(),
        );
        ui.add_space(4.0);
        egui::Grid::new("yw_pad").num_columns(2).striped(true).spacing([12.0, 4.0]).show(ui, |ui| {
            for b in Botao::PAD {
                ui.label(b.nome());
                let esperando = self.captura == Some(Captura::Pad(b));
                let txt = if esperando {
                    "aperte um botão no controle…".to_string()
                } else {
                    c.get(b).map(pad_name).unwrap_or_default()
                };
                if ui.add_enabled(!sem_pad || esperando, egui::Button::new(txt).selected(esperando)).clicked() {
                    self.captura = if esperando { None } else { Some(Captura::Pad(b)) };
                }
                ui.end_row();
            }
        });
    }
}

fn combo<T: Copy + PartialEq>(ui: &mut egui::Ui, label: &str, v: &mut T, all: &[T], nome: fn(T) -> &'static str) {
    egui::ComboBox::from_label(label).selected_text(nome(*v)).show_ui(ui, |ui| {
        for &o in all {
            ui.selectable_value(v, o, nome(o));
        }
    });
}

fn graficos(ui: &mut egui::Ui, s: &mut Settings, info: &Info) {
    let g = &mut s.graficos;
    ui.horizontal(|ui| {
        combo(ui, "Renderizador", &mut g.renderizador, &Renderizador::ALL, Renderizador::nome);
    });
    let nota = if info.renderer_do_ambiente {
        "vale ao reiniciar · agora YW_RENDERER (ambiente) está mandando"
    } else {
        "vale ao reiniciar o jogo"
    };
    ui.label(RichText::new(nota).weak().small());
    if let Some(b) = info.renderer_em_uso {
        use crate::render::Backend;
        let txt = match b {
            Backend::Vulkan => "Em uso agora: Vulkan",
            Backend::Soft if g.renderizador == Renderizador::Vulkan && !info.renderer_do_ambiente => {
                "Em uso agora: Soft (o Vulkan não inicializou)"
            }
            Backend::Soft => "Em uso agora: Soft",
            Backend::None => "Em uso agora: nenhum (YW_RENDERER=none)",
        };
        ui.label(RichText::new(txt).small());
    }
    combo(ui, "Renderização", &mut g.renderizacao, &Renderizacao::ALL, Renderizacao::nome);
    ui.label(
        RichText::new(
            "Assíncrona: a GPU do PC desenha enquanto o jogo segue para o próximo quadro. Síncrona: cada lote é \
             desenhado e esperado na hora (mais lenta; só para comparar ou contornar problemas). Só no renderizador \
             Vulkan; vale ao reiniciar; YW_VK_ASYNC (ambiente) manda por cima.",
        )
        .weak()
        .small(),
    );
    ui.add_space(6.0);
    ui.label(RichText::new("Apresentação da janela").strong());
    combo(ui, "Modo", &mut g.apresentacao, &Apresentacao::ALL, Apresentacao::nome);
    let aceita = |n: &str| info.modos_presente.iter().any(|m| m == n);
    let falta = match g.apresentacao {
        Apresentacao::Caixa if !aceita("Mailbox") => Some("esta GPU não oferece Mailbox: usando V-Sync"),
        Apresentacao::SemSincronia if !aceita("Immediate") => Some("esta GPU não oferece modo imediato: usando o que o driver tiver sem V-Sync"),
        _ => None,
    };
    if let Some(f) = falta {
        ui.label(RichText::new(f).small().color(egui::Color32::from_rgb(220, 160, 60)));
    }
    ui.horizontal(|ui| {
        ui.label("Quadros em voo:");
        for n in 1..=3 {
            ui.selectable_value(&mut g.quadros_em_voo, n, n.to_string());
        }
    });
    combo(ui, "Redesenhos por segundo (máx.)", &mut g.limite_fps, &LimiteFps::ALL, LimiteFps::nome);
    ui.label(
        RichText::new(
            "O jogo anda sempre na velocidade do console (30 quadros/s de imagem nova); estes ajustes só decidem como \
             e quando cada quadro novo chega ao monitor: Mailbox/Imediato e 1 quadro em voo dão a menor latência. \
             Redesenhos são as repinturas da janela sem imagem nova (menu, mouse); desligados, a janela só repinta \
             quando o jogo ou a interpolação trazem imagem nova, e a entrada continua lida a cada milissegundo. \
             Valem na hora.",
        )
        .weak()
        .small(),
    );
    if !info.modo_em_uso.is_empty() {
        ui.label(RichText::new(format!("Em uso: {}", info.modo_em_uso)).small());
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label("Anisotrópico (texturas):");
        for (n, nome) in [(0, "desligado"), (2, "2×"), (4, "4×"), (8, "8×"), (16, "16×")] {
            ui.selectable_value(&mut g.anisotropico, n, nome);
        }
    });
    ui.label(
        RichText::new(
            "Extra do port (o PICA real não tem): em superfícies inclinadas ou distantes o jogo tira várias amostras \
             da textura em vez de uma, menos serrilhado e cintilação. Só no renderizador Vulkan; custa GPU.",
        )
        .weak()
        .small(),
    );
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label("Resolução interna:");
        for n in 1..=6u32 {
            let nome = if n == 1 { "1× (nativa)".to_string() } else { format!("{n}×") };
            ui.selectable_value(&mut g.escala_interna, n, nome);
        }
    });
    ui.label(
        RichText::new(
            "A cena 3D das telas é desenhada em N× o tamanho do 3DS (a de cima: 400N×240N pixels); o jogo continua \
             lendo a imagem nativa, então nada do jogo muda. Custa GPU (cerca de N² vezes o desenho das telas). Só no \
             renderizador Vulkan.",
        )
        .weak()
        .small(),
    );
    combo(ui, "Proporção da tela de cima", &mut g.proporcao, &Proporcao::ALL, Proporcao::nome);
    ui.checkbox(&mut g.hud_ancorada, "HUD ancorada nas bordas (widescreen)");
    ui.label(
        RichText::new(
            "Widescreen: o jogo passa a enxergar uma cena mais larga (nada é esticado e o que fica nas bordas não              some); a HUD fica no retângulo 5:3 do meio e os fades/filtros de tela cheia cobrem a largura toda.              \"Seguir a janela\" usa a proporção da área da tela de cima no layout escolhido. A tela de baixo não              muda. Vale na hora; YW_ASPECT (ambiente) manda por cima. Só no renderizador Vulkan.",
        )
        .weak()
        .small(),
    );
    if let Some((w, h)) = info.wide {
        ui.label(RichText::new(format!("Em uso: tela de cima {w}×{h} na resolução interna ({:.3}:1)", w as f32 / h as f32)).small());
    }
    ui.horizontal(|ui| {
        ui.label("Interpolação de quadros:");
        for (hz, nome) in [(0u32, "desligada"), (60, "60"), (90, "90"), (120, "120"), (144, "144"), (180, "180")] {
            ui.selectable_value(&mut g.interpolacao, hz, nome);
        }
    });
    ui.label(
        RichText::new(
            "O jogo roda a 30 quadros por segundo e conta tempo em quadros, então a velocidade não muda. Entre dois \
             quadros reais o port refaz o desenho da cena com as posições (matrizes de câmera e de ossos, rotações \
             por quatérnio) em ponto intermediário: nenhuma imagem é misturada. Cortes de cena, vídeo e geometria \
             que muda de forma voltam ao quadro real. Os passos aparecem em ordem e igualmente espaçados, e nunca \
             passam da taxa do monitor. Atrasa a imagem em até um quadro do jogo e custa GPU (≈2–3 ms por quadro \
             intermediário). Só no renderizador Vulkan.",
        )
        .weak()
        .small(),
    );
    if g.interpolacao > 0 {
        let (em_uso, monitor) = info.interp;
        let txt = match monitor {
            0 => format!("Em uso: {em_uso} Hz"),
            m if em_uso < g.interpolacao => format!("Em uso: {em_uso} Hz (limitada à taxa do monitor, {m} Hz)"),
            m => format!("Em uso: {em_uso} Hz (monitor: {m} Hz)"),
        };
        ui.label(RichText::new(txt).small());
    }
    ui.checkbox(&mut g.fxaa, "FXAA só na cena 3D (não suaviza texto nem HUD)");
    ui.checkbox(&mut g.fsr, "FSR 1 ao ampliar para a janela (EASU + nitidez)");
    ui.add_enabled_ui(g.fsr, |ui| {
        ui.add(egui::Slider::new(&mut g.fsr_nitidez, 0..=100).text("Nitidez do FSR").suffix(" %"));
    });
    ui.label(
        RichText::new(
            "FSR 1 só entra quando a janela é maior que a imagem da tela. O FSR 2/3 (temporal e geração de quadros) \
             precisa de vetores de movimento e jitter do motor do jogo, e o FSR 4 só existe em GPUs RDNA 4: nenhum \
             dos dois se aplica a este port.",
        )
        .weak()
        .small(),
    );
    ui.checkbox(&mut g.estereo_3d, "3D estéreo (desenha o olho direito; vale ao reiniciar)");
    ui.label(
        RichText::new(
            "A janela mostra só o olho esquerdo. Desligado, o jogo roda no modo 2D do console e desenha a tela de \
             cima uma vez por quadro em vez de duas. YW_STEREO (ambiente) manda por cima.",
        )
        .weak()
        .small(),
    );
    ui.add_space(6.0);
    combo(ui, "Layout das telas", &mut g.layout, &Layout::ALL, Layout::nome);
    ui.horizontal(|ui| {
        ui.label("Filtro:");
        for f in Filtro::ALL {
            ui.radio_value(&mut g.filtro, f, f.nome());
        }
    });
    ui.checkbox(&mut g.escala_inteira, "Escala inteira (pixels todos do mesmo tamanho)");
    ui.horizontal(|ui| {
        ui.label("Tamanho da janela:");
        for n in 1..=4 {
            ui.selectable_value(&mut g.escala_janela, n, format!("{n}×"));
        }
    });
    ui.checkbox(&mut g.tela_cheia, "Tela cheia (F11)");
    ui.checkbox(&mut g.mostrar_fps, "Mostrar FPS do jogo");
    if !info.apresentacao.is_empty() {
        ui.add_space(6.0);
        ui.label(
            RichText::new(format!(
                "Só a apresentação da janela (menu e cópia das telas) usa {}; o jogo é desenhado pelo \
                 renderizador acima.",
                info.apresentacao
            ))
            .weak()
            .small(),
        );
    }
}

fn som(ui: &mut egui::Ui, s: &mut Settings) {
    ui.add_enabled(!s.som.mudo, egui::Slider::new(&mut s.som.volume, 0..=100).text("Volume").suffix(" %"));
    ui.checkbox(&mut s.som.mudo, "Mudo");
}

fn emulacao(ui: &mut egui::Ui, s: &mut Settings) {
    emulacao_base(ui, s);
    ui.add_space(6.0);
    ui.checkbox(&mut s.dev.ferramentas, "Ferramentas de desenvolvedor (F3 no jogo)");
    ui.label(
        RichText::new("ganchos, memória, arquivos abertos, câmera e desempenho; YW_DEV=1/0 vence esta opção")
            .weak()
            .small(),
    );
}

fn emulacao_base(ui: &mut egui::Ui, s: &mut Settings) {
    combo(ui, "Idioma do console", &mut s.emulacao.idioma, &Idioma::ALL, Idioma::nome);
    ui.label(RichText::new("vale ao reiniciar o jogo (o cartucho americano tem inglês, francês e espanhol)").weak().small());
    ui.add_space(6.0);
    ui.label("Velocidade: normal (como no 3DS); o turbo só vale enquanto a tecla/botão dele é segurado");
    ui.add(egui::Slider::new(&mut s.emulacao.turbo, 2..=4).text("Turbo (segurar)").suffix("×"));
    ui.label(
        RichText::new(
            "Turbo: Tab no teclado / clique do analógico direito no controle (remapeáveis). O som fica mudo e a \
             interpolação de quadros pausa enquanto ele está ligado; ao soltar, volta ao normal no quadro seguinte.",
        )
        .weak()
        .small(),
    );
    ui.label(
        RichText::new(
            "Com este menu aberto o jogo fica pausado: VBlank, som, relógio do sistema e timers param.",
        )
        .weak()
        .small(),
    );
}

/// Janela de erro quando o jogo parou. Devolve `true` se o usuário pediu para sair.
pub fn crash_window(ctx: &egui::Context, msg: &str) -> bool {
    let mut sair = false;
    let area = ctx.content_rect();
    egui::Window::new("O jogo parou")
        .id(egui::Id::unique("yw_crash"))
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
        .max_width((area.width() - 24.0).max(120.0))
        .max_height((area.height() - 48.0).max(80.0))
        .show(ctx, |ui| {
            ui.label("O código recompilado parou com este erro:");
            egui::ScrollArea::vertical().max_height((area.height() - 160.0).max(40.0)).show(ui, |ui| {
                ui.add(egui::Label::new(RichText::new(msg).monospace()).wrap());
            });
            ui.label(RichText::new("Detalhes das threads saem no terminal ao fechar.").weak().small());
            ui.horizontal(|ui| {
                if ui.button("Copiar mensagem").clicked() {
                    ui.ctx().copy_text(msg.to_string());
                }
                if ui.button("Sair").clicked() {
                    sair = true;
                }
            });
        });
    sair
}

/// Contador de FPS no canto da tela.
pub fn fps_overlay(ctx: &egui::Context, fps: f32) {
    egui::Area::new(egui::Id::unique("yw_fps"))
        .anchor(Align2::LEFT_TOP, Vec2::new(6.0, 6.0))
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(egui::Color32::from_black_alpha(160))
                .corner_radius(4.0)
                .inner_margin(egui::Margin::symmetric(6, 2))
                .show(ui, |ui| {
                    ui.label(RichText::new(format!("{fps:.0} FPS")).monospace().color(egui::Color32::WHITE));
                });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captura_de_tecla() {
        let mut m = Menu::default();
        let mut s = Settings::default();
        assert!(!m.capture_key(&mut s, KeyCode::KeyP));
        m.captura = Some(Captura::Tecla(Botao::A, 0));
        assert!(m.capture_key(&mut s, KeyCode::F1)); // reservada: ainda esperando
        assert!(m.capture_key(&mut s, KeyCode::F11));
        assert!(m.captura.is_some());
        assert!(m.capture_key(&mut s, KeyCode::KeyP));
        assert_eq!(s.teclado.a[0], KeyCode::KeyP);
        assert!(m.captura.is_none());
        // Esc cancela sem mudar nada
        m.captura = Some(Captura::Tecla(Botao::B, 0));
        assert!(m.capture_key(&mut s, KeyCode::Escape));
        assert_eq!(s.teclado.b, Teclado::default().b);
    }

    #[test]
    fn esc_durante_captura_nao_fecha_o_menu() {
        let mut m = Menu { open: true, ..Menu::default() };
        m.captura = Some(Captura::Pad(Botao::A));
        m.toggle();
        assert!(m.open && m.captura.is_none());
        m.toggle();
        assert!(!m.open);
    }

    #[test]
    fn captura_de_botao_do_controle() {
        let mut m = Menu { captura: Some(Captura::Pad(Botao::Start)), ..Menu::default() };
        let mut s = Settings::default();
        assert!(m.capture_pad(&mut s, gilrs::Button::Mode));
        assert!(m.capture_pad(&mut s, gilrs::Button::RightThumb));
        assert_eq!(s.controle.start, gilrs::Button::RightThumb);
    }

    #[test]
    fn menu_desenha_sem_janela() {
        // roda o egui sem GPU, em todas as seções, para pegar panes de layout
        let ctx = egui::Context::default();
        let mut m = Menu { open: true, ..Menu::default() };
        let mut s = Settings::default();
        let info = Info {
            pads: vec!["Teste".into()],
            renderer_em_uso: Some(crate::render::Backend::Soft),
            ..Info::default()
        };
        for p in [Pagina::Principal, Pagina::Config] {
            for sec in Secao::ALL {
                m.pagina = p;
                m.secao = sec;
                let raw = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(400.0, 480.0))),
                    ..Default::default()
                };
                let _ = ctx.run_pass(raw, |ctx| {
                    assert_eq!(m.ui(ctx, &mut s, &info), Acao::Nada);
                    fps_overlay(ctx, 30.0);
                    assert!(!crash_window(ctx, "panic"));
                });
            }
        }
        assert_eq!(s, Settings::default());
    }
}
