//! Configurações da janela do jogo recompilado, guardadas em `%APPDATA%/yw1/settings.toml` (`YW_SETTINGS_TOML`
//! sobrescreve o caminho). Lidas no início, salvas quando mudam no menu e ao sair.
//!
//! As variáveis de ambiente antigas continuam valendo **por cima** do arquivo: `YW_RENDERER`, `YW_SCALE`,
//! `YW_PAD_LAYOUT` e `YW_VOLUME`. Um valor vindo do ambiente não é gravado no arquivo, a não ser que o usuário o
//! mude no menu (aí a escolha dele passa a valer e é salva).
//!
//! Ideia da organização (seções Gráficos/Som/Controles/Controle/Emulação, layouts de tela, remapeamento por botão
//! do 3DS) adaptada da interface do emulador Zakuro (MIT).

use gilrs::Button as Pad;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use winit::keyboard::KeyCode;

/// Renderizador do PICA (`render::backend()`); vale no próximo início.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Renderizador {
    #[default]
    Vulkan,
    Soft,
}

impl Renderizador {
    pub const ALL: [Renderizador; 2] = [Renderizador::Vulkan, Renderizador::Soft];
    pub fn nome(self) -> &'static str {
        match self {
            Renderizador::Vulkan => "Vulkan (GPU)",
            Renderizador::Soft => "Soft (CPU, referência)",
        }
    }
}

/// Como as duas telas ficam na janela.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    /// a de cima sobre a de baixo, como no console
    #[default]
    Empilhadas,
    /// lado a lado (a de cima à esquerda)
    LadoALado,
    /// só a de cima
    SoCima,
    /// só a de baixo
    SoBaixo,
    /// a de cima grande e a de baixo pequena (metade) no canto inferior direito
    GrandePequena,
}

impl Layout {
    pub const ALL: [Layout; 5] =
        [Layout::Empilhadas, Layout::LadoALado, Layout::GrandePequena, Layout::SoCima, Layout::SoBaixo];
    pub fn nome(self) -> &'static str {
        match self {
            Layout::Empilhadas => "Empilhadas (como no 3DS)",
            Layout::LadoALado => "Lado a lado",
            Layout::GrandePequena => "Tela grande + pequena",
            Layout::SoCima => "Só a tela de cima",
            Layout::SoBaixo => "Só a tela de baixo",
        }
    }
}

/// Filtro ao ampliar as telas.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Filtro {
    /// vizinho mais próximo: pixels quadrados
    #[default]
    Nitido,
    /// bilinear
    Suave,
}

impl Filtro {
    pub const ALL: [Filtro; 2] = [Filtro::Nitido, Filtro::Suave];
    pub fn nome(self) -> &'static str {
        match self {
            Filtro::Nitido => "Nítido (pixels)",
            Filtro::Suave => "Suave (bilinear)",
        }
    }
}

/// Idioma do console (bloco 0x000A0002 do `cfg:u`). O cartucho americano tem inglês, francês e espanhol.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Idioma {
    #[default]
    Ingles,
    Frances,
    Espanhol,
}

impl Idioma {
    pub const ALL: [Idioma; 3] = [Idioma::Ingles, Idioma::Frances, Idioma::Espanhol];
    pub fn nome(self) -> &'static str {
        match self {
            Idioma::Ingles => "Inglês",
            Idioma::Frances => "Francês",
            Idioma::Espanhol => "Espanhol",
        }
    }
    /// Código do idioma nos blocos de configuração do 3DS (3dbrew "Config Savegame").
    pub fn codigo(self) -> u8 {
        match self {
            Idioma::Ingles => 1,
            Idioma::Frances => 2,
            Idioma::Espanhol => 5,
        }
    }
}

/// Disposição dos botões de face do controle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PadLayout {
    /// confirmar (A) no botão de baixo, como num controle de Xbox
    #[default]
    Xbox,
    /// A no botão da direita, como no 3DS / Switch
    Nintendo,
}

impl PadLayout {
    pub const ALL: [PadLayout; 2] = [PadLayout::Xbox, PadLayout::Nintendo];
    pub fn nome(self) -> &'static str {
        match self {
            PadLayout::Xbox => "Xbox (A no botão de baixo)",
            PadLayout::Nintendo => "Nintendo (A no botão da direita, como no 3DS)",
        }
    }
}

