//! Eventos dirigidos pelo PRÓPRIO script (`seq/event/<ev>.xq`) na VM XQ32, com um host que implementa as funções
//! de biblioteca e os comandos `op2110` de evento usados no prólogo. Primeiro alvo: `ev01_0010a` (rua, 19 falas).
//!
//! Mapeamento (nomes das funções resolvidos pelo CRC-32 em `prog_common_001e.xq`; semântica pelo uso — [H]):
//!   AssignCharacterBase{LQ2,HQ2}/AssignCharacterNPCLQ2(nome, id) → cria ator (`c000` = protagonista)
//!   SetDriveCharaMot_Sys2(ator, clipe, ...) → troca o clipe (nomes Shift-JIS comparados por bytes)
//!   OpenTextCaptionDetail2(id, ...) → fala (texto `data/txt/ev/<ev>_{m|f}_en.cfg.bin`, chave crc32(id)); espera A
//!   WaitFrame2(n) → espera n quadros (30 quadros/s) · LoadCameraMotion/SetDriveCameraMot → câmera `.xv`
//!   StartBrightFade(-1, alvo, quadros) → fade de/para preto
//!   op2110: 11004 objeto (modelo), 11006 pacote de movimentos, 11046 prender a nó, 14010 visível,
//!           14016 posição (x, y, z, ângulo), 28009 ponto de rota, 28010 percorre a rota com um clipe
//! O resto devolve 0 e fica registrado em `unknown` (lista impressa no fim do evento).

use crate::assets::arc0::Archive;
use crate::assets::cfgbin::{self, Param};
use crate::assets::crc32;
use crate::character::{Character, Motion};
use crate::cutscene::CameraAnim;
use crate::dialog::{Dialog, Mode};
use crate::gfx3d::{self, Camera, Frame, MeshRef, V3};
use crate::input::Input;
use crate::room::Room;
use crate::screen::Screens;
use crate::xq::{self, Call, Host, Native, Value, Vm};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

struct Actor {
    body: Option<Character>,
    motions: Vec<Motion>,
    /// (slot, nome do arquivo sem extensão) de cada movimento em `motions` (-1 = liberado por `11007`).
    slots: Vec<(i32, String)>,
    /// Balão de emoção (`11030`): nome e quadros restantes. [H] ainda não desenhado.
    emote: Option<(String, u32)>,
    /// Rotação em graus pedida por `14020` (x, y, z). [H] só o yaw (y) entra no desenho.
    rot: [f32; 3],
    /// Clipe tocado por `14007`: sem repetição (nome que não termina em `L`) e se já chegou ao fim (`14009`).
    oneshot: bool,
    finished: bool,
    clip: Option<(usize, usize)>,
    frame: f32,
    pos: V3,
    yaw: f32,
    visible: bool,
    attach: Option<(u32, u32)>,
    path: Vec<V3>,
    speed: f32,
}

impl Actor {
    fn new(body: Option<Character>) -> Actor {
        Actor {
            body,
            motions: Vec::new(),
            slots: Vec::new(),
            emote: None,
            rot: [0.0; 3],
            oneshot: false,
            finished: false,
            clip: None,
            frame: 0.0,
            pos: [0.0; 3],
            yaw: 0.0,
            visible: true,
            attach: None,
            path: Vec::new(),
            speed: 0.0,
        }
    }

    fn set_clip(&mut self, name: &str) -> bool {
        for (mi, m) in self.motions.iter().enumerate() {
            if let Some(ci) = m
                .clips
                .iter()
                .position(|c| c.name == name || c.raw == xq::raw_bytes(name))
            {
                if self.clip != Some((mi, ci)) {
                    self.clip = Some((mi, ci));
                    self.frame = m.clips[ci].start;
                }
                self.oneshot = false;
                self.finished = false;
                return true;
            }
        }
        false
    }

    fn advance(&mut self) {
        if let Some((mi, ci)) = self.clip {
            let c = &self.motions[mi].clips[ci];
            let (start, end, speed) = (c.start, c.end, c.speed);
            if self.oneshot {
                // clipe sem repetição: pára no último quadro e avisa o script (`14009`)
                if self.frame + speed >= end {
                    self.frame = (end - 0.01).max(start);
                    self.finished = true;
                } else {
                    self.frame += speed;
                }
            } else {
                self.frame += speed;
                if self.frame >= end {
                    self.frame = start + (self.frame - start).rem_euclid((end - start).max(1.0));
                }
            }
        }
        if let Some(t) = self.path.first().copied() {
            let (dx, dz) = (t[0] - self.pos[0], t[2] - self.pos[2]);
            let d = dx.hypot(dz);
            if d <= self.speed {
                self.pos = t;
                self.path.remove(0);
            } else {
                self.yaw = dx.atan2(dz).to_degrees();
                self.pos[0] += dx / d * self.speed;
                self.pos[2] += dz / d * self.speed;
            }
        }
    }
}

/// Câmera dada por parâmetros (`SetCameraParamData2`): vai do estado atual até o conjunto nomeado em `frames` quadros de 30 Hz.
#[derive(Clone, Copy)]
struct ParamCam {
    from: (V3, V3, f32),
    to: (V3, V3, f32),
    t: f32,
    frames: f32,
}

