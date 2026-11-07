//! Cena 3D de um mapa interior (`data/map/<id>/`): geometria, texturas, câmera do `mapenv`, jogador
//! provisório e colisão com as paredes. Alvo: o quarto do protagonista (`t101i02`, onde o jogo fica
//! jogável pela primeira vez — docs/FORMATS.md, seção "Mapas 3D").
//!
//! HIPÓTESES (não lidas no código do jogo): câmera orbital em torno do jogador com `Dist`/`RotV`/`Fov` do
//! mapenv (Fov vertical); a câmera fica no lado +z olhando para -z; `jn1-*` é a variante "baixa" duplicada
//! de `jmden23-*` (desenha-se só uma); teste de alfa 0.4 no lugar do material real (`.mtr`, não decifrado);
//! velocidades do jogador chutadas.

use crate::assets::arc0::Archive;
use crate::assets::cfgbin::{self, MapCamera};
use crate::assets::chrc::Manifest;
use crate::assets::imgc::{self, Image};
use crate::assets::xmpr::{self, Mesh};
use crate::assets::xpck::Pack;
use crate::assets::{f32le, u32le, Result};
#[cfg(test)]
use crate::assets::{l5, u16le, xpvb};
use crate::gfx3d::{self, Camera, Frame, MeshRef, V3};
use crate::input::{Button, Input};
use crate::screen::{TOP_H, TOP_W};
use std::collections::HashMap;

struct Piece {
    mesh: Mesh,
    offset: V3,
    texture: Option<usize>,
    blend: bool,
    /// Esfera envolvente em mundo (descarte por distância/direção).
    center: V3,
    radius: f32,
}

fn bounds(pos: &[V3], o: V3) -> (V3, f32) {
    if pos.is_empty() {
        return (o, 0.0);
    }
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for p in pos {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k] + o[k]);
            hi[k] = hi[k].max(p[k] + o[k]);
        }
    }
    let c = [
        (lo[0] + hi[0]) / 2.0,
        (lo[1] + hi[1]) / 2.0,
        (lo[2] + hi[2]) / 2.0,
    ];
    let r =
        ((hi[0] - lo[0]).powi(2) + (hi[1] - lo[1]).powi(2) + (hi[2] - lo[2]).powi(2)).sqrt() / 2.0;
    (c, r)
}

/// Troca de mapa de um ponto de função `MAP_JUMP` (`<id>_funcpt.bin`).
#[derive(Clone, Debug, PartialEq)]
pub struct MapJump {
    pub name: String,
    pub to: String,
    /// `STD_POS`: onde o jogador aparece no mapa de destino (conferido na planta: a chegada de i01→i02
    /// cai no patamar acima da escada do quarto, fora do gatilho de descida).
    pub arrival: V3,
    pub yaw: f32,
}

impl MapJump {
    /// Teleporte pedido por script (`map_warp`).
    pub fn to(to: String, arrival: V3, yaw: f32) -> MapJump {
        MapJump {
            name: format!("map_warp {to}"),
            to,
            arrival,
            yaw,
        }
    }
}

struct Trigger {
    center: V3,
    half: [f32; 2],
    /// Metade da altura da caixa (BOX[1]/2): o gatilho também tem limite vertical
    /// (nas casas BOX h=20 => quase sempre vale;城墙 de mapas abertos podem exigir).
    half_y: f32,
    jump: MapJump,
}