/// Entradas do 3DS que podem ser remapeadas. ZL/ZR ficam de fora: são do New 3DS (via `ir:rst`, que o HLE não
/// tem) e o Yo-kai Watch 1 é jogo do 3DS antigo, então não teriam efeito.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Botao {
    A,
    B,
    X,
    Y,
    L,
    R,
    Start,
    Select,
    Cima,
    Baixo,
    Esquerda,
    Direita,
    AnalogicoCima,
    AnalogicoBaixo,
    AnalogicoEsquerda,
    AnalogicoDireita,
}

impl Botao {
    /// Todas as entradas, na ordem em que aparecem no menu (teclado).
    pub const ALL: [Botao; 16] = [
        Botao::A,
        Botao::B,
        Botao::X,
        Botao::Y,
        Botao::L,
        Botao::R,
        Botao::Start,
        Botao::Select,
        Botao::Cima,
        Botao::Baixo,
        Botao::Esquerda,
        Botao::Direita,
        Botao::AnalogicoCima,
        Botao::AnalogicoBaixo,
        Botao::AnalogicoEsquerda,
        Botao::AnalogicoDireita,
    ];
    /// As que existem como botão no controle (o Circle Pad é o analógico esquerdo).
    pub const PAD: [Botao; 12] = [
        Botao::A,
        Botao::B,
        Botao::X,
        Botao::Y,
        Botao::L,
        Botao::R,
        Botao::Start,
        Botao::Select,
        Botao::Cima,
        Botao::Baixo,
        Botao::Esquerda,
        Botao::Direita,
    ];

    pub fn nome(self) -> &'static str {
        match self {
            Botao::A => "A",
            Botao::B => "B",
            Botao::X => "X",
            Botao::Y => "Y",
            Botao::L => "L",
            Botao::R => "R",
            Botao::Start => "Start",
            Botao::Select => "Select",
            Botao::Cima => "Direcional ↑",
            Botao::Baixo => "Direcional ↓",
            Botao::Esquerda => "Direcional ←",
            Botao::Direita => "Direcional →",
            Botao::AnalogicoCima => "Circle Pad ↑",
            Botao::AnalogicoBaixo => "Circle Pad ↓",
            Botao::AnalogicoEsquerda => "Circle Pad ←",
            Botao::AnalogicoDireita => "Circle Pad →",
        }
    }

    /// Bit no estado de botões do HID (`services::hid`); o Circle Pad não tem bit próprio (vai pelo analógico).
    pub fn bit(self) -> Option<u32> {
        Some(match self {
            Botao::A => 1 << 0,
            Botao::B => 1 << 1,
            Botao::Select => 1 << 2,
            Botao::Start => 1 << 3,
            Botao::Direita => 1 << 4,
            Botao::Esquerda => 1 << 5,
            Botao::Cima => 1 << 6,
            Botao::Baixo => 1 << 7,
            Botao::R => 1 << 8,
            Botao::L => 1 << 9,
            Botao::X => 1 << 10,
            Botao::Y => 1 << 11,
            _ => return None,
        })
    }
}

/// Teclas de cada entrada (várias por entrada, como no motor antigo: Z, Espaço e K são todas A).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Teclado {
    pub a: Vec<KeyCode>,
    pub b: Vec<KeyCode>,
    pub x: Vec<KeyCode>,
    pub y: Vec<KeyCode>,
    pub l: Vec<KeyCode>,
    pub r: Vec<KeyCode>,
    pub start: Vec<KeyCode>,
    pub select: Vec<KeyCode>,
    pub cima: Vec<KeyCode>,
    pub baixo: Vec<KeyCode>,
    pub esquerda: Vec<KeyCode>,
    pub direita: Vec<KeyCode>,
    pub analogico_cima: Vec<KeyCode>,
    pub analogico_baixo: Vec<KeyCode>,
    pub analogico_esquerda: Vec<KeyCode>,
    pub analogico_direita: Vec<KeyCode>,
}