impl ParamCam {
    fn now(&self) -> (V3, V3, f32) {
        let k = if self.frames <= 0.0 {
            1.0
        } else {
            (self.t / self.frames).clamp(0.0, 1.0)
        };
        // [H] suavização entre os dois pontos (o último argumento da chamada parece escolher a curva)
        let k = k * k * (3.0 - 2.0 * k);
        let mix = |a: f32, b: f32| a + (b - a) * k;
        let v = |a: V3, b: V3| [mix(a[0], b[0]), mix(a[1], b[1]), mix(a[2], b[2])];
        (
            v(self.from.0, self.to.0),
            v(self.from.1, self.to.1),
            mix(self.from.2, self.to.2),
        )
    }
}

/// `data/res/event/<evento>.cfg`: `CAMERA_ITEM "nome", x, y, z, alvoX, alvoY, alvoZ, fov, near, far;`
fn event_camera_params(fa: &Archive, event: &str, base: &str) -> HashMap<String, [f32; 9]> {
    let mut out = HashMap::new();
    for name in [event, base] {
        let Ok(b) = fa.read(&format!("data/res/event/{name}.cfg")) else {
            continue;
        };
        for line in String::from_utf8_lossy(&b).lines() {
            let Some(rest) = line.trim().strip_prefix("CAMERA_ITEM") else {
                continue;
            };
            let rest = rest.trim().trim_end_matches(';');
            let mut it = rest.split(',');
            let Some(n) = it.next().map(|x| x.trim().trim_matches('"').to_string()) else {
                continue;
            };
            let v: Vec<f32> = it.filter_map(|x| x.trim().parse().ok()).collect();
            if v.len() == 9 {
                out.insert(n, [v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7], v[8]]);
            }
        }
        if !out.is_empty() {
            break;
        }
    }
    out
}

pub struct EventHost {
    fa: Rc<Archive>,
    event: String,
    player_type: i32,
    actors: HashMap<u32, Actor>,
    texts: HashMap<u32, String>,
    /// Conversas (`EventTalkRun(id)`): crc32(id) → falas na ordem do índice.
    talks: HashMap<u32, Vec<String>>,
    /// Textos de sistema por hash do nome (tutoriais).
    sys_texts: HashMap<u32, String>,
    /// Câmeras por parâmetros do evento (`CAMERA_ITEM`) e a que está ativa.
    camera_params: HashMap<String, [f32; 9]>,
    pcam: Option<ParamCam>,
    /// Tremor de câmera (`CtrlCameraShake`): amplitude e velocidade; `shake_t` conta quadros de 60 Hz.
    shake: (f32, f32),
    shake_t: f32,
    /// Controle do jogador travado (`16026`); o evento já não deixa o jogador andar, fica registrado.
    pub input_locked: bool,
    /// Escala de tempo (`12011`); [H] ainda não aplicada às animações.
    pub time_scale: f32,
    /// Efeitos sonoros pedidos (`14002`), na ordem — ainda sem som (o banco BCSAR não é lido).
    pub sfx: Vec<String>,
    /// Objetos presos à mão (`27006`) por nome.
    pub props: Vec<String>,
    dialog: Option<Dialog>,
    waits: HashMap<usize, u32>,
    camera: Option<CameraAnim>,
    cam_frame: f32,
    cam_playing: bool,
    route: Vec<V3>,
    /// 0 = cena visível, 1 = preto.
    fade: f32,
    fade_to: f32,
    fade_step: f32,
    pub unknown: BTreeMap<String, usize>,
}

fn event_texts(fa: &Archive, event: &str, player_type: i32) -> HashMap<u32, String> {
    let g = if player_type == 2 { "f" } else { "m" };
    let mut out = HashMap::new();
    if let Ok(b) = fa.read(&format!("data/txt/ev/{event}_{g}_en.cfg.bin")) {
        for e in cfgbin::parse(&b).unwrap_or_default() {
            if let (Some(Param::Int(h)), Some(Param::Str(t))) = (e.params.first(), e.params.get(2))
            {
                if e.name == "TEXT_INFO" {
                    out.insert(*h as u32, t.clone());
                }
            }
        }
    }
    out
}

/// Textos de sistema (`data/res/text/system_text_en.cfg.bin`, `TEXT_INFO(hash do nome, 0, texto)`): janelas de
/// tutorial como `sys_info_win_11`.
pub fn system_texts(fa: &Archive) -> HashMap<u32, String> {
    let mut out = HashMap::new();
    if let Ok(b) = fa.read("data/res/text/system_text_en.cfg.bin") {
        for e in cfgbin::parse(&b).unwrap_or_default() {
            if let (true, Some(Param::Int(h)), Some(Param::Str(t))) =
                (e.name == "TEXT_INFO", e.params.first(), e.params.get(2))
            {
                out.insert(*h as u32, t.clone());
            }
        }
    }
    out
}

/// Funções de janela de informação/obtenção de item da biblioteca comum, tratadas como concluídas
/// (`OpenInfoWin` tem tratamento próprio: mostra o texto do tutorial).
const INFO_WINDOWS: [&[u8]; 9] = [
    b"OpenInfoWin2",
    b"OpenInfoWinEx",
    b"OpenInfoWinStr",
    b"OpenInfoWinStrEx",
    b"OpenInfoWinStrEx2",
    b"OpenInfoWinStrEx3",
    b"OpenInfoWinStrSkip",
    b"OpenInfoWinBStr",
    b"AddItemWin",
];