/// `FP [nome]{POS[x,y,z,rot] BOX[w,h,d]}` + `FP_DEF [nome]{MAP_JUMP{JUMP_TO{MapID,...} STD_POS{POS[x,y,z,rot]}}}`.
fn parse_funcpt(entries: &[cfgbin::Entry]) -> Vec<Trigger> {
    use cfgbin::Param;
    let mut stack: Vec<(String, String)> = Vec::new();
    let mut boxes: HashMap<String, (V3, [f32; 3])> = HashMap::new();
    let mut defs: HashMap<String, (String, V3, f32)> = HashMap::new();
    for e in entries {
        match e.name.as_str() {
            "PTREE" => {
                let n = e
                    .params
                    .first()
                    .and_then(Param::as_str)
                    .unwrap_or("")
                    .to_string();
                let arg = e
                    .params
                    .get(1)
                    .and_then(Param::as_str)
                    .unwrap_or("")
                    .to_string();
                stack.push((n, arg));
            }
            "_PTREE" => {
                stack.pop();
            }
            "PTVALS" | "PTVAL" => {
                let names: Vec<&str> = stack.iter().map(|(n, _)| n.as_str()).collect();
                let owner = |kind: &str| {
                    stack
                        .iter()
                        .rev()
                        .find(|(n, _)| n == kind)
                        .map(|(_, a)| a.clone())
                        .unwrap_or_default()
                };
                let v: Vec<f32> = e.params.iter().filter_map(Param::as_f32).collect();
                match names.as_slice() {
                    [.., "FP", "POS"] if v.len() >= 3 => {
                        boxes.entry(owner("FP")).or_insert(([0.0; 3], [0.0; 3])).0 =
                            [v[0], v[1], v[2]];
                    }
                    [.., "FP", "BOX"] if v.len() >= 3 => {
                        boxes.entry(owner("FP")).or_insert(([0.0; 3], [0.0; 3])).1 =
                            [v[0], v[1], v[2]];
                    }
                    [.., "JUMP_TO"] if e.params.get(1).and_then(Param::as_str) == Some("MapID") => {
                        let to = e
                            .params
                            .first()
                            .and_then(Param::as_str)
                            .unwrap_or("")
                            .to_string();
                        defs.entry(owner("FP_DEF"))
                            .or_insert((String::new(), [0.0; 3], 0.0))
                            .0 = to;
                    }
                    [.., "STD_POS", "POS"] if v.len() >= 3 => {
                        let d =
                            defs.entry(owner("FP_DEF"))
                                .or_insert((String::new(), [0.0; 3], 0.0));
                        d.1 = [v[0], v[1], v[2]];
                        d.2 = v.get(3).copied().unwrap_or(0.0);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    let mut out: Vec<Trigger> = defs
        .into_iter()
        .filter(|(_, d)| !d.0.is_empty())
        .filter_map(|(name, (to, arrival, yaw))| {
            let (center, size) = *boxes.get(&name)?;
            Some(Trigger {
                center,
                half: [size[0] / 2.0, size[2] / 2.0],
                half_y: size[1] / 2.0,
                jump: MapJump {
                    name,
                    to,
                    arrival,
                    yaw,
                },
            })
        })
        .collect();
    out.sort_by(|a, b| a.jump.name.cmp(&b.jump.name));
    out
}

/// Modelo do jogador: corpo + pacote de movimentos + clipe atual.
pub struct PlayerModel {
    body: crate::character::Character,
    motion: crate::character::Motion,
    clip: &'static str,
    frame: f32,
}

impl PlayerModel {
    /// Menino: `c001000` + movimentos `c000010_p10`; menina: `c002000` + `c000020_p10` (HIPÓTESE pelo padrão de nomes).
    pub fn load(fa: &Archive, player_type: i32) -> Result<PlayerModel> {
        let (body, motion) = if player_type == 2 {
            (
                "data/character/c002000/c002000_p00.xc",
                "data/character/c000020/c000020_p10.xc",
            )
        } else {
            (
                "data/character/c001000/c001000_p00.xc",
                "data/character/c000010/c000010_p10.xc",
            )
        };
        Ok(PlayerModel {
            body: crate::character::Character::load(&fa.read(body)?)?,
            motion: crate::character::Motion::load(&fa.read(motion)?)?,
            clip: "立ち1L",
            frame: 0.0,
        })
    }

    fn set_clip(&mut self, name: &'static str) {
        if self.clip != name {
            self.clip = name;
            self.frame = self.motion.clip(name).map_or(0.0, |c| c.start);
        }
    }

    fn advance(&mut self) {
        if let Some(c) = self.motion.clip(self.clip) {
            self.frame += c.speed;
            if self.frame >= c.end || self.frame < c.start {
                self.frame =
                    c.start + (self.frame - c.start).rem_euclid((c.end - c.start).max(1.0));
            }
        }
    }
}

pub struct Player {
    pub pos: V3,
    /// Ângulo em graus em torno de Y (0 = voltado para +z).
    pub yaw: f32,
}

pub struct Room {
    pub id: String,
    pieces: Vec<Piece>,
    textures: Vec<Option<Image>>,
    pub camera: MapCamera,
    walls: Vec<([f32; 2], [f32; 2])>,
    /// Triângulos andáveis da malha de colisão (`<id>cl.*` com material `col-`, ny > 0.3), em
    /// coordenadas de mundo. Fonte da ALTURA do jogador: em i01/i02/i03/i04 o `cl` é a casca
    /// bruta da casa (pisos empilhados por nível + rampas), e os y coincidem com os FPs de
    /// funcpt (ex.: FP da escada i01→i02 em y=4, casca dá 3.99 — ver work/notes/collision/).
    floors: Vec<[[f32; 3]; 3]>,
    triggers: Vec<Trigger>,
    /// Modelo animado do jogador (sem ele, desenha a caixa provisória).
    pub model: Option<PlayerModel>,
    /// Gatilhos só valem depois que o jogador saiu de todas as caixas (não dispara ao chegar).
    armed: bool,
    pub player: Player,
    /// Giro extra da câmera (graus), controlado por L/R.
    pub orbit: f32,
    /// Graus por quadro do giro de câmera (configuração).
    pub camera_speed: f32,
    frame: Frame,
}

const PLAYER_RADIUS: f32 = 3.5;
const WALK_SPEED: f32 = 0.6;
const RUN_SPEED: f32 = 1.2;
/// Degrau máximo de subida/descida por passo: acima disso não é "andar", é outra camada.
/// A análise BFS (work/notes/collision/) subiu todas as escadas com |Δy|<=0.6 por 0.5 de XZ.
const MAX_STEP: f32 = 0.65;

impl Room {
    #[cfg(test)]
    /// Caixa envolvente do cenário (mín., máx.) e a lista de peças (centro, raio) — usado para enquadrar câmeras.
    pub fn bounds(&self) -> (V3, V3, Vec<(V3, f32)>) {
        let mut lo = [f32::MAX; 3];
        let mut hi = [f32::MIN; 3];
        for p in &self.pieces {
            for k in 0..3 {
                lo[k] = lo[k].min(p.center[k] - p.radius);
                hi[k] = hi[k].max(p.center[k] + p.radius);
            }
        }
        (
            lo,
            hi,
            self.pieces.iter().map(|p| (p.center, p.radius)).collect(),
        )
    }

    pub fn load(fa: &Archive, id: &str) -> Result<Room> {
        let base = format!("data/map/{id}/{id}");
        // Interiores: um `<id>.xc`. Mapas abertos (rua `t101g00`): grade de blocos `<prefixo>NN.xc` +
        // biblioteca de texturas `<prefixo>0(_en).xr` (ex. t1010_en.xr).
        let mut packs: Vec<Pack> = Vec::new();
        if let Ok(b) = fa.read(&format!("{base}.xc")) {
            packs.push(Pack::parse(&b)?);
        } else {
            let prefix = &id[..id.len().saturating_sub(2)]; // t101g00 → t101g
            for n in 0..100 {
                if let Ok(b) = fa.read(&format!("data/map/{id}/{prefix}{n:02}.xc")) {
                    packs.push(Pack::parse(&b)?);
                }
            }
        }
        if packs.is_empty() {
            return Err(format!("mapa {id}: nenhum .xc"));
        }
        // Bibliotecas externas de textura (hash → índice global): as citadas pela tag 20 dos blocos + a do mapa.
        let mut textures: Vec<Option<Image>> = Vec::new();
        let mut external: HashMap<u32, usize> = HashMap::new();
        let mut libs: Vec<String> = vec![id.to_string()];
        for pack in &packs {
            if let Ok(m) = pack.get("RES.bin").and_then(Manifest::parse) {
                for l in m.libraries {
                    if !libs.contains(&l) {
                        libs.push(l);
                    }
                }
            }
        }
        for lib_id in &libs {
            if lib_id.len() < 4 {
                continue;
            }
            let lib_base = format!("data/map/{lib_id}/{}0", &lib_id[..4]);
            let Some(b) = [format!("{lib_base}_en.xr"), format!("{lib_base}.xr")]
                .iter()
                .find_map(|p| fa.read(p).ok())
            else {
                continue;
            };
            let lib = Pack::parse(&b)?;
            let m = Manifest::parse(lib.get("RES.bin")?)?;
            for (i, h) in m.textures.iter().enumerate() {
                external.entry(*h).or_insert(textures.len());
                textures.push(lib.get(&format!("{i:03}.xi")).and_then(imgc::decode).ok());
            }
        }
        let collision_prefix = format!("{id}cl.");
        let mut pieces = Vec::new();
        let mut walls = Vec::new();
        let mut floors = Vec::new();
        for pack in &packs {
            let manifest = Manifest::parse(pack.get("RES.bin")?)?;
            // Nós (.mbn, 168 B): w0 hash, w1 hash do pai, w3..5 translação.
            let mut nodes: HashMap<u32, (u32, V3)> = HashMap::new();
            for (name, d) in &pack.files {
                if name.ends_with(".mbn") && d.len() >= 24 {
                    nodes.insert(
                        u32le(d, 0)?,
                        (u32le(d, 4)?, [f32le(d, 12)?, f32le(d, 16)?, f32le(d, 20)?]),
                    );
                }
            }
            let world = |mut h: u32| -> V3 {
                let mut t = [0.0; 3];
                for _ in 0..32 {
                    let Some(&(parent, d)) = nodes.get(&h) else {
                        break;
                    };
                    t = [t[0] + d[0], t[1] + d[1], t[2] + d[2]];
                    h = parent;
                }
                t
            };
            let local0 = textures.len();
            for i in 0..manifest.textures.len() {
                textures.push(pack.get(&format!("{i:03}.xi")).and_then(imgc::decode).ok());
            }
            for i in 0..manifest.meshes.len() {
                let Ok(d) = pack.get(&format!("{i:03}.prm")) else {
                    continue;
                };
                let mesh = xmpr::parse(d)?;
                if mesh.name.contains("cl.")
                    && (mesh.name.starts_with(&collision_prefix) || !mesh.has_uv)
                {
                    // grupo `cl`: material `col-` = paredes (só faces verticais); `snd..-sdw..-` (som de passo +
                    // sombra) = chão andável, inclusive rampas da escada. Conferido em t101i01/t101i02.
                    let offset = world(mesh.node_hash);
                    if mesh.material_name.contains("col") {
                        collect_walls(&mesh, offset, &mut walls);
                    }
                    if mesh.material_name.contains("snd") || mesh.material_name.contains("sdw") {
                        collect_floors(&mesh, offset, &mut floors);
                    }
                    continue;
                }
                if mesh.name.contains(".jn1-") || mesh.material_name.contains("col-") {
                    continue;
                }
                let texture = manifest
                    .texture_of_material(mesh.material_hash)
                    .map(|t| local0 + t)
                    .or_else(|| {
                        manifest
                            .texture_hash_of_material(mesh.material_hash)
                            .and_then(|h| external.get(&h).copied())
                    });
                // HIPÓTESE: textura com muitos texels semitransparentes (sombras baked) = mistura alfa.
                let blend = texture
                    .and_then(|t| textures.get(t))
                    .and_then(|t: &Option<Image>| t.as_ref())
                    .is_some_and(translucent);
                let offset = world(mesh.node_hash);
                let (center, radius) = bounds(&mesh.pos, offset);
                pieces.push(Piece {
                    offset,
                    texture,
                    blend,
                    center,
                    radius,
                    mesh,
                });
            }
        }
        let camera = fa
            .read(&format!("{base}_mapenv.bin"))
            .and_then(|b| cfgbin::parse(&b))
            .map(|e| cfgbin::map_camera(&e))
            .unwrap_or_default();
        let triggers = fa
            .read(&format!("{base}_funcpt.bin"))
            .and_then(|b| cfgbin::parse(&b))
            .map(|e| parse_funcpt(&e))
            .unwrap_or_default();
        Ok(Room {
            model: None,
            triggers,
            armed: false,
            id: id.to_string(),
            pieces,
            textures,
            camera,
            walls,
            floors,
            // Posição inicial do menino em t101i02 (SetPosLeaderChara do Phase1, ângulo 90).
            player: Player {
                pos: [-10.0, 0.0, 15.76],
                yaw: 90.0,
            },
            orbit: 0.0,
            camera_speed: 1.5,
            frame: Frame::new(TOP_W, TOP_H),
        })
    }

    pub fn triangles(&self) -> usize {
        self.pieces.iter().map(|p| p.mesh.tris.len()).sum()
    }

    #[cfg(test)]
    pub fn jumps(&self) -> Vec<MapJump> {
        self.triggers.iter().map(|t| t.jump.clone()).collect()
    }

    /// Só anima (parado) — usado enquanto há diálogo na tela.
    pub fn idle(&mut self) {
        if let Some(m) = self.model.as_mut() {
            m.set_clip("立ち1L");
            m.advance();
        }
    }

    pub fn place(&mut self, pos: V3, yaw: f32) {
        self.player = Player { pos, yaw };
        // chegada por script/STD_POS: o y pedido pode não ser o do chão (ex. rua 490,0,-348 tem chão em 3);
        // procura o chão mais próximo numa faixa larga só aqui, não ao andar
        if let Some(y) = floor_nearest(&self.floors, pos[0], pos[2], pos[1], 12.0) {
            self.player.pos[1] = y;
        }
        self.settle_height();
        self.armed = false;
    }

    /// Coloca o jogador no chão do (x, z) atual — banda de degrau [y-MAX_STEP, y+MAX_STEP];
    /// sem apoio, congela a altura (o `cl` é casca grosseira e entendida com furos: o spawn
    /// por evento, ex. t101i02 em (−3.6, 21.8), não tem piso 0 no `cl` ali).
    fn settle_height(&mut self) {
        let (x, z, y) = (self.player.pos[0], self.player.pos[2], self.player.pos[1]);
        if let Some(t) = floor_at(&self.floors, x, z, y) {
            self.player.pos[1] = t;
        }
    }

    fn trigger_at(&self, p: V3) -> Option<&Trigger> {
        self.triggers.iter().find(|t| {
            (p[0] - t.center[0]).abs() <= t.half[0]
                && (p[2] - t.center[2]).abs() <= t.half[1]
                && (p[1] - t.center[1]).abs() <= t.half_y.max(1.5)
        })
    }

    pub fn walls(&self) -> usize {
        self.walls.len()
    }

    fn camera_eye(&self) -> (V3, V3) {
        let c = &self.camera;
        let h = (c.rot_h + self.orbit).to_radians();
        let v = c.rot_v.to_radians();
        let target = [
            self.player.pos[0],
            self.player.pos[1] + 6.0,
            self.player.pos[2],
        ];
        let eye = [
            target[0] + c.dist * v.cos() * h.sin(),
            target[1] + c.dist * v.sin(),
            target[2] + c.dist * v.cos() * h.cos(),
        ];
        (eye, target)
    }

    /// Um passo de 1/60 s: movimento relativo à câmera + colisão com paredes. Devolve a troca de mapa se o
    /// jogador entrou num ponto `MAP_JUMP` (ex.: a escada do quarto).
    pub fn tick(&mut self, input: &Input) -> Option<MapJump> {
        let before = self.player.pos;
        self.move_player(input);
        let moved = (self.player.pos[0] - before[0]).hypot(self.player.pos[2] - before[2]);
        if let Some(m) = self.model.as_mut() {
            m.set_clip(if moved < 0.05 {
                "立ち1L"
            } else if moved > WALK_SPEED * 1.2 {
                "走り1L"
            } else {
                "歩き1L"
            });
            m.advance();
        }
        match self.trigger_at(self.player.pos).map(|t| t.jump.clone()) {
            Some(j) if self.armed => Some(j),
            Some(_) => None,
            None => {
                self.armed = true;
                None
            }
        }
    }

    fn move_player(&mut self, input: &Input) {
        if input.down(Button::L) {
            self.orbit -= self.camera_speed;
        }
        if input.down(Button::R) {
            self.orbit += self.camera_speed;
        }
        let (mut mx, mut mz) = input.circle;
        if mx == 0.0 && mz == 0.0 {
            mx = input.down(Button::Right) as i32 as f32 - input.down(Button::Left) as i32 as f32;
            mz = input.down(Button::Up) as i32 as f32 - input.down(Button::Down) as i32 as f32;
        }
        let len = (mx * mx + mz * mz).sqrt();
        if len < 0.01 {
            return;
        }
        let (mx, mz) = (mx / len.max(1.0), mz / len.max(1.0));
        let (eye, target) = self.camera_eye();
        let (fx, fz) = {
            let (dx, dz) = (target[0] - eye[0], target[2] - eye[2]);
            let l = (dx * dx + dz * dz).sqrt().max(1e-6);
            (dx / l, dz / l)
        };
        // direita da câmera (Y para cima, olhando para (fx, fz)): (-fz, fx)
        let (rx, rz) = (-fz, fx);
        let speed = if input.down(Button::B) {
            RUN_SPEED
        } else {
            WALK_SPEED
        };
        let step = [
            (fx * mz + rx * mx) * speed * len.min(1.0),
            (fz * mz + rz * mx) * speed * len.min(1.0),
        ];
        self.player.yaw = step[0].atan2(step[1]).to_degrees();
        self.player.pos[0] += step[0];
        self.player.pos[2] += step[1];
        for _ in 0..3 {
            for &(a, b) in &self.walls {
                push_out(&mut self.player.pos, a, b);
            }
        }
        self.settle_height();
    }

    /// Desenha na resolução interna `scale`×(400×240) e devolve o quadro.
    pub fn draw(&mut self, scale: usize) -> &Frame {
        self.frame.resize(TOP_W * scale, TOP_H * scale);
        self.frame.clear(0x0010_1018);
        let (eye, target) = self.camera_eye();
        let cam = Camera::look_at(eye, target, self.camera.fov, 1.0);
        let mut frame = std::mem::replace(&mut self.frame, Frame::new(1, 1));
        self.draw_world(&mut frame, &cam, false);
        self.frame = frame;
        self.draw_player(&cam);
        let mut frame = std::mem::replace(&mut self.frame, Frame::new(1, 1));
        self.draw_world(&mut frame, &cam, true);
        self.frame = frame;
        &self.frame
    }

    /// Desenha o cenário (passe opaco ou translúcido) com qualquer câmera — usado também pelas cutscenes.
    /// Peças fora do alcance (`Far` do mapenv) ou atrás da câmera são descartadas.
    pub fn draw_world(&self, frame: &mut Frame, cam: &Camera, blend_pass: bool) {
        let far = self.camera.far.max(300.0);
        let fwd = cam.forward();
        for p in self.pieces.iter().filter(|p| p.blend == blend_pass) {
            let d = gfx3d::sub(p.center, cam.eye);
            let dist = gfx3d::dot(d, d).sqrt();
            if dist - p.radius > far || gfx3d::dot(d, fwd) < -p.radius {
                continue;
            }
            let tex = p
                .texture
                .and_then(|t| self.textures.get(t))
                .and_then(|t| t.as_ref());
            gfx3d::draw_mesh(
                frame,
                cam,
                &MeshRef {
                    pos: &p.mesh.pos,
                    color: &p.mesh.color,
                    uv: &p.mesh.uv,
                    tris: &p.mesh.tris,
                    offset: p.offset,
                    texture: tex,
                    blend: p.blend,
                    tint: [1.0; 3],
                },
            );
        }
    }

    /// Jogador: modelo animado (skinning por pesos) ou, se não carregou, a caixa provisória.
    fn draw_player(&mut self, cam: &Camera) {
        if let Some(m) = &self.model {
            let (s, c) = self.player.yaw.to_radians().sin_cos();
            let p = self.player.pos;
            let skinned = m.body.skin(&m.motion, m.frame);
            for (mesh, verts) in m.body.meshes.iter().zip(skinned) {
                let pos: Vec<V3> = verts
                    .iter()
                    .map(|v| {
                        [
                            p[0] + v[0] * c + v[2] * s,
                            p[1] + v[1],
                            p[2] - v[0] * s + v[2] * c,
                        ]
                    })
                    .collect();
                let colors = vec![[1.0; 4]; pos.len()];
                let tex = mesh
                    .texture
                    .and_then(|t| m.body.textures.get(t))
                    .and_then(|t| t.as_ref());
                gfx3d::draw_mesh(
                    &mut self.frame,
                    cam,
                    &MeshRef {
                        pos: &pos,
                        color: &colors,
                        uv: &mesh.uv,
                        tris: &mesh.tris,
                        offset: [0.0; 3],
                        texture: tex,
                        blend: false,
                        tint: [1.0; 3],
                    },
                );
            }
            return;
        }
        let (unit, tris) = gfx3d::unit_box();
        let (s, c) = self.player.yaw.to_radians().sin_cos();
        let place = |sx: f32, sy: f32, sz: f32, ox: f32, oz: f32| -> Vec<V3> {
            unit.iter()
                .map(|p| {
                    let (x, y, z) = (p[0] * sx + ox, p[1] * sy, p[2] * sz + oz);
                    [
                        self.player.pos[0] + x * c + z * s,
                        self.player.pos[1] + y,
                        self.player.pos[2] - x * s + z * c,
                    ]
                })
                .collect()
        };
        for (pos, color) in [
            (place(5.0, 11.0, 4.0, 0.0, 0.0), [1.0, 0.55, 0.15, 1.0]),
            (place(2.0, 2.0, 3.0, 0.0, 3.5), [1.0, 1.0, 1.0, 1.0]),
        ] {
            let colors = vec![color; pos.len()];
            let uv = vec![[0.0; 2]; pos.len()];
            gfx3d::draw_mesh(
                &mut self.frame,
                cam,
                &MeshRef {
                    pos: &pos,
                    color: &colors,
                    uv: &uv,
                    tris: &tris,
                    offset: [0.0; 3],
                    texture: None,
                    blend: false,
                    tint: [1.0; 3],
                },
            );
        }
    }
}

/// Mais de 5% dos texels com alfa intermediário (10..245) → trata como translúcida.
fn translucent(img: &Image) -> bool {
    let semi = img
        .pixels
        .iter()
        .filter(|p| (10..245).contains(&(*p >> 24)))
        .count();
    semi * 20 > img.pixels.len()
}

/// Triângulos quase verticais da malha de colisão viram segmentos no plano XZ.
fn collect_walls(mesh: &Mesh, offset: V3, out: &mut Vec<([f32; 2], [f32; 2])>) {
    for t in &mesh.tris {
        let p = t.map(|i| {
            let v = mesh.pos[i as usize];
            [v[0] + offset[0], v[1] + offset[1], v[2] + offset[2]]
        });
        let n = gfx3d::cross(gfx3d::sub(p[1], p[0]), gfx3d::sub(p[2], p[0]));
        let l = gfx3d::dot(n, n).sqrt();
        if l < 1e-6 || (n[1] / l).abs() > 0.3 {
            continue;
        }
        // só paredes que atravessam a altura do jogador
        let (lo, hi) = p
            .iter()
            .fold((f32::MAX, f32::MIN), |(l, h), v| (l.min(v[1]), h.max(v[1])));
        if lo > 8.0 || hi < 1.0 {
            continue;
        }
        for k in 0..3 {
            let (a, b) = (p[k], p[(k + 1) % 3]);
            if (a[0] - b[0]).hypot(a[2] - b[2]) > 0.01 {
                out.push(([a[0], a[2]], [b[0], b[2]]));
            }
        }
    }
}

/// Triângulos quase horizontais (ny > 0.3: chão e rampa) da malha de colisão, em coordenadas
/// de mundo — é a fonte da altura do andar/piso em que o jogador anda.
fn collect_floors(mesh: &Mesh, offset: V3, out: &mut Vec<[[f32; 3]; 3]>) {
    for t in &mesh.tris {
        let p = t.map(|i| {
            let v = mesh.pos[i as usize];
            [v[0] + offset[0], v[1] + offset[1], v[2] + offset[2]]
        });
        let n = gfx3d::cross(gfx3d::sub(p[1], p[0]), gfx3d::sub(p[2], p[0]));
        let l = gfx3d::dot(n, n).sqrt();
        // as listas de colisão (XPVI formato 0) vêm com o sentido invertido: vale |ny|; o teto não atrapalha
        // porque `floor_at` só aceita superfícies dentro da banda de degrau da altura atual
        if l < 1e-6 || (n[1] / l).abs() <= 0.3 {
            continue;
        }
        out.push(p);
    }
}

/// Chão de apoio em (x, z) para um jogador na altura `y`: o mais alto dos triângulos andáveis
/// que cobrem o ponto dentro da banda de degrau [y - MAX_STEP, y + MAX_STEP]. O `cl` de casas
/// é casca grosseira (pode ficar ±1 abaixo/acima do piso visual — ex. -0.918 sob piso 0 na
/// escada de t101i01), então bandas maiores provocam afundar/flutuar; sem apoio = congela.
fn floor_at(floors: &[[[f32; 3]; 3]], x: f32, z: f32, y: f32) -> Option<f32> {
    let mut best: Option<f32> = None;
    for p in floors {
        let (v0x, v0z) = (p[1][0] - p[0][0], p[1][2] - p[0][2]);
        let (v1x, v1z) = (p[2][0] - p[0][0], p[2][2] - p[0][2]);
        let (px, pz) = (x - p[0][0], z - p[0][2]);
        let den = v0x * v1z - v1x * v0z;
        if den.abs() < 1e-9 {
            continue;
        }
        let l1 = (px * v1z - v1x * pz) / den;
        let l2 = (v0x * pz - px * v0z) / den;
        if l1 < -1e-6 || l2 < -1e-6 || l1 + l2 > 1.0 + 1e-6 {
            continue;
        }
        let ty = p[0][1] + l1 * (p[1][1] - p[0][1]) + l2 * (p[2][1] - p[0][1]);
        if (ty - y).abs() <= MAX_STEP && best.is_none_or(|b| ty > b) {
            best = Some(ty);
        }
    }
    best
}

/// Chão em (x, z) mais próximo da altura `y` (até `range`).
fn floor_nearest(floors: &[[[f32; 3]; 3]], x: f32, z: f32, y: f32, range: f32) -> Option<f32> {
    let mut best: Option<f32> = None;
    for p in floors {
        let (v0x, v0z) = (p[1][0] - p[0][0], p[1][2] - p[0][2]);
        let (v1x, v1z) = (p[2][0] - p[0][0], p[2][2] - p[0][2]);
        let (px, pz) = (x - p[0][0], z - p[0][2]);
        let den = v0x * v1z - v1x * v0z;
        if den.abs() < 1e-9 {
            continue;
        }
        let l1 = (px * v1z - v1x * pz) / den;
        let l2 = (v0x * pz - px * v0z) / den;
        if l1 < -1e-6 || l2 < -1e-6 || l1 + l2 > 1.0 + 1e-6 {
            continue;
        }
        let ty = p[0][1] + l1 * (p[1][1] - p[0][1]) + l2 * (p[2][1] - p[0][1]);
        if (ty - y).abs() <= range && best.is_none_or(|b| (ty - y).abs() < (b - y).abs()) {
            best = Some(ty);
        }
    }
    best
}

fn push_out(pos: &mut V3, a: [f32; 2], b: [f32; 2]) {
    let (abx, abz) = (b[0] - a[0], b[1] - a[1]);
    let len2 = abx * abx + abz * abz;
    if len2 < 1e-9 {
        return;
    }
    let t = (((pos[0] - a[0]) * abx + (pos[2] - a[1]) * abz) / len2).clamp(0.0, 1.0);
    let (cx, cz) = (a[0] + abx * t, a[1] + abz * t);
    let (dx, dz) = (pos[0] - cx, pos[2] - cz);
    let d = dx.hypot(dz);
    if d < PLAYER_RADIUS && d > 1e-6 {
        let k = (PLAYER_RADIUS - d) / d;
        pos[0] += dx * k;
        pos[2] += dz * k;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room(id: &str) -> Option<Room> {
        let romfs = crate::romfs::locate(None)?;
        let fa = Archive::open(&romfs.join("yw1_a.fa")).ok()?;
        Some(Room::load(&fa, id).unwrap())
    }

    /// Caixa 3D de cada MAP_JUMP intersecta alguma superfície Andável do `cl` no seu (x, z):
    /// é a semântica volumétrica do gatilho (a BFS da análise em work/notes/collision/ acha
    /// os FPs por célula com y dentro da banda da caixa; deltas vistos: -0.01 .. 1.12).
    #[test]
    fn collision_floors_match_fp_levels() {
        let Some(romfs) = crate::romfs::locate(None) else {
            return;
        };
        let Ok(fa) = Archive::open(&romfs.join("yw1_a.fa")) else {
            return;
        };
        for id in ["t101i01", "t101i02", "t101i03", "t101i04"] {
            let r = Room::load(&fa, id).unwrap();
            // o quarto tem 8 triângulos de chão (malha `snd05-sdw05`): basta não ser vazio
            assert!(!r.floors.is_empty(), "{id}: sem pisos coletados do cl");
            assert!(!r.triggers.is_empty(), "{id}: sem gatilhos");
            for t in &r.triggers {
                let band = t.half_y.max(1.5);
                let hit =
                    r.floors.iter().any(|p| {
                        let covers = {
                            let (v0x, v0z) = (p[1][0] - p[0][0], p[1][2] - p[0][2]);
                            let (v1x, v1z) = (p[2][0] - p[0][0], p[2][2] - p[0][2]);
                            let (px, pz) = (t.center[0] - p[0][0], t.center[2] - p[0][2]);
                            let den = v0x * v1z - v1x * v0z;
                            den.abs() >= 1e-9 && {
                                let l1 = (px * v1z - v1x * pz) / den;
                                let l2 = (v0x * pz - px * v0z) / den;
                                l1 >= -1e-6 && l2 >= -1e-6 && l1 + l2 <= 1.0 + 1e-6
                            }
                        };
                        if !covers {
                            return false;
                        }
                        let ty = (p[0][1] + p[1][1] + p[2][1]) / 3.0;
                        (ty - t.center[1]).abs() <= band
                    }) || floor_at(&r.floors, t.center[0], t.center[2], t.center[1] + MAX_STEP)
                        .is_some_and(|l| (l - t.center[1]).abs() <= band);
                assert!(
                    hit,
                    "{id}: FP {}: nenhuma superfície do cl na altura do gatilho y={:?}",
                    t.jump.name, t.center[1]
                );
            }
        }
    }

    #[test]
    fn debug_floors_near() {
        let Some(romfs) = crate::romfs::locate(None) else {
            return;
        };
        let Ok(fa) = Archive::open(&romfs.join("yw1_a.fa")) else {
            return;
        };
        let b = fa.read("data/map/t101i01/t101i01.xc").expect("xc t101i01");
        let pack = Pack::parse(&b).unwrap();
        let manifest = Manifest::parse(pack.get("RES.bin").unwrap()).unwrap();
        for i in 0..manifest.meshes.len() {
            let Ok(d) = pack.get(&format!("{i:03}.prm")) else {
                continue;
            };
            let mesh = match xmpr::parse(d) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("{i:03}: parse err {e}");
                    continue;
                }
            };
            let hit = mesh.name.contains("cl") || mesh.material_name.contains("col");
            if hit {
                eprintln!(
                    "{i:03}: name={:?} mat={:?} uv={} pos={} tris={}",
                    mesh.name,
                    mesh.material_name,
                    mesh.has_uv,
                    mesh.pos.len(),
                    mesh.tris.len()
                );
                if mesh.material_name.contains("col") && mesh.tris.len() > 300 {
                    let vbat = d.windows(4).position(|w| w == b"XPVB").unwrap();
                    let viat = d.windows(4).position(|w| w == b"XPVI").unwrap();
                    let wt = |base: usize, o: usize| -> usize {
                        u16le(&d[base..], o).map(u32::from).unwrap_or(0) as usize
                    };
                    let vb = xpvb::VertexBuffer::parse(&d[vbat..]).unwrap();
                    let rawvb = l5::decompress(&d[vbat + wt(vbat, 8)..]).unwrap_or_default();
                    let rawvi = l5::decompress(&d[viat + wt(viat, 6)..]).unwrap_or_default();
                    let vicnt = u32le(&d[viat..], 8).unwrap_or(0) as usize;
                    let vstride = u16le(&d[vbat..], 10).unwrap_or(0) as usize;
                    let vcount = u32le(&d[vbat..], 12).unwrap_or(0) as usize;
                    eprintln!(
                        "     [raw] vb stride={vstride} count={vcount} dec={} | vi off={} fmt={} icnt={vicnt} dec={} | attrs={:?}",
                        rawvb.len(),
                        wt(viat, 6),
                        u16le(&d[viat..], 4).unwrap_or(0),
                        rawvi.len(),
                        vb.attributes
                            .iter()
                            .map(|a| a
                                .as_ref()
                                .map(|a| (a.kind, a.offset, a.size, a.constant)))
                            .collect::<Vec<_>>()
                    );
                    let ih: Vec<u16> = xpvb::parse_indices(&d[viat..]).unwrap();
                    eprintln!(
                        "     [raw] strip len={} head={:?}",
                        ih.len(),
                        &ih[..16.min(ih.len())]
                    );
                    let p0: Vec<String> = mesh
                        .pos
                        .iter()
                        .take(4)
                        .map(|p| format!("[{:.2},{:.2},{:.2}]", p[0], p[1], p[2]))
                        .collect();
                    eprintln!("     [raw] pos0: {}", p0.join(" "));
                    let deg = mesh
                        .tris
                        .iter()
                        .filter(|t| t[0] == t[1] || t[1] == t[2] || t[0] == t[2])
                        .count();
                    eprintln!("     [raw] tris degenerados: {deg}");
                }
                if mesh.material_name.contains("col") {
                    let mut ny_hist: Vec<String> = Vec::new();
                    let mut cls = [0usize; 4]; // chao, teto, rampa, parede/degen
                    let mut deg = 0usize;
                    for t in mesh.tris.iter().take(2000) {
                        let p: Vec<[f32; 3]> = t
                            .iter()
                            .map(|&k| {
                                let v = mesh.pos[k as usize];
                                [v[0], v[1], v[2]]
                            })
                            .collect();
                        let (ax, ay, az) = (p[0][0], p[0][1], p[0][2]);
                        let (bx, by, bz) = (p[1][0], p[1][1], p[1][2]);
                        let (cx, cy, cz) = (p[2][0], p[2][1], p[2][2]);
                        let n = [
                            (by - ay) * (cz - az) - (bz - az) * (cy - ay),
                            (bz - az) * (cx - ax) - (bx - ax) * (cz - az),
                            (bx - ax) * (cy - ay) - (by - ay) * (cx - ax),
                        ];
                        let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
                        if l < 1e-9 {
                            deg += 1;
                            continue;
                        }
                        let ny = n[1] / l;
                        if ny > 0.7 {
                            cls[0] += 1;
                        } else if ny < -0.7 {
                            cls[1] += 1;
                        } else if ny.abs() > 0.3 {
                            cls[2] += 1;
                        } else {
                            cls[3] += 1;
                        }
                        if ny_hist.len() < 20 {
                            ny_hist.push(format!("ny={:.3}", ny));
                        }
                    }
                    eprintln!(
                        "     cls chao={} teto={} rampa={} parede={} degen={}",
                        cls[0], cls[1], cls[2], cls[3], deg
                    );
                    eprintln!("     amostra: {}", ny_hist.join(" "));
                }
            }
        }
    }

    #[test]
    fn bedroom_loads_with_expected_camera() {
        let Some(r) = room("t101i02") else { return };
        // mapenv (docs): Fov 35, Dist 80, RotV 45.
        assert_eq!(
            (r.camera.fov, r.camera.dist, r.camera.rot_v),
            (35.0, 80.0, 45.0)
        );
        assert!(r.triangles() > 1000, "triângulos: {}", r.triangles());
        assert!(r.walls() > 4);
        // Escada: funcpt real do quarto.
        let j = r.jumps();
        assert_eq!(j.len(), 1, "{j:?}");
        assert_eq!(j[0].to, "t101i01");
        assert_eq!(j[0].arrival, [23.5, 0.0, 26.0]);
    }

    /// Do patamar (chegada vinda do térreo) andando para a escada, sem janela: tem de levar ao térreo.
    #[test]
    fn walking_down_the_stairs_reaches_ground_floor() {
        let Some(mut r) = room("t101i02") else { return };
        let mut input = crate::input::Input::headless();
        let mut hit = None;
        // A escada fica em +z do patamar; com a câmera em +z, isso é "baixo" no direcional.
        r.place([26.0, 0.0, -41.0], 0.0);
        for _ in 0..600 {
            input.set(crate::input::Button::Down, true);
            if let Some(j) = r.tick(&input) {
                hit = Some(j);
                break;
            }
        }
        let j = hit.unwrap_or_else(|| panic!("não chegou na escada; parou em {:?}", r.player.pos));
        assert_eq!(j.to, "t101i01");
    }

    /// Renderiza o quarto sem janela em várias resoluções e grava RGBA cru em work/room/.
    /// Manual: `cargo test --release -- --ignored render_room_snapshots`.
    #[test]
    #[ignore]
    fn render_room_snapshots() {
        let Some(mut r) = room("t101i02") else { return };
        let romfs = crate::romfs::locate(None).unwrap();
        let fa = Archive::open(&romfs.join("yw1_a.fa")).unwrap();
        r.model = PlayerModel::load(&fa, 1).ok();
        if let Some(m) = r.model.as_mut() {
            m.set_clip("歩き1L");
            for _ in 0..10 {
                m.advance();
            }
        }
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../work/room");
        std::fs::create_dir_all(&dir).unwrap();
        for scale in [1usize, 3] {
            let f = r.draw(scale);
            let raw: Vec<u8> = f
                .color
                .iter()
                .flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, 255])
                .collect();
            std::fs::write(
                dir.join(format!("rust_room_x{scale}_{}x{}.rgba", f.width, f.height)),
                raw,
            )
            .unwrap();
        }
        if let Some(mut g) = room("t101i01") {
            g.place([23.5, 0.0, 26.0], 0.0);
            let f = g.draw(2);
            let raw: Vec<u8> = f
                .color
                .iter()
                .flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, 255])
                .collect();
            std::fs::write(
                dir.join(format!(
                    "rust_t101i01_arrival_{}x{}.rgba",
                    f.width, f.height
                )),
                raw,
            )
            .unwrap();
        }
        r.orbit = 90.0;
        let f = r.draw(2);
        let raw: Vec<u8> = f
            .color
            .iter()
            .flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, 255])
            .collect();
        std::fs::write(
            dir.join(format!("rust_room_orbit90_{}x{}.rgba", f.width, f.height)),
            raw,
        )
        .unwrap();
    }
}