impl Default for Teclado {
    /// Os mesmos atalhos da janela anterior (minifb): setas = direcional, WASD = Circle Pad, Z/Espaço/K = A,
    /// X/Backspace/L = B, C/I = X, V/J = Y, Q = L, E = R, Enter = Start, Tab/Shift direito = Select.
    fn default() -> Self {
        use KeyCode::*;
        Teclado {
            a: vec![KeyZ, Space, KeyK],
            b: vec![KeyX, Backspace, KeyL],
            x: vec![KeyC, KeyI],
            y: vec![KeyV, KeyJ],
            l: vec![KeyQ],
            r: vec![KeyE],
            start: vec![Enter],
            select: vec![Tab, ShiftRight],
            cima: vec![ArrowUp],
            baixo: vec![ArrowDown],
            esquerda: vec![ArrowLeft],
            direita: vec![ArrowRight],
            analogico_cima: vec![KeyW],
            analogico_baixo: vec![KeyS],
            analogico_esquerda: vec![KeyA],
            analogico_direita: vec![KeyD],
        }
    }
}

impl Teclado {
    pub fn get(&self, b: Botao) -> &Vec<KeyCode> {
        match b {
            Botao::A => &self.a,
            Botao::B => &self.b,
            Botao::X => &self.x,
            Botao::Y => &self.y,
            Botao::L => &self.l,
            Botao::R => &self.r,
            Botao::Start => &self.start,
            Botao::Select => &self.select,
            Botao::Cima => &self.cima,
            Botao::Baixo => &self.baixo,
            Botao::Esquerda => &self.esquerda,
            Botao::Direita => &self.direita,
            Botao::AnalogicoCima => &self.analogico_cima,
            Botao::AnalogicoBaixo => &self.analogico_baixo,
            Botao::AnalogicoEsquerda => &self.analogico_esquerda,
            Botao::AnalogicoDireita => &self.analogico_direita,
        }
    }

    pub fn get_mut(&mut self, b: Botao) -> &mut Vec<KeyCode> {
        match b {
            Botao::A => &mut self.a,
            Botao::B => &mut self.b,
            Botao::X => &mut self.x,
            Botao::Y => &mut self.y,
            Botao::L => &mut self.l,
            Botao::R => &mut self.r,
            Botao::Start => &mut self.start,
            Botao::Select => &mut self.select,
            Botao::Cima => &mut self.cima,
            Botao::Baixo => &mut self.baixo,
            Botao::Esquerda => &mut self.esquerda,
            Botao::Direita => &mut self.direita,
            Botao::AnalogicoCima => &mut self.analogico_cima,
            Botao::AnalogicoBaixo => &mut self.analogico_baixo,
            Botao::AnalogicoEsquerda => &mut self.analogico_esquerda,
            Botao::AnalogicoDireita => &mut self.analogico_direita,
        }
    }

    /// Põe `key` em `b` (na posição `slot`, ou no fim se `slot` passar do tamanho) e tira essa tecla de qualquer
    /// outra entrada, para uma tecla nunca apertar duas coisas.
    pub fn assign(&mut self, b: Botao, slot: usize, key: KeyCode) {
        for o in Botao::ALL {
            if o != b {
                self.get_mut(o).retain(|k| *k != key);
            }
        }
        let v = self.get_mut(b);
        match v.iter().position(|k| *k == key) {
            // a tecla já está nesta entrada: só muda de posição (a tecla clicada some, as outras ficam)
            Some(i) if i == slot => {}
            Some(i) if slot < v.len() => {
                v[slot] = key;
                v.remove(i);
            }
            Some(_) => {} // "+" com uma tecla que já está na lista: nada muda
            None if slot < v.len() => v[slot] = key,
            None => v.push(key),
        }
    }
}

/// Configuração do controle (gilrs): disposição e um botão por entrada do 3DS.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Controle {
    pub layout: PadLayout,
    /// quanto o analógico precisa inclinar para contar (0–0,9)
    pub zona_morta: f32,
    pub a: Pad,
    pub b: Pad,
    pub x: Pad,
    pub y: Pad,
    pub l: Pad,
    pub r: Pad,
    pub start: Pad,
    pub select: Pad,
    pub cima: Pad,
    pub baixo: Pad,
    pub esquerda: Pad,
    pub direita: Pad,
}

impl Default for Controle {
    fn default() -> Self {
        Controle::preset(PadLayout::Xbox)
    }
}

impl Controle {
    /// Mapeamento padrão de cada disposição (o mesmo da janela anterior com/sem `YW_PAD_LAYOUT=nintendo`).
    pub fn preset(layout: PadLayout) -> Self {
        let (a, b, x, y) = match layout {
            PadLayout::Xbox => (Pad::South, Pad::East, Pad::North, Pad::West),
            PadLayout::Nintendo => (Pad::East, Pad::South, Pad::West, Pad::North),
        };
        Controle {
            layout,
            zona_morta: 0.2,
            a,
            b,
            x,
            y,
            l: Pad::LeftTrigger,
            r: Pad::RightTrigger,
            start: Pad::Start,
            select: Pad::Select,
            cima: Pad::DPadUp,
            baixo: Pad::DPadDown,
            esquerda: Pad::DPadLeft,
            direita: Pad::DPadRight,
        }
    }