/// Falas agrupadas por conversa: `TEXT_INFO(crc32(id), índice, texto, quem fala)`.
pub fn event_talks(fa: &Archive, event: &str, player_type: i32) -> HashMap<u32, Vec<String>> {
    let g = if player_type == 2 { "f" } else { "m" };
    let mut lines: HashMap<u32, Vec<(i32, String)>> = HashMap::new();
    if let Ok(b) = fa.read(&format!("data/txt/ev/{event}_{g}_en.cfg.bin")) {
        for e in cfgbin::parse(&b).unwrap_or_default() {
            if let (Some(Param::Int(h)), Some(Param::Int(i)), Some(Param::Str(t))) =
                (e.params.first(), e.params.get(1), e.params.get(2))
            {
                if e.name == "TEXT_INFO" {
                    lines.entry(*h as u32).or_default().push((*i, t.clone()));
                }
            }
        }
    }
    lines
        .into_iter()
        .map(|(k, mut v)| {
            v.sort_by_key(|x| x.0);
            (k, v.into_iter().map(|x| x.1).collect())
        })
        .collect()
}

impl EventHost {
    /// Câmera do momento: parâmetros do evento, ou o arquivo `.xv`, ou uma câmera de mapa atrás do protagonista;
    /// mais o tremor, se houver.
    fn camera_now(&self) -> (V3, V3, f32) {
        let (mut eye, target, fov) = if let Some(p) = &self.pcam {
            p.now()
        } else {
            match (&self.camera, self.cam_playing) {
                (Some(c), true) => c.at(self.cam_frame),
                _ => {
                    let lead = self
                        .actors
                        .get(&crc32(&xq::raw_bytes("\u{8e}\u{e5}\u{90}l\u{8c}\u{f6}")))
                        .map(|a| a.pos);
                    let t = lead.unwrap_or([490.0, 3.0, -348.0]);
                    (
                        [t[0], t[1] + 6.0 + 56.6, t[2] + 56.6],
                        [t[0], t[1] + 6.0, t[2]],
                        35.0,
                    )
                }
            }
        };
        if self.shake.0 > 0.0 {
            let (a, t) = (self.shake.0, self.shake_t);
            eye[0] += a * (t * 1.7).sin();
            eye[1] += a * (t * 2.3 + 1.0).sin();
        }
        (eye, target, fov)
    }

    fn actor(&mut self, v: &Value) -> Option<&mut Actor> {
        self.actors.get_mut(&v.key())
    }

    fn body_path(&self, id: &str) -> String {
        match id {
            "c000" if self.player_type == 2 => "data/character/c002000/c002000_p00.xc".into(),
            "c000" => "data/character/c001000/c001000_p00.xc".into(),
            _ => format!("data/character/{id}000/{id}000_p00.xc"),
        }
    }

    fn assign(&mut self, name: &Value, id: &str) {
        let body = self
            .fa
            .read(&self.body_path(id))
            .and_then(|b| Character::load(&b))
            .ok();
        let mut a = Actor::new(body);
        if id == "c000" {
            let base = if self.player_type == 2 {
                "c000020"
            } else {
                "c000010"
            };
            if let Ok(m) = self
                .fa
                .read(&format!("data/character/{base}/{base}_p10.xc"))
                .and_then(|b| Motion::load(&b))
            {
                a.motions.push(m);
            }
        }
        self.actors.insert(name.key(), a);
    }

    fn wait(&mut self, thread: usize, frames: u32) -> Native {
        let left = self.waits.entry(thread).or_insert(frames + 1);
        *left = left.saturating_sub(1);
        if *left == 0 {
            self.waits.remove(&thread);
            Native::Done(Value::Int(0))
        } else {
            Native::Wait
        }
    }
}

