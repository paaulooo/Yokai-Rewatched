//! Host dos scripts de menu (`seq/menu/*.xq`): implementa os opcodes nativos do motor que os menus usam —
//! recursos de layout, objetos 2D (flipbooks do ANMC), botões em matriz (`op2110` 1000..1033), música,
//! entrada e troca de cena — em cima de `layout.rs`. Os NOMES/semânticas vêm do catálogo
//! `work/notes/xq32_opcodes.md` e do uso nos scripts; quase tudo é HIPÓTESE (marcado `[H]`), validada
//! só pelo comportamento: o menu tem de montar e andar como no 3DS.
//!
//! Não implementado ainda: texto (fontes `fnt/*.xf` não decifradas → `op59xx` ignorados), janelas de
//! mensagem (respondidas automaticamente com "sim"), efeitos sonoros, modelos 3D de menu (`op37xx`/`op6xxx`).

use crate::assets::arc0::Archive;
use crate::assets::font::{self, Font};
use crate::assets::{cfgbin, crc32, Result};
use crate::input::{Button, Input};
use crate::layout::{Anim, Layout};
use crate::render2d::Target;
use crate::screen::{Screens, BOTTOM_H, BOTTOM_W, TOP_H, TOP_W};
use crate::xq::{Call, Host, Native, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

/// Movimento em andamento: (origem, destino, quadros, quadro atual).
type Tween = ((f32, f32), (f32, f32), u32, u32);

struct Obj {
    name: String,
    screen: u8,
    prio: i32,
    order: usize,
    res: Option<u32>,
    anim: Option<Anim>,
    anim_name: String,
    visible: bool,
    alpha: f32,
    alpha_to: Option<(f32, f32, u32, u32)>,
    pos: (f32, f32),
    move_to: Option<Tween>,
    parent: Option<u32>,
    params: HashMap<i32, Value>,
    create_args: Vec<Value>,
    text: Option<String>,
    small_font: bool,
    align: i32,
    text_box: Option<(f32, f32)>,
    /// `op5944(obj, 1)`: o texto termina em `x` (ancorado à direita) — os valores dos contadores do diário.
    anchor_right: bool,
    /// `op5932(obj, r, g, b)`: cor do texto (o diário usa um marrom).
    color: Option<u32>,
}

#[derive(Default)]
struct Matrix {
    cols: i32,
    rows: i32,
    select_cb: Option<Value>,
    cells: HashMap<i32, u32>,
    focus: i32,
    locked: bool,
}

/// Pedido de troca de cena (`op2104`).
#[derive(Debug, Clone)]
pub struct SceneRequest {
    pub id: i32,
    pub args: Vec<Value>,
}

pub struct MenuHost {
    fa_layouts: HashMap<u32, Layout>,
    objs: HashMap<u32, Obj>,
    next_order: usize,
    matrix: Matrix,
    /// Botões/estado de entrada deste quadro.
    held: u16,
    pressed: u16,
    pub scene_args: Vec<Value>,
    pub request: Option<SceneRequest>,
    /// Música: handle → caminho; a reprodução fica com o `main` (cpal).
    pub bgm_names: HashMap<i32, String>,
    pub bgm_play: Option<String>,
    pub bgm_stop: bool,
    current_bgm: i32,
    pub player_name: Option<String>,
    /// O script pediu para gravar (`SaveData` → `op12003`).
    pub save_requested: bool,
    pub player_type: i32,
    romfs: PathBuf,
    fa: Rc<Archive>,
    pub unknown: std::collections::BTreeMap<String, usize>,
    fonts: [Option<Font>; 2],
    /// Diários 1..3 (índice 0 = diário 1).
    pub slots: [Option<crate::save::Save>; crate::save::SLOTS],
    /// Diário escolhido (`op12050`, 1..3).
    pub current_slot: usize,
    texts: HashMap<String, HashMap<u32, String>>,
    /// Cenário 3D do menu (prévia do personagem etc.).
    m3d: crate::menu3d::Menu3d,
}

/// Índices de `op1103(n)` → botões. HIPÓTESE: bits do HID do 3DS (0 A, 1 B, 2 Select, 3 Start, 4 →, 5 ←, 6 ↑,
/// 7 ↓, 8 R, 9 L, 10 X, 11 Y), coerente com os grupos 0..3/10/11 dos scripts.
const HID: [Button; 12] = [
    Button::A,
    Button::B,
    Button::Select,
    Button::Start,
    Button::Right,
    Button::Left,
    Button::Up,
    Button::Down,
    Button::R,
    Button::L,
    Button::X,
    Button::Y,
];

impl MenuHost {
    pub fn new(fa: Rc<Archive>, romfs: PathBuf, scene_args: Vec<Value>) -> MenuHost {
        MenuHost {
            fa_layouts: HashMap::new(),
            objs: HashMap::new(),
            next_order: 0,
            matrix: Matrix::default(),
            held: 0,
            pressed: 0,
            scene_args,
            request: None,
            bgm_names: HashMap::new(),
            bgm_play: None,
            bgm_stop: false,
            current_bgm: 0,
            player_name: None,
            save_requested: false,
            player_type: 1,
            romfs,
            fa,
            unknown: Default::default(),
            fonts: [None, None],
            slots: [1, 2, 3].map(crate::save::load),
            current_slot: 1,
            texts: HashMap::new(),
            m3d: crate::menu3d::Menu3d::default(),
        }
    }

    fn fa(&self) -> &Archive {
        &self.fa
    }

    /// `op13733(tabela, chave)`: `data/res/text/**/<tabela>_en.cfg.bin`, entradas `TEXT_INFO [crc, índice, texto, flag]`.
    fn text(&mut self, table: &str, key: u32) -> Option<String> {
        if !self.texts.contains_key(table) {
            let mut map = HashMap::new();
            for dir in ["data/res/text/menu", "data/res/text"] {
                let Ok(bytes) = self.fa().read(&format!("{dir}/{table}_en.cfg.bin")) else {
                    continue;
                };
                for e in cfgbin::parse(&bytes).unwrap_or_default() {
                    if e.name == "TEXT_INFO" {
                        if let (Some(cfgbin::Param::Int(h)), Some(cfgbin::Param::Str(t))) =
                            (e.params.first(), e.params.get(2))
                        {
                            map.insert(*h as u32, t.clone());
                        }
                    }
                }
                break;
            }
            self.texts.insert(table.to_string(), map);
        }
        self.texts.get(table)?.get(&key).cloned()
    }

    /// Texto avulso do port (ex.: nome digitado). `top` = tela de cima.
    pub fn draw_text(
        &self,
        screens: &mut Screens,
        top: bool,
        pos: (f32, f32),
        text: &str,
        rgb: u32,
    ) {
        let Some(f) = self.font(false) else { return };
        let (px, w, h) = if top {
            (&mut screens.top, TOP_W, TOP_H)
        } else {
            (&mut screens.bottom, BOTTOM_W, BOTTOM_H)
        };
        f.draw(px, w, h, pos, text, rgb, 1.0, 1.0);
    }

    fn font(&self, small: bool) -> Option<&Font> {
        self.fonts[small as usize].as_ref()
    }

    pub fn load_fonts(&mut self) {
        self.ensure_fonts();
    }

    fn ensure_fonts(&mut self) {
        if self.fonts[0].is_none() {
            for (i, path) in ["fnt/ft_nrm.xf", "fnt/ft_sml.xf"].iter().enumerate() {
                match self.fa().read(path).and_then(|b| Font::parse(&b)) {
                    Ok(f) => self.fonts[i] = Some(f),
                    Err(e) => eprintln!("[menu] fonte {path}: {e}"),
                }
            }
        }
    }

    fn load_layout(&mut self, key: u32, path: &str) -> Result<()> {
        let candidates = [format!("{path}_en.xa"), format!("{path}.xa")];
        for c in &candidates {
            if let Ok(bytes) = self.fa().read(c) {
                let l = Layout::load(&bytes)?;
                self.fa_layouts.insert(key, l);
                return Ok(());
            }
        }
        Err(format!("layout {path} não encontrado"))
    }

    /// Lê os botões do quadro e trata a navegação dos botões em matriz. Devolve corrotinas a criar.
    pub fn update_input(&mut self, input: &Input) -> Vec<(Value, Vec<Value>)> {
        let mut held = 0u16;
        for (i, b) in HID.iter().enumerate() {
            if input.down(*b) {
                held |= 1 << i;
            }
        }
        self.pressed = held & !self.held;
        self.held = held;
        let mut spawn = Vec::new();
        if self.matrix.locked || self.matrix.cells.is_empty() {
            return spawn;
        }
        let (c, r) = (self.matrix.cols.max(1), self.matrix.rows.max(1));
        let (mut x, mut y) = (self.matrix.focus % c, self.matrix.focus / c);
        let p = |i: usize| self.pressed & (1 << i) != 0;
        if p(6) {
            y = (y - 1).rem_euclid(r);
        }
        if p(7) {
            y = (y + 1).rem_euclid(r);
        }
        if p(5) {
            x = (x - 1).rem_euclid(c);
        }
        if p(4) {
            x = (x + 1).rem_euclid(c);
        }
        let cell = y * c + x;
        if cell != self.matrix.focus && self.matrix.cells.contains_key(&cell) {
            self.matrix.focus = cell;
            if let Some(cb) = self.matrix.select_cb.clone() {
                spawn.push((cb, vec![Value::Int(cell), Value::Int(0), Value::Int(2)]));
            }
        }
        if p(0) && std::env::var_os("YW_DEBUG_MENU").is_some() {
            eprintln!(
                "[menu] A: foco {} células {:?} travado {}",
                self.matrix.focus,
                self.matrix
                    .cells
                    .iter()
                    .map(|(c, k)| (
                        *c,
                        self.objs
                            .get(k)
                            .map(|o| (o.name.clone(), o.params.get(&5).map(Value::text)))
                    ))
                    .collect::<Vec<_>>(),
                self.matrix.locked
            );
        }
        if p(0) {
            if let Some(obj) = self
                .matrix
                .cells
                .get(&self.matrix.focus)
                .and_then(|k| self.objs.get(k))
            {
                if let Some(cb) = obj
                    .params
                    .get(&5)
                    .filter(|v| matches!(v, Value::Str(s) if !s.is_empty()))
                {
                    let arg = obj.params.get(&6).cloned().unwrap_or(Value::Int(0));
                    spawn.push((cb.clone(), vec![arg]));
                }
            }
        }
        if p(1) {
            // [H] botão "voltar": objeto criado com um nome de função como 6º argumento do op5002.
            for o in self.objs.values() {
                if o.name.contains("cancel") || o.name.contains("back") {
                    if let Some(Value::Str(cb)) = o.create_args.get(5) {
                        spawn.push((Value::Str(cb.clone()), vec![]));
                        break;
                    }
                }
            }
        }
        spawn
    }

    /// Botões em matriz: o focado toca a animação `_on` (parâmetro 8), os outros `_off` (7) — o que os
    /// callbacks `Menu_MatrixNormal/Touch` fazem no jogo (lá via `op5622($0, $7|$8)`).
    fn update_matrix_visuals(&mut self) {
        let cells: Vec<(i32, u32)> = self.matrix.cells.iter().map(|(c, k)| (*c, *k)).collect();
        for (cell, key) in cells {
            let Some(o) = self.objs.get(&key) else {
                continue;
            };
            let want = o
                .params
                .get(if cell == self.matrix.focus { &8 } else { &7 })
                .map(Value::text);
            let Some(want) = want.filter(|w| !w.is_empty() && *w != o.anim_name) else {
                continue;
            };
            let Some(layout) = o.res.and_then(|r| self.fa_layouts.get(&r)) else {
                continue;
            };
            if let Ok(anim) = Anim::new(layout, &want, true) {
                let o = self.objs.get_mut(&key).expect("obj");
                o.anim = Some(anim);
                o.anim_name = want;
            }
        }
    }

    pub fn tick_objects(&mut self) {
        self.m3d.tick();
        self.update_matrix_visuals();
        for o in self.objs.values_mut() {
            if let Some(a) = o.anim.as_mut() {
                a.tick();
            }
            if let Some((from, to, n, t)) = o.alpha_to.as_mut() {
                *t += 1;
                o.alpha = *from + (*to - *from) * (*t as f32 / (*n).max(1) as f32).min(1.0);
                if *t >= *n {
                    o.alpha = *to;
                    o.alpha_to = None;
                }
            }
            if let Some((from, to, n, t)) = o.move_to.as_mut() {
                *t += 1;
                let k = (*t as f32 / (*n).max(1) as f32).min(1.0);
                o.pos = (from.0 + (to.0 - from.0) * k, from.1 + (to.1 - from.1) * k);
                if *t >= *n {
                    o.move_to = None;
                }
            }
        }
    }

    fn world_pos(&self, key: u32) -> (f32, f32) {
        let mut p = (0.0, 0.0);
        let mut k = Some(key);
        for _ in 0..16 {
            let Some(o) = k.and_then(|k| self.objs.get(&k)) else {
                break;
            };
            p.0 += o.pos.0;
            p.1 += o.pos.1;
            k = o.parent;
        }
        p
    }

    /// Posição de um texto: a do objeto + as posições de instância (layout) dos ancestrais animados.
    /// [H] o objeto de texto herda o lugar da peça de layout do pai (ex.: texto da janela de informação).
    fn text_pos(&self, key: u32) -> (f32, f32) {
        let mut p = self.world_pos(key);
        let mut k = self.objs.get(&key).and_then(|o| o.parent);
        for _ in 0..16 {
            let Some(o) = k.and_then(|k| self.objs.get(&k)) else {
                break;
            };
            if let (Some(r), Some(a)) = (o.res, o.anim.as_ref()) {
                if let Some(l) = self.fa_layouts.get(&r) {
                    let ip = l.instance_position(a.instance);
                    p.0 += ip[0];
                    p.1 += ip[1];
                }
            }
            k = o.parent;
        }
        p
    }

    fn effective_alpha(&self, key: u32) -> f32 {
        let mut a = 1.0;
        let mut k = Some(key);
        for _ in 0..16 {
            let Some(o) = k.and_then(|k| self.objs.get(&k)) else {
                break;
            };
            if !o.visible {
                return 0.0;
            }
            a *= o.alpha;
            k = o.parent;
        }
        a
    }

    pub fn draw(&self, screens: &mut Screens) {
        screens.clear();
        let mut list: Vec<(&u32, &Obj)> = self.objs.iter().collect();
        list.sort_by_key(|(_, o)| (o.prio, o.order));
        for (key, o) in list {
            if let Some(text) = &o.text {
                let alpha = self.effective_alpha(*key);
                if let (Some(f), true) = (self.font(o.small_font), alpha > 0.0) {
                    let (px, w, h) = if o.screen == 0 {
                        (&mut screens.top, TOP_W, TOP_H)
                    } else {
                        (&mut screens.bottom, BOTTOM_W, BOTTOM_H)
                    };
                    // [H] posição do objeto = canto superior esquerdo do texto; cor escura padrão.
                    let mut pos = self.text_pos(*key);
                    // [H] op5942: 1 = centralizado, 2 = à direita (largura da linha mais longa).
                    let tw = text.lines().map(|l| f.width(l)).max().unwrap_or(0) as f32;
                    // [H] com caixa (op5950) alinha dentro dela; sem caixa, em torno do ponto.
                    let bw = o.text_box.map_or(0.0, |b| b.0);
                    match o.align {
                        1 => pos.0 += (bw - tw) / 2.0,
                        2 => pos.0 += bw - tw,
                        _ => {}
                    }
                    if o.anchor_right {
                        pos.0 -= tw;
                    }
                    f.draw(
                        px,
                        w,
                        h,
                        pos,
                        text,
                        o.color.unwrap_or(0x30_2820),
                        1.0,
                        alpha,
                    );
                }
            }
            let (Some(res), Some(anim)) = (o.res, o.anim.as_ref()) else {
                continue;
            };
            let Some(layout) = self.fa_layouts.get(&res) else {
                continue;
            };
            let alpha = self.effective_alpha(*key) * anim.alpha;
            if alpha <= 0.0 {
                continue;
            }
            let pos = self.world_pos(*key);
            let a = Anim {
                instance: anim.instance,
                frame: anim.frame,
                looping: anim.looping,
                reverse: anim.reverse,
                alpha,
                offset: pos,
            };
            let mut target = if o.screen == 0 {
                Target {
                    pixels: &mut screens.top,
                    width: TOP_W,
                    height: TOP_H,
                }
            } else {
                Target {
                    pixels: &mut screens.bottom,
                    width: BOTTOM_W,
                    height: BOTTOM_H,
                }
            };
            layout.draw(&mut target, &[&a]);
        }
        self.m3d.draw(&mut screens.top, TOP_W, TOP_H);
    }

    #[cfg(test)]
    pub fn object_count(&self) -> usize {
        self.objs.len()
    }

    /// Caminho do .bcstm para um nome de música do script (`M35_MENU` → `snd/product/stream/M35_menu*.bcstm`).
    pub fn bgm_path(&self, name: &str) -> Option<PathBuf> {
        let dir = self.romfs.join("snd/product/stream");
        let want = name.to_ascii_lowercase();
        std::fs::read_dir(&dir).ok()?.flatten().find_map(|e| {
            let n = e.file_name().to_string_lossy().to_ascii_lowercase();
            (n.starts_with(&want) && n.ends_with(".bcstm")).then(|| e.path())
        })
    }

    fn obj(&mut self, v: &Value) -> Option<&mut Obj> {
        self.objs.get_mut(&v.key())
    }
}

impl Host for MenuHost {
    fn intercept(&mut self, function: u32, _args: &[Value], _thread: usize) -> Option<Native> {
        // Janelas de informação da biblioteca comum (`OpenInfoWin*`): o menu não as desenha ainda; sem isto o script
        // espera para sempre o fechamento e a tela de nome (2070) fica coberta. [H] respondidas como fechadas.
        const INFO_WINDOWS: [&[u8]; 9] = [
            b"OpenInfoWin",
            b"OpenInfoWin2",
            b"OpenInfoWinEx",
            b"OpenInfoWinStr",
            b"OpenInfoWinStrEx2",
            b"OpenInfoWinStrEx3",
            b"OpenInfoWinStrSkip",
            b"OpenInfoWinBStr",
            b"OpenInfoWinBStrSkip",
        ];
        if INFO_WINDOWS.iter().any(|n| function == crc32(n)) {
            return Some(Native::Done(Value::Int(1)));
        }
        let v = match function {
            // [H] janelas de mensagem/sim-não: sem texto ainda → respondidas como "sim" (1) na hora.
            0x2462_94f0 /* Common_SelectYN */ => Some(Value::Int(1)),
            0x2a2b_dced /* OpenInfoWinStrEx */ => Some(Value::Int(1)),
            // O tipo escolhido na CharaMake (2060) chega ao NameMenu (2070) por esta função do script anterior.
            0xc88d_4429 /* CharaMakeMenu_GetPlayerType */ => Some(Value::Int(self.player_type)),
            _ => None,
        };
        v.map(Native::Done)
    }

    fn native(&mut self, c: Call) -> Native {
        let a = |k: usize| c.args.get(k).cloned().unwrap_or(Value::Int(0));
        let done = |v: i32| Native::Done(Value::Int(v));
        match c.op {
            // ---- recursos
            3502 | 16526 | 16520 => {
                // [H] (nome, caminho sem extensão/idioma) → layout .xa. 16526 visto com os mesmos argumentos.
                if let (Value::Str(_) | Value::Hash(_), Value::Str(path)) = (a(0), a(1)) {
                    let path = path.replace("<LG>", "en");
                    if let Err(e) = self.load_layout(a(0).key(), &path) {
                        eprintln!("[menu] {e}");
                    }
                }
                done(1)
            }
            // ---- objetos 2D
            5002 => {
                let key = a(0).key();
                let order = self.next_order;
                self.next_order += 1;
                self.objs.insert(
                    key,
                    Obj {
                        name: a(0).text(),
                        screen: a(1).int() as u8,
                        prio: a(2).int(),
                        order,
                        res: None,
                        anim: None,
                        anim_name: String::new(),
                        visible: true,
                        alpha: 1.0,
                        alpha_to: None,
                        pos: (0.0, 0.0),
                        move_to: None,
                        parent: None,
                        params: HashMap::new(),
                        create_args: c.args.to_vec(),
                        text: None,
                        small_font: false,
                        align: 0,
                        text_box: None,
                        anchor_right: false,
                        color: None,
                    },
                );
                Native::Done(a(0))
            }
            5009 => done(self.objs.contains_key(&a(0).key()) as i32),
            5600 => {
                let res = a(1).key();
                if let Some(o) = self.obj(&a(0)) {
                    o.res = Some(res);
                }
                done(1)
            }
            5622 => {
                // (obj, animação, flags). Flags vistas só no título com `youkai_wa` [H, pelo efeito no 3DS]:
                // 2 = prepara oculta (criação), 12 = entra tocando para frente (New Game/Continue),
                // 6 = oculta de novo (Menu_TitleStartReset). Sem flag = toca em laço.
                let key = a(0).key();
                let name = a(1).text();
                let flags = a(2).int();
                let res = self.objs.get(&key).and_then(|o| o.res);
                if let Some(layout) = res.and_then(|r| self.fa_layouts.get(&r)) {
                    match Anim::new(layout, &name, flags == 0) {
                        Ok(mut anim) => {
                            anim.alpha = if flags == 2 || flags == 6 { 0.0 } else { 1.0 };
                            if let Some(o) = self.objs.get_mut(&key) {
                                o.anim = Some(anim);
                                o.anim_name = name;
                            }
                        }
                        Err(e) => {
                            *self.unknown.entry(format!("anim {name}: {e}")).or_default() += 1;
                        }
                    }
                }
                done(1)
            }
            5045 => Native::Done(Value::Str("".into())),
            5005 => {
                let v = a(1).truthy();
                if let Some(o) = self.obj(&a(0)) {
                    o.visible = v;
                }
                done(1)
            }
            5102 => {
                let v = a(1).float();
                if let Some(o) = self.obj(&a(0)) {
                    o.alpha = v;
                    o.alpha_to = None;
                }
                done(1)
            }
            5103 => {
                let (to, n) = (a(1).float(), a(2).int().max(0) as u32);
                if let Some(o) = self.obj(&a(0)) {
                    o.alpha_to = Some((o.alpha, to, n, 0));
                }
                done(1)
            }
            // [H] "ainda se movendo?" (laços de espera de CharaMake)
            5250 => done(
                self.objs
                    .get(&a(0).key())
                    .is_some_and(|o| o.move_to.is_some()) as i32,
            ),
            // [H] "ainda animando alfa?" (TitleMenu espera o logo com ele)
            5100 => done(
                self.objs
                    .get(&a(0).key())
                    .is_some_and(|o| o.alpha_to.is_some()) as i32,
            ),
            5242 => {
                let p = (a(1).float(), a(2).float());
                if let Some(o) = self.obj(&a(0)) {
                    o.pos = p;
                }
                done(1)
            }
            5247 => {
                let d = (a(1).float(), a(2).float());
                if let Some(o) = self.obj(&a(0)) {
                    o.pos.0 += d.0;
                    o.pos.1 += d.1;
                }
                done(1)
            }
            5248 => {
                let (d, n) = ((a(1).float(), a(2).float()), a(3).int().max(1) as u32);
                // [H] destino absoluto (CharaMake: sai para ±200 e volta a 0 com o mesmo opcode).
                if let Some(o) = self.obj(&a(0)) {
                    o.move_to = Some((o.pos, d, n, 0));
                }
                done(1)
            }
            5013 => {
                let parent = a(1).key();
                if let Some(o) = self.obj(&a(0)) {
                    o.parent = Some(parent);
                }
                done(1)
            }
            5046 => {
                let (i, v) = (a(1).int(), a(2));
                if let Some(o) = self.obj(&a(0)) {
                    o.params.insert(i, v);
                }
                done(1)
            }
            5050 | 5006 | 5008 | 5051 | 5202 | 5252 | 5262 | 5033 | 5430 => done(1),
            // ---- texto
            13733 => {
                let t = self.text(&a(0).text(), a(1).key()).unwrap_or_default();
                Native::Done(Value::Str(t.as_str().into()))
            }
            // (s, i, TAG, v) / (s, TAG, v) / (s, tipo, TAG, v): troca <TAG> por v.
            13730 | 13790 | 13791 => {
                let (tag, v) = if c.op == 13790 {
                    (a(1), a(2))
                } else {
                    (a(2), a(3))
                };
                let s = a(0).text().replace(&format!("<{}>", tag.text()), &v.text());
                Native::Done(Value::Str(s.as_str().into()))
            }
            5911 => {
                self.ensure_fonts();
                let t = font::clean(&a(1).text());
                if let Some(o) = self.obj(&a(0)) {
                    o.text = Some(t);
                }
                done(1)
            }
            5910 => {
                if let Some(o) = self.obj(&a(0)) {
                    o.text = None;
                }
                done(1)
            }
            17000 => {
                let small = a(2).key() == crc32(b"#FNT_SML");
                if let Some(o) = self.obj(&a(0)) {
                    o.small_font = small;
                }
                done(1)
            }
            5942 => {
                let v = a(1).int();
                if std::env::var_os("YW_DEBUG_MENU").is_some() {
                    eprintln!("[menu] 5942 {} {}", a(0).text(), v);
                }
                if let Some(o) = self.obj(&a(0)) {
                    o.align = v;
                }
                done(1)
            }
            // [H] op5932 = largura da caixa de texto (64 "Diary", 128 texto do slot); o alinhamento (op5942)
            // é dentro dela. op5950 = espaçamento de letras/linhas (0/8, -1/0) — ainda ignorado.
            5932 => {
                if c.args.len() >= 4 {
                    // (obj, r, g, b) em 0..1 = cor do texto
                    let ch = |f: f32| (f.clamp(0.0, 1.0) * 255.0).round() as u32;
                    let rgb = (ch(a(1).float()) << 16) | (ch(a(2).float()) << 8) | ch(a(3).float());
                    if let Some(o) = self.obj(&a(0)) {
                        o.color = Some(rgb);
                    }
                } else {
                    let w = a(1).float();
                    if let Some(o) = self.obj(&a(0)) {
                        o.text_box = (w > 0.0).then_some((w, 0.0));
                    }
                }
                done(1)
            }
            5944 => {
                let on = a(1).int() == 1;
                if let Some(o) = self.obj(&a(0)) {
                    o.anchor_right = on;
                }
                done(1)
            }
            5933 | 5943 | 5950 => done(1),
            // ---- botões em matriz
            2110 => {
                let sub = a(0).int();
                let m = &mut self.matrix;
                match sub {
                    1000 => {
                        *m = Matrix {
                            cols: a(1).int(),
                            rows: a(2).int(),
                            select_cb: Some(a(4)).filter(|v| matches!(v, Value::Str(_))),
                            ..Matrix::default()
                        };
                        done(1)
                    }
                    1001 => done(a(1).int() + a(2).int() * m.cols.max(1)),
                    1002 => {
                        let cell = a(2).int() + a(3).int() * m.cols.max(1);
                        m.cells.insert(cell, a(1).key());
                        done(1)
                    }
                    1003 => {
                        m.focus = a(1).int();
                        done(1)
                    }
                    1011 => {
                        m.locked = a(1).truthy();
                        done(1)
                    }
                    _ => {
                        *self.unknown.entry(format!("2110.{sub}")).or_default() += 1;
                        done(0)
                    }
                }
            }
            // ---- entrada
            1000 => done((self.pressed != 0) as i32),
            1103 => {
                let i = a(0).int();
                done(((0..12).contains(&i) && self.pressed & (1 << i) != 0) as i32)
            }
            // ---- música [H]: 14800 carrega → handle; 14806 toca; 14805 "carregando?"; 14807 "em fade?";
            // 14808 fade de saída; 14809 para; 14810 handle atual.
            14800 => {
                self.current_bgm += 1;
                self.bgm_names.insert(self.current_bgm, a(0).text());
                done(self.current_bgm)
            }
            14806 => {
                if let Some(n) = self.bgm_names.get(&a(0).int()) {
                    self.bgm_play = Some(n.clone());
                }
                done(1)
            }
            14808 | 14809 => {
                self.bgm_stop = true;
                done(1)
            }
            14810 => done(self.current_bgm),
            14805 | 14807 => done(0),
            // ---- cenas
            2104 => {
                self.request = Some(SceneRequest {
                    id: a(0).int(),
                    args: c.args.to_vec(),
                });
                Native::Wait
            }
            2212 => Native::Done(
                self.scene_args
                    .get(a(0).int() as usize)
                    .cloned()
                    .unwrap_or(Value::Int(0)),
            ),
            2106 | 2109 | 2102 | 2190 | 2191 | 2193 | 2194 | 3010 | 3011 | 3012 => done(0),
            // ---- save: diários do port (`save.rs`). Slots 1..3.
            12054 => {
                let slot = self
                    .slots
                    .get((a(0).int() - 1).max(0) as usize)
                    .and_then(|s| s.as_ref());
                done(match (slot, a(1).int()) {
                    (Some(s), 0) => s.flags(),
                    (Some(s), 10) => s.play_seconds as i32,
                    _ => 0,
                })
            }
            // op13760(id, i): texto por id (nome do lugar do diário etc.). A chave dos nomes de lugar do
            // `map_config` não foi decifrada (não está em nenhuma tabela de texto): devolve vazio, não `0`. [H]
            13760 => Native::Done(Value::Str(std::rc::Rc::from(""))),
            // data do diário (`FUN_003fb1cc`: u16 ano, u8 mês, dia, hora, minuto)
            12055 => {
                let slot = self
                    .slots
                    .get((a(0).int() - 1).max(0) as usize)
                    .and_then(|s| s.as_ref());
                done(match (slot, a(1).int()) {
                    (Some(s), i @ 0..=4) => s.saved_at[i as usize] as i32,
                    _ => 0,
                })
            }
            12053 => {
                let slot = self
                    .slots
                    .get((a(0).int() - 1).max(0) as usize)
                    .and_then(|s| s.as_ref());
                Native::Done(Value::Str(slot.map_or("", |s| s.name.as_str()).into()))
            }
            12050 => {
                self.current_slot = a(0).int().clamp(1, crate::save::SLOTS as i32) as usize;
                done(1)
            }
            12003 => {
                self.save_requested = true;
                done(1)
            }
            // ---- cenário 3D do menu (`menu3d.rs`)
            3702 => {
                let fa = self.fa.clone();
                self.m3d.load(&fa, a(0).key(), &a(1).text());
                done(0)
            }
            6002 => {
                self.m3d.create(a(0).key());
                Native::Done(a(0))
            }
            6800 => {
                self.m3d.set_model(a(0).key(), a(1).key());
                done(0)
            }
            3730 => {
                self.m3d.set_motion(a(0).key(), a(1).key());
                done(0)
            }
            6810 | 6819 => {
                self.m3d.play(a(0).key(), &a(1).text(), c.op == 6819);
                done(0)
            }
            6202 => {
                self.m3d.set_scale(a(0).key(), a(1).float());
                done(0)
            }
            6222 | 6223 => {
                self.m3d
                    .set_pos(a(0).key(), [a(1).float(), a(2).float(), a(3).float()]);
                done(0)
            }
            6227 => {
                self.m3d.set_yaw(a(0).key(), a(2).float());
                done(0)
            }
            4102 => {
                self.m3d
                    .set_camera(Some([a(1).float(), a(2).float(), a(3).float()]), None, None);
                done(0)
            }
            4122 => {
                self.m3d
                    .set_camera(None, Some([a(1).float(), a(2).float(), a(3).float()]), None);
                done(0)
            }
            4142 => {
                self.m3d
                    .set_camera(None, None, Some(a(1).float().to_degrees()));
                done(0)
            }
            // Opções do jogo (tela Settings): op14150(i) lê, op14151(i, v) grava — persistidas no settings.ini.
            14150 => {
                let i = a(0).int().clamp(0, 4) as usize;
                done(crate::settings::Settings::load().game[i])
            }
            14151 => {
                let mut st = crate::settings::Settings::load();
                let i = a(0).int().clamp(0, 4) as usize;
                st.game[i] = a(1).int();
                st.save();
                done(1)
            }
            // LoadData: op12004/op2728/op12002 → sucesso.
            12004 | 2728 | 12002 | 12007 => done(1),
            12052 | 12056 => done(0),
            15155 => {
                self.player_name = Some(a(0).text());
                done(1)
            }
            15156 => {
                self.player_type = a(0).int();
                done(1)
            }
            15150 => done(self.player_type),
            op => {
                *self.unknown.entry(op.to_string()).or_default() += 1;
                done(0)
            }
        }
    }
}