#[cfg(test)]
mod exit_tests {
    use super::*;

    /// Busca em largura numa grade de 1 unidade com o raio do jogador: a saída tem de ser alcançável a partir
    /// do ponto de chegada. (Com a colisão lida como strip em vez de lista, o pé da escada ficava preso.)
    fn reachable(r: &Room, start: [f32; 2], goal: [f32; 2]) -> f32 {
        let clear = |x: f32, z: f32| {
            r.walls.iter().all(|(a, b)| {
                let (abx, abz) = (b[0] - a[0], b[1] - a[1]);
                let l2 = abx * abx + abz * abz;
                let t = if l2 < 1e-9 {
                    0.0
                } else {
                    (((x - a[0]) * abx + (z - a[1]) * abz) / l2).clamp(0.0, 1.0)
                };
                (x - a[0] - abx * t).hypot(z - a[1] - abz * t) >= PLAYER_RADIUS
            })
        };
        let (x0, z0, n) = (start[0] - 150.0, start[1] - 150.0, 300usize);
        let cell = |i: usize, j: usize| (x0 + i as f32, z0 + j as f32);
        let mut seen = vec![false; n * n];
        let s = (150usize, 150usize);
        seen[s.1 * n + s.0] = true;
        let mut q = std::collections::VecDeque::from([s]);
        let mut best = f32::MAX;
        while let Some((i, j)) = q.pop_front() {
            let (x, z) = cell(i, j);
            best = best.min((x - goal[0]).hypot(z - goal[1]));
            for (di, dj) in [(1i32, 0i32), (-1, 0), (0, 1), (0, -1)] {
                let (ni, nj) = (i as i32 + di, j as i32 + dj);
                if ni < 0 || nj < 0 || ni >= n as i32 || nj >= n as i32 {
                    continue;
                }
                let (ni, nj) = (ni as usize, nj as usize);
                let (x, z) = cell(ni, nj);
                if !seen[nj * n + ni] && clear(x, z) {
                    seen[nj * n + ni] = true;
                    q.push_back((ni, nj));
                }
            }
        }
        best
    }