impl Host for EventHost {
    fn intercept(&mut self, function: u32, args: &[Value], thread: usize) -> Option<Native> {
        let a = |k: usize| args.get(k).cloned().unwrap_or(Value::Int(0));
        let done = Some(Native::Done(Value::Int(0)));
        // WaitCloseWindow2: a fala aqui já bloqueia até fechar (OpenTextCaptionDetail2).
        if function == crc32(b"WaitCloseWindow2") {
            return done;
        }
        match function {
            0x4bc4_d588 | 0x4ccd_7d54 | 0xc888_374c => {
                self.assign(&a(0), &a(1).text());
                done
            }
            0x7454_1bc0 => {
                let clip = a(1).text();
                if let Some(act) = self.actor(&a(0)) {
                    act.set_clip(&clip);
                }
                done
            }
            0x9bbe_b133 => {
                // OpenTextCaptionDetail2: abre (1ª chamada) e espera fechar (chamadas repetidas pela VM)
                if let Some(d) = &self.dialog {
                    if d.done {
                        self.dialog = None;
                        return done;
                    }
                    return Some(Native::Wait);
                }
                let key = crc32(&xq::raw_bytes(&a(0).text()));
                let text = self.texts.get(&key).cloned().unwrap_or_else(|| a(0).text());
                self.dialog = Dialog::new(&self.fa, &[text], Mode::Balloon).ok();
                Some(Native::Wait)
            }
            // EventTalkRun(id): conversa com balões; bloqueia até a última fala
            f if f == crc32(b"EventTalkRun") || f == crc32(b"HelpTalkRun") => {
                if let Some(d) = &self.dialog {
                    if d.done {
                        self.dialog = None;
                        return done;
                    }
                    return Some(Native::Wait);
                }
                let key = crc32(&xq::raw_bytes(&a(0).text()));
                match self.talks.get(&key) {
                    Some(lines) if !lines.is_empty() => {
                        self.dialog = Dialog::new(&self.fa, lines, Mode::Balloon).ok();
                        Some(Native::Wait)
                    }
                    _ => done,
                }
            }
            // OpenInfoWin(tipo, "sys_info_win_NN", …): janela de tutorial com o texto do sistema; espera fechar
            f if f == crc32(b"OpenInfoWin") => {
                if let Some(d) = &self.dialog {
                    if d.done {
                        self.dialog = None;
                        return done;
                    }
                    return Some(Native::Wait);
                }
                match self.sys_texts.get(&crc32(&xq::raw_bytes(&a(1).text()))) {
                    Some(t) => {
                        self.dialog =
                            Dialog::new(&self.fa, std::slice::from_ref(t), Mode::Balloon).ok();
                        Some(Native::Wait)
                    }
                    None => done,
                }
            }
            // Demais janelas de informação (obtenção de item etc.): ainda sem desenho → fecham na hora. [H]
            f if INFO_WINDOWS.iter().any(|n| f == crc32(n)) => done,
            0x6d02_73a1 => Some(self.wait(thread, a(0).float().max(0.0) as u32)),
            // SetCameraParamData2(cam, quadros, "nome", …) e SetCameraParamData: câmera pelos parâmetros do evento
            f if f == crc32(b"SetCameraParamData2") || f == crc32(b"SetCameraParamData") => {
                let name = a(2).text();
                if let Some(p) = self.camera_params.get(&name).copied() {
                    let from = self.camera_now();
                    self.cam_playing = false;
                    self.pcam = Some(ParamCam {
                        from,
                        to: ([p[0], p[1], p[2]], [p[3], p[4], p[5]], p[6]),
                        t: 0.0,
                        frames: a(1).float().max(0.0),
                    });
                } else {
                    *self.unknown.entry(format!("camera:{name}")).or_default() += 1;
                }
                done
            }
            // CtrlCameraShake(cam, quadros, amplitude, velocidade, …): amplitude 0 = parar
            f if f == crc32(b"CtrlCameraShake") => {
                self.shake = (a(2).float(), a(3).float());
                done
            }
            0xc8a7_859c => {
                self.camera = self
                    .fa
                    .read(&a(1).text())
                    .and_then(|b| CameraAnim::parse_xv(&b))
                    .ok();
                self.cam_playing = false;
                done
            }
            0x8d84_daea => {
                self.cam_playing = self.camera.is_some();
                self.cam_frame = 0.0;
                done
            }
            0x0862_2fbc => {
                // StartBrightFade(-1, alvo, quadros): alvo -1 = escurecer, 0 = clarear
                self.fade_to = if a(1).float() < 0.0 { 1.0 } else { 0.0 };
                self.fade_step = 1.0 / a(2).float().max(1.0);
                done
            }
            _ => None,
        }
    }