    /// Troca a disposição: os quatro botões de face voltam ao padrão dela (com troca, como em `assign`, para nenhum
    /// botão do controle ficar em duas entradas); o resto fica como está.
    pub fn set_layout(&mut self, layout: PadLayout) {
        let p = Controle::preset(layout);
        self.layout = layout;
        for (b, pad) in [(Botao::A, p.a), (Botao::B, p.b), (Botao::X, p.x), (Botao::Y, p.y)] {
            self.assign(b, pad);
        }
    }

    pub fn get(&self, b: Botao) -> Option<Pad> {
        Some(match b {
            Botao::A => self.a,
            Botao::B => self.b,
            Botao::X => self.x,
            Botao::Y => self.y,
            Botao::L => self.l,
            Botao::R => self.r,
            Botao::Start => self.start,
            Botao::Select => self.select,
            Botao::Cima => self.cima,
            Botao::Baixo => self.baixo,
            Botao::Esquerda => self.esquerda,
            Botao::Direita => self.direita,
            _ => return None,
        })
    }

    fn slot(&mut self, b: Botao) -> Option<&mut Pad> {
        Some(match b {
            Botao::A => &mut self.a,
            Botao::B => &mut self.b,
            Botao::X => &mut self.x,
            Botao::Y => &mut self.y,
            Botao::L => &mut self.l,
            Botao::R => &mut self.r,
            Botao::Start => &mut self.start,
            Botao::Select => &mut self.select,
            Botao::Cima => &mut self.cima,
            Botao::Baixo => &mut self.baixo,
            Botao::Esquerda => &mut self.esquerda,
            Botao::Direita => &mut self.direita,
            _ => return None,
        })
    }