    #[test]
    fn house_exits_are_reachable() {
        let Some(romfs) = crate::romfs::locate(None) else {
            return;
        };
        let fa = Archive::open(&romfs.join("yw1_a.fa")).unwrap();
        // térreo: do pé da escada (chegada vinda do quarto) até a porta da rua
        let r = Room::load(&fa, "t101i01").unwrap();
        let door = r.triggers.iter().find(|t| t.jump.to == "t101g00").unwrap();
        let d = reachable(&r, [23.5, 30.0], [door.center[0], door.center[2]]);
        assert!(d < 2.0, "porta da rua inalcançável (mais perto: {d:.1})");
        // quarto: do ponto inicial do jogo até a escada
        let r = Room::load(&fa, "t101i02").unwrap();
        let stairs = &r.triggers[0];
        let d = reachable(&r, [-10.0, 15.76], [stairs.center[0], stairs.center[2]]);
        assert!(
            d < 2.0,
            "escada do quarto inalcançável (mais perto: {d:.1})"
        );
    }
}

#[cfg(test)]
mod open_map_probe {
    use super::*;

    #[test]
    #[ignore]
    fn open_maps_collision() {
        let romfs = crate::romfs::locate(None).unwrap();
        let fa = Archive::open(&romfs.join("yw1_a.fa")).unwrap();
        for (id, p) in [
            ("t101g00", [490.0f32, 0.0, -348.0]),
            ("t102g00", [693.0, -20.0, -96.0]),
            ("t102g00", [640.0, 0.0, -42.0]),
        ] {
            let r = Room::load(&fa, id).unwrap();
            let f = floor_nearest(&r.floors, p[0], p[2], p[1], 12.0);
            let near = r
                .walls
                .iter()
                .filter(|(a, b)| {
                    ((a[0] + b[0]) / 2.0 - p[0]).hypot((a[1] + b[1]) / 2.0 - p[2]) < 15.0
                })
                .count();
            println!(
                "{id}: paredes {} pisos {} | em {:?}: chão {:?}, paredes a <15: {near}",
                r.walls.len(),
                r.floors.len(),
                p,
                f
            );
        }
    }
}