    fn native(&mut self, c: Call) -> Native {
        let a = |k: usize| c.args.get(k).cloned().unwrap_or(Value::Int(0));
        if c.op == 2110 {
            let sub = a(0).int();
            match sub {
                // 11004(3, nome, caminho.xc, 0, 0, 0): carrega o modelo como objeto de cena (o 3 é o tipo)
                11004 => {
                    let body = self
                        .fa
                        .read(&a(3).text())
                        .and_then(|b| Character::load(&b))
                        .ok();
                    self.actors.insert(a(2).key(), Actor::new(body));
                }
                11006 => {
                    let path = a(2).text();
                    let base = path
                        .replace("data/event/ev_mot/", "data/character/")
                        .replace("_em01_p10", "_p10");
                    let fa = self.fa.clone();
                    let slot = a(3).int();
                    if let Some(act) = self.actor(&a(1)) {
                        for p in [base, path] {
                            if let Ok(m) = fa.read(&p).and_then(|b| Motion::load(&b)) {
                                let stem =
                                    p.rsplit('/').next().unwrap_or(&p).trim_end_matches(".xc");
                                act.motions.push(m);
                                act.slots.push((slot, stem.to_string()));
                            }
                        }
                    }
                }
                11046 => {
                    let (parent, node) = (a(2).key(), a(3).key());
                    if let Some(act) = self.actor(&a(1)) {
                        act.attach = Some((parent, node));
                    }
                }
                14010 => {
                    let v = a(2).truthy();
                    if let Some(act) = self.actor(&a(1)) {
                        act.visible = v;
                    }
                }
                14016 => {
                    let (p, yaw) = ([a(3).float(), a(4).float(), a(5).float()], a(6).float());
                    if let Some(act) = self.actor(&a(1)) {
                        act.pos = p;
                        act.yaw = yaw;
                        act.path.clear();
                    }
                }
                // 16026(0/1): trava/libera o controle do jogador durante a cena
                16026 => self.input_locked = a(1).truthy(),
                // 11013(nome, 0): prepara o objeto — o port já o cria pronto em 11004
                11013 => {}
                // 11030(nome, "ビックリ01"): balão de emoção sobre o personagem
                11030 => {
                    let e = a(2).text();
                    if let Some(act) = self.actor(&a(1)) {
                        act.emote = Some((e, 60));
                    }
                }
                // 14007(nome, clipe, [slot]): toca o clipe pelo NOME nos movimentos carregados do objeto; se não for um
                // clipe, o argumento é o arquivo de movimento (`c0010_point_a`) ou o slot. Nome terminado em `L` repete;
                // os demais tocam uma vez (o script espera o fim em `14009`).
                14007 => {
                    let (anim, slot) = (a(2).text(), a(3).int());
                    if let Some(act) = self.actor(&a(1)) {
                        if act.set_clip(&anim) {
                            let (mi, ci) = act.clip.unwrap_or((0, 0));
                            let c = &act.motions[mi].clips[ci];
                            act.frame = c.start;
                            act.oneshot = c.raw.last() != Some(&b'L');
                            act.finished = false;
                        } else {
                            let found = act
                                .slots
                                .iter()
                                .position(|(s, n)| {
                                    *s >= 0 && !anim.is_empty() && n.ends_with(&anim)
                                })
                                .or_else(|| act.slots.iter().position(|(s, _)| *s == slot));
                            match found {
                                Some(mi) => {
                                    // 1º clipe com duração de verdade (o clipe `t` é só a pose T)
                                    let ci = act.motions[mi]
                                        .clips
                                        .iter()
                                        .position(|c| c.end - c.start > 2.0)
                                        .unwrap_or(0);
                                    if let Some(c) = act.motions[mi].clips.get(ci) {
                                        act.frame = c.start;
                                        act.clip = Some((mi, ci));
                                        act.oneshot = false;
                                        act.finished = false;
                                    }
                                }
                                // clipe que o port não tem: dá por terminado para o script não esperar para sempre
                                None => act.finished = true,
                            }
                        }
                    }
                }
                // 14009(nome): "o movimento terminou?" — o script repete a consulta até vir verdadeira
                14009 => {
                    let done = self
                        .actor(&a(1))
                        .is_none_or(|act| act.finished || act.clip.is_none() || !act.oneshot);
                    return Native::Done(Value::Int(i32::from(done)));
                }
                // 11007(nome, slot): libera o slot de movimento
                11007 => {
                    let slot = a(2).int();
                    if let Some(act) = self.actor(&a(1)) {
                        for i in 0..act.slots.len() {
                            if act.slots[i].0 == slot {
                                act.slots[i].0 = -1;
                                if act.clip.is_some_and(|(mi, _)| mi == i) {
                                    act.clip = None;
                                }
                            }
                        }
                    }
                }
                // 11008(nome): destrói o objeto
                11008 => {
                    self.actors.remove(&a(1).key());
                }
                // 11016(nome, ponto, 1, ângulo): orienta o objeto
                11016 => {
                    let yaw = a(4).float();
                    if let Some(act) = self.actor(&a(1)) {
                        act.yaw = yaw;
                    }
                }
                // 11061(nome, 0/1): ativa ou desativa o personagem
                11061 => {
                    let on = a(2).truthy();
                    if let Some(act) = self.actor(&a(1)) {
                        act.visible = on;
                    }
                }
                // 14020(nome, rx, ry, rz): rotação em graus (os valores medidos são ângulos, não escala)
                14020 => {
                    let r = [a(2).float(), a(3).float(), a(4).float()];
                    if let Some(act) = self.actor(&a(1)) {
                        act.rot = r;
                    }
                }
                // 27006(nome): adereço (rede, gaiola) vai para a mão do personagem [H: só registrado]
                27006 => self.props.push(a(1).text()),
                // 14002("ese…", v, v, v): efeito sonoro de evento — registrado até haver som
                14002 => self.sfx.push(a(1).text()),
                // 12011(escala, 0): escala de tempo
                12011 => self.time_scale = a(1).float().max(0.05),
                // 15203(): reinicia a câmera de evento
                15203 => {
                    self.pcam = None;
                    self.cam_playing = false;
                    self.shake = (0.0, 0.0);
                }
                // 14019(obj_flag), 11062(modo), 11017(offset), 11052(skin do relógio), 11015, 16023/16035
                // (início visual do evento): sem efeito visível no port por enquanto
                14019 | 11062 | 11017 | 11052 | 11015 | 16023 | 16035 => {}
                28008 => self.route.clear(),
                28009 => self.route.push([a(1).float(), a(2).float(), a(3).float()]),
                28010 => {
                    let (speed, clip) = (a(2).float(), a(4).text());
                    let route = std::mem::take(&mut self.route);
                    if let Some(act) = self.actor(&a(1)) {
                        act.path = route;
                        act.speed = (speed * 2.0).max(0.3); // [H] unidades por quadro de 30 Hz → tique de 60 Hz
                        act.set_clip(&clip);
                    }
                }
                _ => *self.unknown.entry(format!("2110.{sub}")).or_default() += 1,
            }
            return Native::Done(Value::Int(0));
        }
        // consultas "terminou?" de objetos 3D (op6813): o script repete até virem verdadeiras. [H] o port não
        // anima esses objetos, então responde que já terminaram.
        if c.op == 6813 {
            return Native::Done(Value::Int(1));
        }
        *self.unknown.entry(c.op.to_string()).or_default() += 1;
        Native::Done(Value::Int(0))
    }
}

pub struct EventScene {
    vm: Vm,
    pub host: EventHost,
    stage: Option<Room>,
    frame: Frame,
    tick: u64,
    pub finished: bool,
}