    /// Põe `pad` em `b`; se outra entrada usava esse botão, ela fica com o botão antigo de `b` (troca).
    pub fn assign(&mut self, b: Botao, pad: Pad) {
        let Some(old) = self.get(b) else { return };
        for o in Botao::PAD {
            if o != b && self.get(o) == Some(pad) {
                *self.slot(o).unwrap() = old;
            }
        }
        *self.slot(b).unwrap() = pad;
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Graficos {
    pub renderizador: Renderizador,
    pub layout: Layout,
    pub filtro: Filtro,
    /// amplia só por números inteiros (pixels todos do mesmo tamanho)
    pub escala_inteira: bool,
    /// tamanho inicial da janela: múltiplo do tamanho do 3DS no layout escolhido (1–4)
    pub escala_janela: u32,
    pub mostrar_fps: bool,
    /// tela cheia sem borda (F11)
    pub tela_cheia: bool,
}

impl Default for Graficos {
    fn default() -> Self {
        Graficos {
            renderizador: Renderizador::Vulkan,
            layout: Layout::Empilhadas,
            filtro: Filtro::Nitido,
            escala_inteira: false,
            escala_janela: 2,
            mostrar_fps: false,
            tela_cheia: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Som {
    /// 0–100
    pub volume: u32,
    pub mudo: bool,
}

impl Default for Som {
    fn default() -> Self {
        Som { volume: 100, mudo: false }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Emulacao {
    /// idioma do console; vale no próximo início
    pub idioma: Idioma,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub graficos: Graficos,
    pub som: Som,
    pub teclado: Teclado,
    pub controle: Controle,
    pub emulacao: Emulacao,
}

impl Settings {
    /// Lê de TOML; campos que faltam ficam no padrão. Valores fora da faixa são corrigidos.
    pub fn from_toml(s: &str) -> Result<Settings, String> {
        Settings::from_toml_report(s).map(|(v, _)| v)
    }

    /// Como `from_toml`, mas tolerante **por campo**: um valor que não serve (tecla escrita errado, layout de outra
    /// versão…) volta ao padrão só naquele campo; numa lista (teclas), só o item ruim cai. Devolve também o que foi
    /// descartado. Só é erro se o TOML em si não for válido.
    pub fn from_toml_report(s: &str) -> Result<(Settings, Vec<String>), String> {
        use toml::{Table, Value};
        let file: Table = toml::from_str(s).map_err(|e| e.to_string())?;
        let mut base = Table::try_from(Settings::default()).map_err(|e| e.to_string())?;
        let mut descartados = Vec::new();
        let ok = |t: &Table| Value::Table(t.clone()).try_into::<Settings>().is_ok();
        // põe `v` em base[sec][campo] (ou base[sec]) se o resultado ainda for uma configuração válida
        let tenta = |base: &mut Table, sec: &str, campo: Option<&str>, v: Value| -> bool {
            let mut c = base.clone();
            match campo {
                None => {
                    c.insert(sec.to_string(), v);
                }
                Some(k) => match c.get_mut(sec) {
                    Some(Value::Table(t)) => {
                        t.insert(k.to_string(), v);
                    }
                    _ => return false,
                },
            }
            let bom = ok(&c);
            if bom {
                *base = c;
            }
            bom
        };
        for (sec, v) in file {
            let tabela_padrao = matches!(base.get(&sec), Some(Value::Table(_)));
            match v {
                Value::Table(campos) if tabela_padrao => {
                    for (k, fv) in campos {
                        if tenta(&mut base, &sec, Some(&k), fv.clone()) {
                            continue;
                        }
                        match fv {
                            // lista: guarda os itens que servem, um a um
                            Value::Array(itens) => {
                                let mut aceitos = Vec::new();
                                for it in itens {
                                    let mut cand = aceitos.clone();
                                    cand.push(it.clone());
                                    if tenta(&mut base, &sec, Some(&k), Value::Array(cand.clone())) {
                                        aceitos = cand;
                                    } else {
                                        descartados.push(format!("{sec}.{k}: item {it}"));
                                    }
                                }
                                // (nenhum item serviu: a lista fica no padrão)
                            }
                            fv => descartados.push(format!("{sec}.{k} = {fv}")),
                        }
                    }
                }
                v if tabela_padrao => descartados.push(format!("{sec} = {v}")),
                // chave desconhecida (de outra versão): ignorada
                _ => {}
            }
        }
        let mut v: Settings = Value::Table(base).try_into().map_err(|e: toml::de::Error| e.to_string())?;
        v.clamp();
        Ok((v, descartados))
    }

    pub fn to_toml(&self) -> String {
        let body = toml::to_string_pretty(self).unwrap_or_default();
        format!(
            "# Configurações da janela do Yo-kai Watch recompilado (gravado pelo menu; Esc/F1 no jogo).\n\
             # Teclas: nomes do winit (KeyZ, Space, ArrowUp, ShiftRight…); botões do controle: nomes do gilrs.\n\n{body}"
        )
    }

    pub fn clamp(&mut self) {
        self.graficos.escala_janela = self.graficos.escala_janela.clamp(1, 4);
        self.som.volume = self.som.volume.min(100);
        let z = self.controle.zona_morta;
        self.controle.zona_morta = if z.is_finite() { z.clamp(0.0, 0.9) } else { 0.2 };
    }
}

/// Caminho do arquivo: `YW_SETTINGS_TOML`, senão `%APPDATA%/yw1/settings.toml` (sem APPDATA: pasta atual).
pub fn path() -> PathBuf {
    if let Some(p) = std::env::var_os("YW_SETTINGS_TOML") {
        return PathBuf::from(p);
    }
    std::env::var_os("APPDATA")
        .map(|a| PathBuf::from(a).join("yw1"))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("settings.toml")
}

/// Lê o arquivo. Sem arquivo → padrão, em silêncio. Valores que não servem → padrão só naqueles campos. TOML
/// inválido ou arquivo ilegível → padrão. Em todos os casos com perda há aviso no terminal e o arquivo original é
/// copiado para `.toml.bak` antes que o próximo `save()` o sobrescreva.
pub fn load() -> Settings {
    let p = path();
    let bak = || {
        let b = p.with_extension("toml.bak");
        match std::fs::copy(&p, &b) {
            Ok(_) => format!("cópia em {}", b.display()),
            Err(e) => format!("não consegui copiar para {}: {e}", b.display()),
        }
    };
    let bytes = match std::fs::read(&p) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Settings::default(),
        Err(e) => {
            eprintln!("[config] não consegui ler {} ({e}); usando o padrão ({})", p.display(), bak());
            return Settings::default();
        }
    };
    let Ok(s) = String::from_utf8(bytes) else {
        eprintln!("[config] {} não é texto UTF-8; usando o padrão ({})", p.display(), bak());
        return Settings::default();
    };
    match Settings::from_toml_report(&s) {
        Ok((v, descartados)) => {
            if !descartados.is_empty() {
                eprintln!(
                    "[config] {}: valores ignorados (voltaram ao padrão): {} ({})",
                    p.display(),
                    descartados.join("; "),
                    bak()
                );
            }
            v
        }
        Err(e) => {
            eprintln!("[config] {} inválido ({e}); usando o padrão ({})", p.display(), bak());
            Settings::default()
        }
    }
}

pub fn save(s: &Settings) {
    let p = path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    // grava num temporário e renomeia: um arquivo cortado no meio não perde a configuração inteira
    let tmp = p.with_extension("toml.tmp");
    let ok = std::fs::write(&tmp, s.to_toml()).and_then(|_| std::fs::rename(&tmp, &p));
    if let Err(e) = ok {
        eprintln!("[config] não consegui salvar {}: {e}", p.display());
    }
}

/// Valores vindos das variáveis de ambiente antigas (valem por cima do arquivo).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EnvOverrides {
    /// `YW_RENDERER` está definido (o próprio `render::backend()` o lê; o arquivo não manda)
    pub renderer: Option<Renderizador>,
    pub renderer_set: bool,
    pub scale: Option<u32>,
    pub pad_layout: Option<PadLayout>,
    pub volume: Option<u32>,
}

impl EnvOverrides {
    pub fn from_env() -> Self {
        Self::parse(|k| std::env::var(k).ok())
    }

    /// Interpreta as variáveis (função separada para testar sem mexer no ambiente do processo).
    pub fn parse(get: impl Fn(&str) -> Option<String>) -> Self {
        let r = get("YW_RENDERER");
        EnvOverrides {
            renderer: match r.as_deref() {
                Some("vulkan") | Some("vk") => Some(Renderizador::Vulkan),
                Some("soft") => Some(Renderizador::Soft),
                _ => None,
            },
            renderer_set: r.is_some(),
            scale: get("YW_SCALE").and_then(|v| v.trim().parse::<u32>().ok()).map(|v| v.clamp(1, 4)),
            pad_layout: get("YW_PAD_LAYOUT").map(|v| {
                if v.eq_ignore_ascii_case("nintendo") {
                    PadLayout::Nintendo
                } else {
                    PadLayout::Xbox
                }
            }),
            volume: get("YW_VOLUME")
                .and_then(|v| v.trim().parse::<f32>().ok())
                .map(|v| v.clamp(0.0, 100.0).round() as u32),
        }
    }

    /// Configuração em uso = arquivo + ambiente.
    pub fn apply(&self, file: &Settings) -> Settings {
        let mut s = file.clone();
        if let Some(r) = self.renderer {
            s.graficos.renderizador = r;
        }
        if let Some(v) = self.scale {
            s.graficos.escala_janela = v;
        }
        if let Some(l) = self.pad_layout {
            if s.controle.layout != l {
                s.controle.set_layout(l);
            }
        }
        if let Some(v) = self.volume {
            s.som.volume = v;
        }
        s
    }

    /// O que gravar: a configuração em uso, mas cada valor que veio do ambiente e não foi mexido no menu volta ao
    /// valor do arquivo (o ambiente é só desta execução).
    pub fn to_save(&self, current: &Settings, file: &Settings) -> Settings {
        let applied = self.apply(file);
        let mut s = current.clone();
        if self.renderer.is_some() && current.graficos.renderizador == applied.graficos.renderizador {
            s.graficos.renderizador = file.graficos.renderizador;
        }
        if self.scale.is_some() && current.graficos.escala_janela == applied.graficos.escala_janela {
            s.graficos.escala_janela = file.graficos.escala_janela;
        }
        if self.pad_layout.is_some()
            && current.controle.layout == applied.controle.layout
            && (current.controle.a, current.controle.b, current.controle.x, current.controle.y)
                == (applied.controle.a, applied.controle.b, applied.controle.x, applied.controle.y)
        {
            let c = &file.controle;
            (s.controle.layout, s.controle.a, s.controle.b, s.controle.x, s.controle.y) =
                (c.layout, c.a, c.b, c.x, c.y);
        }
        if self.volume.is_some() && current.som.volume == applied.som.volume {
            s.som.volume = file.som.volume;
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_ida_e_volta() {
        let mut s = Settings::default();
        s.graficos.layout = Layout::GrandePequena;
        s.graficos.filtro = Filtro::Suave;
        s.graficos.escala_inteira = true;
        s.graficos.renderizador = Renderizador::Soft;
        s.graficos.mostrar_fps = true;
        s.graficos.tela_cheia = true;
        s.som = Som { volume: 37, mudo: true };
        s.teclado.assign(Botao::A, 0, KeyCode::KeyP);
        s.controle.set_layout(PadLayout::Nintendo);
        s.controle.assign(Botao::L, Pad::LeftThumb);
        s.emulacao.idioma = Idioma::Espanhol;
        let txt = s.to_toml();
        assert!(txt.contains("[graficos]") && txt.contains("layout = \"grande_pequena\""), "{txt}");
        assert!(txt.contains("KeyP"), "{txt}");
        assert_eq!(Settings::from_toml(&txt).unwrap(), s);
    }

    #[test]
    fn padroes() {
        let s = Settings::default();
        assert_eq!(s.graficos.renderizador, Renderizador::Vulkan);
        assert_eq!(s.graficos.layout, Layout::Empilhadas);
        assert_eq!(s.graficos.escala_janela, 2);
        assert_eq!(s.som.volume, 100);
        assert!(!s.som.mudo);
        assert_eq!(s.controle.layout, PadLayout::Xbox);
        assert_eq!(s.controle.a, Pad::South);
        assert_eq!(s.emulacao.idioma.codigo(), 1);
        // arquivo vazio ou parcial = padrão nos campos que faltam
        assert_eq!(Settings::from_toml("").unwrap(), s);
        let p = Settings::from_toml("[som]\nvolume = 250\n[graficos]\nescala_janela = 9\n").unwrap();
        assert_eq!(p.som.volume, 100);
        assert_eq!(p.graficos.escala_janela, 4);
        assert_eq!(p.teclado, Teclado::default());
        // TOML que nem é TOML: erro (vira .bak no load)
        assert!(Settings::from_toml("graficos = [").is_err());
    }

    #[test]
    fn valor_ruim_volta_ao_padrao_so_naquele_campo() {
        let txt = "[graficos]
layout = \"crt\"
filtro = \"suave\"
escala_janela = \"grande\"
                   [teclado]
a = [\"KeyP\", \"TeclaQueNaoExiste\", \"KeyO\"]
b = [\"Nada\"]
                   [controle]
layout = \"playstation\"
start = \"RightThumb\"
                   [futuro]
qualquer = 1
";
        let (s, desc) = Settings::from_toml_report(txt).unwrap();
        let d = Settings::default();
        assert_eq!(s.graficos.layout, d.graficos.layout);
        assert_eq!(s.graficos.filtro, Filtro::Suave); // o vizinho bom vale
        assert_eq!(s.graficos.escala_janela, d.graficos.escala_janela);
        assert_eq!(s.teclado.a, vec![KeyCode::KeyP, KeyCode::KeyO]); // só o item ruim cai
        assert_eq!(s.teclado.b, d.teclado.b); // nenhum item serviu: padrão
        assert_eq!(s.controle.layout, PadLayout::Xbox);
        assert_eq!(s.controle.start, Pad::RightThumb);
        assert_eq!(desc.len(), 5, "{desc:?}");
        // seção que devia ser tabela mas não é: padrão, sem erro
        assert_eq!(Settings::from_toml("graficos = 3").unwrap(), d);
    }

    #[test]
    fn teclas_padrao_iguais_a_janela_antiga() {
        let t = Teclado::default();
        assert_eq!(t.a, vec![KeyCode::KeyZ, KeyCode::Space, KeyCode::KeyK]);
        assert_eq!(t.b, vec![KeyCode::KeyX, KeyCode::Backspace, KeyCode::KeyL]);
        assert_eq!(t.start, vec![KeyCode::Enter]);
        assert_eq!(t.analogico_esquerda, vec![KeyCode::KeyA]);
        // nenhuma tecla em duas entradas e Esc/F1 (menu) livres
        let mut all: Vec<KeyCode> = Botao::ALL.iter().flat_map(|b| t.get(*b).clone()).collect();
        let n = all.len();
        all.sort_by_key(|k| format!("{k:?}"));
        all.dedup();
        assert_eq!(all.len(), n);
        assert!(!all.contains(&KeyCode::Escape) && !all.contains(&KeyCode::F1) && !all.contains(&KeyCode::F11));
    }

    #[test]
    fn remapear_tecla_tira_de_outra_entrada() {
        let mut t = Teclado::default();
        t.assign(Botao::Y, 0, KeyCode::KeyZ); // Z era de A
        assert_eq!(t.y, vec![KeyCode::KeyZ, KeyCode::KeyJ]);
        assert_eq!(t.a, vec![KeyCode::Space, KeyCode::KeyK]);
        t.assign(Botao::L, 5, KeyCode::Digit1); // posição além do fim: acrescenta
        assert_eq!(t.l, vec![KeyCode::KeyQ, KeyCode::Digit1]);
    }

    #[test]
    fn remapear_com_a_tecla_ja_na_mesma_entrada() {
        use KeyCode::*;
        // A = [Z, Espaço, K]
        let mut t = Teclado::default();
        t.assign(Botao::A, 1, KeyZ); // clicou em Espaço, apertou Z (que vem antes)
        assert_eq!(t.a, vec![KeyZ, KeyK]); // Espaço trocado, K fica
        let mut t = Teclado::default();
        t.assign(Botao::A, 2, KeyZ); // clicou em K, apertou Z
        assert_eq!(t.a, vec![Space, KeyZ]);
        let mut t = Teclado::default();
        t.assign(Botao::A, 0, KeyK); // clicou em Z, apertou K (que vem depois)
        assert_eq!(t.a, vec![KeyK, Space]);
        let mut t = Teclado::default();
        t.assign(Botao::A, 1, Space); // a mesma tecla na mesma posição: nada muda
        assert_eq!(t.a, Teclado::default().a);
        let mut t = Teclado::default();
        t.assign(Botao::A, 3, KeyK); // "+" com tecla que já está: nada muda
        assert_eq!(t.a, Teclado::default().a);
    }

    #[test]
    fn remapear_botao_do_controle_troca() {
        let mut c = Controle::default();
        c.assign(Botao::A, Pad::East);
        assert_eq!((c.a, c.b), (Pad::East, Pad::South));
        c.set_layout(PadLayout::Nintendo);
        assert_eq!((c.a, c.b, c.x, c.y), (Pad::East, Pad::South, Pad::West, Pad::North));
        assert_eq!(c.l, Pad::LeftTrigger);
    }

    #[test]
    fn trocar_disposicao_nao_duplica_botao() {
        let mut c = Controle::default();
        c.assign(Botao::L, Pad::South); // L no botão de baixo; A vai para o LB
        assert_eq!((c.l, c.a), (Pad::South, Pad::LeftTrigger));
        c.set_layout(PadLayout::Xbox);
        assert_eq!(c.a, Pad::South);
        assert_eq!(c.l, Pad::LeftTrigger);
        let mut usados: Vec<String> = Botao::PAD.iter().map(|b| format!("{:?}", c.get(*b))).collect();
        usados.sort();
        usados.dedup();
        assert_eq!(usados.len(), Botao::PAD.len());
    }

    #[test]
    fn ambiente_por_cima_e_nao_gravado() {
        let env = EnvOverrides::parse(|k| match k {
            "YW_SCALE" => Some("4".into()),
            "YW_VOLUME" => Some("30".into()),
            "YW_PAD_LAYOUT" => Some("nintendo".into()),
            "YW_RENDERER" => Some("soft".into()),
            _ => None,
        });
        let file = Settings::default();
        let cur = env.apply(&file);
        assert_eq!(cur.graficos.escala_janela, 4);
        assert_eq!(cur.som.volume, 30);
        assert_eq!(cur.controle.a, Pad::East);
        assert_eq!(cur.graficos.renderizador, Renderizador::Soft);
        // sem mexer: grava o que estava no arquivo
        assert_eq!(env.to_save(&cur, &file), file);
        // o usuário mudou o volume no menu: essa escolha é gravada
        let mut changed = cur.clone();
        changed.som.volume = 55;
        let saved = env.to_save(&changed, &file);
        assert_eq!(saved.som.volume, 55);
        assert_eq!(saved.graficos.escala_janela, 2);
        assert_eq!(EnvOverrides::parse(|_| None), EnvOverrides::default());
    }
}