impl EventScene {
    pub fn open(
        fa: Rc<Archive>,
        event: &str,
        stage: Option<Room>,
        player_type: i32,
    ) -> crate::assets::Result<EventScene> {
        let path = format!("seq/event/{event}.xq");
        let mut vm = Vm::new(xq::load_scene(&fa, &path)?, vec![]);
        if !vm.start_scene() {
            return Err(format!("{path}: sem função de entrada"));
        }
        let base = event.trim_end_matches(['a', 'b']).to_string();
        Ok(EventScene {
            vm,
            host: EventHost {
                texts: event_texts(&fa, &base, player_type),
                talks: event_talks(&fa, &base, player_type),
                sys_texts: system_texts(&fa),
                camera_params: event_camera_params(&fa, event, &base),
                pcam: None,
                shake: (0.0, 0.0),
                shake_t: 0.0,
                input_locked: false,
                time_scale: 1.0,
                sfx: Vec::new(),
                props: Vec::new(),
                fa,
                event: base,
                player_type,
                actors: HashMap::new(),
                dialog: None,
                waits: HashMap::new(),
                camera: None,
                cam_frame: 0.0,
                cam_playing: false,
                route: Vec::new(),
                fade: 1.0,
                fade_to: 1.0,
                fade_step: 0.05,
                unknown: BTreeMap::new(),
            },
            stage,
            frame: Frame::new(400, 240),
            tick: 0,
            finished: false,
        })
    }

    /// Onde está cada corrotina do script (diagnóstico de eventos que não terminam).
    #[cfg(test)]
    pub fn threads(&self) -> Vec<String> {
        self.vm.thread_names()
    }

    /// Um tique de 60 Hz: o script roda a 30 Hz (tiques pares); animações e diálogo a cada tique.
    pub fn tick(&mut self, input: Option<&Input>) -> crate::assets::Result<()> {
        self.tick += 1;
        if let Some(d) = self.host.dialog.as_mut() {
            d.tick(input);
        }
        for a in self.host.actors.values_mut() {
            a.advance();
        }
        if self.host.cam_playing {
            self.host.cam_frame += 0.5;
        }
        if let Some(p) = self.host.pcam.as_mut() {
            p.t += 0.5; // um tique de 60 Hz = meio quadro de 30 Hz
        }
        self.host.shake_t += 1.0;
        for a in self.host.actors.values_mut() {
            if let Some((_, n)) = a.emote.as_mut() {
                *n = n.saturating_sub(1);
            }
            if a.emote.as_ref().is_some_and(|e| e.1 == 0) {
                a.emote = None;
            }
        }
        let h = &mut self.host;
        h.fade += (h.fade_to - h.fade).clamp(-h.fade_step, h.fade_step) * 0.5;
        if self.tick.is_multiple_of(2) {
            self.vm.frame(&mut self.host)?;
            if self.vm.alive() == 0 && !self.finished {
                self.finished = true;
                eprintln!(
                    "[yw1] evento {} terminou; nativos sem host: {:?}",
                    self.host.event, self.host.unknown
                );
            }
        }
        Ok(())
    }

    fn actor_world(&self, key: u32, depth: u32) -> Option<([[f32; 3]; 3], V3)> {
        let a = self.host.actors.get(&key)?;
        let (s, c) = a.yaw.to_radians().sin_cos();
        let own = ([[c, 0.0, -s], [0.0, 1.0, 0.0], [s, 0.0, c]], a.pos);
        match a.attach {
            Some((parent, node)) if depth < 4 => {
                let p = self.host.actors.get(&parent)?;
                let (pr, pp) = self.actor_world(parent, depth + 1)?;
                let (motion, frame) = p
                    .clip
                    .map_or((None, 0.0), |(mi, _)| (Some(&p.motions[mi]), p.frame));
                let (nr, np) = p.body.as_ref()?.node_world(motion, frame, node)?;
                let tp = mulv(np, &pr);
                Some((mul(&nr, &pr), [tp[0] + pp[0], tp[1] + pp[1], tp[2] + pp[2]]))
            }
            _ => Some(own),
        }
    }

    pub fn draw(&mut self, scale: usize, screens: &mut Screens) -> &Frame {
        self.frame.resize(400 * scale, 240 * scale);
        self.frame.clear(0x0080_b0e0);
        // câmera: parâmetros do evento, arquivo `.xv` ou câmera de mapa atrás do protagonista (+ tremor)
        let (eye, target, fov) = self.host.camera_now();
        let cam = Camera::look_at(eye, target, fov, 1.0);
        let mut frame = std::mem::replace(&mut self.frame, Frame::new(1, 1));
        // passe 0: o céu (planos de imagem fixa `ma-fix_img`, com mistura alfa e sem profundidade) fica ATRÁS de tudo;
        // passe 1: cenário opaco e o resto dos objetos
        for pass in 0..2 {
            if pass == 1 {
                if let Some(st) = &self.stage {
                    st.draw_world(&mut frame, &cam, false);
                }
            }
            for (&key, a) in &self.host.actors {
                let (Some(body), true) = (&a.body, a.visible) else {
                    continue;
                };
                let sky = !body.meshes.is_empty()
                    && body.meshes.iter().all(|m| m.material.contains("fix_img"));
                if (pass == 0) != sky {
                    continue;
                }
                let Some((r, p)) = self.actor_world(key, 0) else {
                    continue;
                };
                let skinned: Vec<Vec<V3>> = match a.clip {
                    Some((mi, _)) => body.skin(&a.motions[mi], a.frame),
                    None => body.skin_bind(),
                };
                for (mesh, verts) in body.meshes.iter().zip(skinned) {
                    // protagonista de evento junta menino e menina: mostra só o do tipo escolhido
                    if mesh.material.contains("c002000")
                        && self.host.player_type != 2
                        && mesh.material.contains("DefaultLib")
                        && body.meshes.iter().any(|m| m.material.contains("c001000"))
                    {
                        continue;
                    }
                    let pos: Vec<V3> = verts
                        .iter()
                        .map(|v| {
                            let w = mulv(*v, &r);
                            [w[0] + p[0], w[1] + p[1], w[2] + p[2]]
                        })
                        .collect();
                    let colors = vec![[1.0; 4]; pos.len()];
                    let tex = mesh
                        .texture
                        .and_then(|t| body.textures.get(t))
                        .and_then(|t| t.as_ref());
                    gfx3d::draw_mesh(
                        &mut frame,
                        &cam,
                        &MeshRef {
                            pos: &pos,
                            color: &colors,
                            uv: &mesh.uv,
                            tris: &mesh.tris,
                            offset: [0.0; 3],
                            texture: tex,
                            blend: sky,
                            tint: [1.0; 3],
                        },
                    );
                }
            }
        }
        if let Some(st) = &self.stage {
            st.draw_world(&mut frame, &cam, true);
        }
        let k = ((1.0 - self.host.fade).clamp(0.0, 1.0) * 256.0) as u32;
        if k < 256 {
            for px in frame.color.iter_mut() {
                let ch = |s: u32| ((((*px >> s) & 0xff) * k) >> 8) << s;
                *px = ch(16) | ch(8) | ch(0);
            }
        }
        if let Some(d) = &self.host.dialog {
            let (w, h) = (frame.width, frame.height);
            d.draw(&mut frame.color, w, h, screens);
        }
        self.frame = frame;
        &self.frame
    }
}

fn mul(a: &[[f32; 3]; 3], b: &[[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut r = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    r
}

fn mulv(v: V3, m: &[[f32; 3]; 3]) -> V3 {
    [
        v[0] * m[0][0] + v[1] * m[1][0] + v[2] * m[2][0],
        v[0] * m[0][1] + v[1] * m[1][1] + v[2] * m[2][1],
        v[0] * m[0][2] + v[1] * m[1][2] + v[2] * m[2][2],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Button;

    /// ev01_0010a pelo script, sem janela, apertando A a cada 30 tiques; grava quadros em work/event/.
    #[test]
    #[ignore]
    fn ev01_0010_runs_from_script() {
        let romfs = crate::romfs::locate(None).unwrap();
        let fa = Rc::new(Archive::open(&romfs.join("yw1_a.fa")).unwrap());
        let stage = Room::load(&fa, "t101g00").ok();
        let mut ev = EventScene::open(fa, "ev01_0010a", stage, 1).unwrap();
        let mut input = Input::headless();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../work/event");
        std::fs::create_dir_all(&dir).unwrap();
        let mut lines = 0;
        for t in 0..20000 {
            input.set(Button::A, t % 30 == 29);
            if input.pressed(Button::A) && ev.host.dialog.is_some() {
                lines += 1;
            }
            ev.tick(Some(&input)).unwrap();
            if t % 400 == 100 {
                let mut screens = Screens::new();
                let f = ev.draw(1, &mut screens);
                let raw: Vec<u8> = f
                    .color
                    .iter()
                    .flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, 255])
                    .collect();
                std::fs::write(dir.join(format!("t{t:05}.rgba")), raw).unwrap();
            }
            if ev.finished {
                break;
            }
        }
        println!("corrotinas {:?}", ev.vm.thread_names());
        println!(
            "atores {}, apertos durante fala {lines}, desconhecidos {:?}",
            ev.host.actors.len(),
            ev.host.unknown
        );
        assert!(ev.finished, "evento não terminou");
    }

    /// Roda um evento pelo script real (nome em `YW_EVENT`, mapa de fundo em `YW_STAGE`) e grava quadros em `work/event/`
    /// para conferir o visual sem janela: `YW_EVENT=ev01_0005a cargo test event_snapshots -- --ignored`.
    #[test]
    #[ignore]
    fn event_snapshots() {
        let name = std::env::var("YW_EVENT").unwrap_or_else(|_| "ev01_0005a".into());
        let stage_id = std::env::var("YW_STAGE").unwrap_or_else(|_| "t101g00".into());
        let step: u32 = std::env::var("YW_STEP")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(90);
        let romfs = crate::romfs::locate(None).unwrap();
        let fa = Rc::new(Archive::open(&romfs.join("yw1_a.fa")).unwrap());
        let stage = Room::load(&fa, &stage_id).ok();
        let mut ev = EventScene::open(fa, &name, stage, 1).unwrap();
        let mut input = Input::headless();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../work/event");
        std::fs::create_dir_all(&dir).unwrap();
        for t in 0..20000u32 {
            input.set(Button::A, t % 30 == 29);
            ev.tick(Some(&input)).unwrap();
            if t % step == 0 {
                let mut screens = Screens::new();
                let f = ev.draw(1, &mut screens);
                let raw: Vec<u8> = f
                    .color
                    .iter()
                    .flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, 255])
                    .collect();
                std::fs::write(dir.join(format!("{name}_{t:05}.rgba")), raw).unwrap();
            }
            if ev.finished {
                break;
            }
            if t == 60 || t == 300 {
                for (k, a) in &ev.host.actors {
                    println!(
                        "ator {k:08x}: corpo {} malhas {} visível {} pos {:?} anexo {:?} clipe {:?} movimentos {} quadro {:.1}",
                        a.body.is_some(),
                        a.body.as_ref().map_or(0, |b| b.meshes.len()),
                        a.visible,
                        a.pos,
                        a.attach.map(|x| (format!("{:08x}", x.0), format!("{:08x}", x.1))),
                        a.clip,
                        a.motions.len(),
                        a.frame
                    );
                }
                for (k, a) in &ev.host.actors {
                    if let (Some(b), Some((_, p))) = (&a.body, ev.actor_world(*k, 0)) {
                        println!("  mundo {k:08x} malhas {} em {:.1?}", b.meshes.len(), p);
                    } else if a.body.is_some() {
                        println!(
                            "  SEM POSIÇÃO {k:08x} anexo {:?}",
                            a.attach.map(|x| format!("{:08x}", x.1))
                        );
                    }
                }
                if let Some((_, sky)) = ev
                    .host
                    .actors
                    .iter()
                    .find(|(_, a)| a.body.as_ref().is_some_and(|b| b.meshes.len() == 26))
                {
                    let body = sky.body.as_ref().unwrap();
                    for m in &body.meshes {
                        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
                        for v in &m.pos {
                            for k in 0..3 {
                                lo[k] = lo[k].min(v[k]);
                                hi[k] = hi[k].max(v[k]);
                            }
                        }
                        let tex = m
                            .texture
                            .and_then(|t| body.textures.get(t))
                            .and_then(|t| t.as_ref());
                        let opaque = tex.map_or(0.0, |t| {
                            t.pixels.iter().filter(|p| (*p >> 24) > 16).count() as f32
                                / t.pixels.len().max(1) as f32
                        });
                        println!("  céu {:30} tex {:?} {}x{} opaco {:.2} verts {} tris {} caixa {:.0?}..{:.0?}", m.material, m.texture, tex.map_or(0, |t| t.width), tex.map_or(0, |t| t.height), opaque, m.pos.len(), m.tris.len(), lo, hi);
                    }
                }
                let (e, t, f) = ev.host.camera_now();
                println!("câmera em {e:.1?} olhando {t:.1?} fov {f:.1}");
                println!(
                    "câmera: arquivo {} tocando {} quadro {:.1} param {}",
                    ev.host.camera.is_some(),
                    ev.host.cam_playing,
                    ev.host.cam_frame,
                    ev.host.pcam.is_some()
                );
                println!("ignorados {:?}", ev.host.unknown);
            }
        }
    }

    /// `SetCameraParamData2` lê `CAMERA_ITEM` do `.cfg` do evento e a câmera chega aos valores do arquivo.
    #[test]
    fn param_camera_follows_the_event_cfg() {
        let romfs = crate::romfs::locate(None).unwrap();
        let fa = Rc::new(Archive::open(&romfs.join("yw1_a.fa")).unwrap());
        let cams = event_camera_params(&fa, "ev01_0140", "ev01_0140");
        let c = cams.get("c0010_start").expect("c0010_start");
        assert_eq!(
            [c[0], c[1], c[2], c[6], c[7], c[8]],
            [695.3182, 53.8402, -10.7129, 35.0, 10.0, 600.0]
        );
        assert!(cams.contains_key("c0010_mov"));
        // rodando o evento, a câmera por parâmetros é usada e nenhum nome fica sem achar
        let mut ev = EventScene::open(fa, "ev01_0140", None, 1).unwrap();
        let mut input = Input::headless();
        let mut used = false;
        for t in 0..40000 {
            input.set(Button::A, t % 20 == 19);
            ev.tick(Some(&input)).unwrap();
            used |= ev.host.pcam.is_some();
            if ev.finished {
                break;
            }
        }
        assert!(
            ev.finished && used,
            "terminou {} usou a câmera {used}",
            ev.finished
        );
        assert!(
            !ev.host.unknown.keys().any(|k| k.starts_with("camera:")),
            "{:?}",
            ev.host.unknown
        );
    }

    /// Roda (sem cenário) todos os eventos do capítulo 1 depois da escada, na ordem das fases de
    /// `seq/phs/c01_trigger_0.01.xq`, e relata quais terminam.
    #[test]
    #[ignore]
    fn chapter1_events_run() {
        let romfs = crate::romfs::locate(None).unwrap();
        let fa = Rc::new(Archive::open(&romfs.join("yw1_a.fa")).unwrap());
        let mut input = Input::headless();
        let mut bad = Vec::new();
        for e in crate::chapter1::EVENTS_BOY {
            let mut ev = match EventScene::open(fa.clone(), e, None, 1) {
                Ok(ev) => ev,
                Err(err) => {
                    println!("{e}: não abre: {err}");
                    bad.push(*e);
                    continue;
                }
            };
            let mut t = 0;
            while t < 40000 && !ev.finished {
                input.set(Button::A, t % 30 == 29);
                if let Err(err) = ev.tick(Some(&input)) {
                    println!("{e}: erro {err}");
                    break;
                }
                t += 1;
            }
            println!(
                "{e}: {} em {t} ticks; desconhecidos {:?}",
                if ev.finished { "ok" } else { "PRESO" },
                ev.host.unknown
            );
            if !ev.finished {
                println!("   corrotinas {:?}", ev.vm.thread_names());
                bad.push(*e);
            }
        }
        assert!(bad.is_empty(), "eventos com problema: {bad:?}");
    }
}
