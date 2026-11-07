//! Cenário 3D dos menus: a prévia do personagem na seleção de menino/menina (e o que mais um menu pedir).
//!
//! Opcodes usados pelo `CharaMakeMenu` (lidos em `seq/menu/CharaMakeMenu_001e.xq`):
//! - `op3702(recurso, 'data/character/c001008/c001008_p00')` carrega um modelo (`_p00`) ou os movimentos (`_p10`);
//! - `op6002(nome, …)` cria um objeto 3D e devolve o próprio nome como identificador;
//! - `op6800(obj, recurso)` liga o modelo, `op3730(obj, recurso)` liga os movimentos, `op6810/6819(obj, clipe[, marcas])`
//!   tocam um clipe pelo nome (6819 = o próximo, depois do atual);
//! - `op6202(obj, sx, sy, sz)` escala, `op6222/6223(obj, x, y, z, …)` posição, `op6227(obj, rx, ry, rz)` rotação;
//! - câmera: `op4102(0, x, y, z)` posição, `op4122(0, x, y, z)` alvo, `op4142(0, fov em radianos)`.
//!
//! [H] a ordem de desenho entre o 3D e os objetos 2D (o 3D vai por cima de tudo aqui), a escala de tempo da animação e
//! a rotação `6227` em radianos em torno de Y.

use crate::assets::arc0::Archive;
use crate::character::{Character, Motion};
use crate::gfx3d::{self, Camera, Frame, MeshRef, V3};
use std::collections::HashMap;
use std::rc::Rc;

enum Res {
    Model(Rc<Character>),
    Motion(Rc<Motion>),
}

struct Obj3 {
    model: Option<Rc<Character>>,
    motion: Option<Rc<Motion>>,
    clip: Option<usize>,
    next: Option<usize>,
    oneshot: bool,
    frame: f32,
    pos: V3,
    scale: f32,
    yaw: f32,
}

pub struct Menu3d {
    res: HashMap<u32, Res>,
    objs: HashMap<u32, Obj3>,
    eye: V3,
    target: V3,
    fov: f32,
}

/// Cor de "nada desenhado" na camada 3D (não aparece em texturas do jogo).
const KEY: u32 = 0x00fe_00fe;

impl Default for Menu3d {
    fn default() -> Self {
        Menu3d {
            res: HashMap::new(),
            objs: HashMap::new(),
            eye: [0.0, 5.0, 15.0],
            target: [0.0, 5.0, 0.0],
            fov: 40.0,
        }
    }
}

fn clip_index(m: &Motion, name: &str) -> Option<usize> {
    let raw = crate::xq::raw_bytes(name);
    m.clips.iter().position(|c| c.name == name || c.raw == raw)
}

impl Menu3d {
    pub fn load(&mut self, fa: &Archive, key: u32, path: &str) {
        let Ok(bytes) = fa.read(&format!("{path}.xc")) else {
            return;
        };
        if path.ends_with("p10") {
            if let Ok(m) = Motion::load(&bytes) {
                self.res.insert(key, Res::Motion(Rc::new(m)));
            }
        } else if let Ok(c) = Character::load(&bytes) {
            self.res.insert(key, Res::Model(Rc::new(c)));
        }
    }

    pub fn create(&mut self, key: u32) {
        self.objs.insert(
            key,
            Obj3 {
                model: None,
                motion: None,
                clip: None,
                next: None,
                oneshot: false,
                frame: 0.0,
                pos: [0.0; 3],
                scale: 1.0,
                yaw: 0.0,
            },
        );
    }

    pub fn set_model(&mut self, obj: u32, res: u32) {
        if let (Some(o), Some(Res::Model(m))) = (self.objs.get_mut(&obj), self.res.get(&res)) {
            o.model = Some(m.clone());
        }
    }

    pub fn set_motion(&mut self, obj: u32, res: u32) {
        if let (Some(o), Some(Res::Motion(m))) = (self.objs.get_mut(&obj), self.res.get(&res)) {
            o.motion = Some(m.clone());
        }
    }

    /// `queue` = `op6819`: toca o clipe quando o atual terminar (se for um clipe de um tiro).
    pub fn play(&mut self, obj: u32, clip: &str, queue: bool) {
        let Some(o) = self.objs.get_mut(&obj) else {
            return;
        };
        let Some(m) = o.motion.clone() else {
            return;
        };
        let Some(ci) = clip_index(&m, clip) else {
            return;
        };
        if queue && o.clip.is_some() && o.oneshot {
            o.next = Some(ci);
            return;
        }
        o.clip = Some(ci);
        o.frame = m.clips[ci].start;
        o.oneshot = m.clips[ci].raw.last() != Some(&b'L');
        o.next = None;
    }

    pub fn set_scale(&mut self, obj: u32, s: f32) {
        if let Some(o) = self.objs.get_mut(&obj) {
            o.scale = s;
        }
    }

    pub fn set_pos(&mut self, obj: u32, p: V3) {
        if let Some(o) = self.objs.get_mut(&obj) {
            o.pos = p;
        }
    }

    pub fn set_yaw(&mut self, obj: u32, radians: f32) {
        if let Some(o) = self.objs.get_mut(&obj) {
            o.yaw = radians;
        }
    }

    pub fn set_camera(&mut self, eye: Option<V3>, target: Option<V3>, fov: Option<f32>) {
        if let Some(e) = eye {
            self.eye = e;
        }
        if let Some(t) = target {
            self.target = t;
        }
        if let Some(f) = fov {
            self.fov = f;
        }
    }

    /// Um tique do menu (30 Hz): avança as animações.
    pub fn tick(&mut self) {
        for o in self.objs.values_mut() {
            let (Some(m), Some(ci)) = (&o.motion, o.clip) else {
                continue;
            };
            let c = &m.clips[ci];
            let (start, end, speed) = (c.start, c.end, c.speed);
            o.frame += speed * 2.0;
            if o.frame >= end {
                if let (true, Some(n)) = (o.oneshot, o.next.take()) {
                    o.clip = Some(n);
                    o.frame = m.clips[n].start;
                    o.oneshot = m.clips[n].raw.last() != Some(&b'L');
                } else if o.oneshot {
                    o.frame = (end - 0.01).max(start);
                } else {
                    o.frame = start + (o.frame - start).rem_euclid((end - start).max(1.0));
                }
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.objs.values().all(|o| o.model.is_none())
    }

    /// Desenha por cima da tela de cima (400×240).
    pub fn draw(&self, top: &mut [u32], w: usize, h: usize) {
        // `YW_NO_MENU3D=1` desliga a camada (para comparar com e sem o 3D nos testes de imagem)
        if self.is_empty() || std::env::var_os("YW_NO_MENU3D").is_some() {
            return;
        }
        let mut frame = Frame::new(w, h);
        frame.clear(KEY);
        let cam = Camera::look_at(self.eye, self.target, self.fov, 0.5);
        for o in self.objs.values() {
            let Some(body) = &o.model else {
                continue;
            };
            let skinned = match (&o.motion, o.clip) {
                (Some(m), Some(_)) => body.skin(m, o.frame),
                _ => body.skin_bind(),
            };
            let (sn, cs) = o.yaw.sin_cos();
            for (mesh, verts) in body.meshes.iter().zip(skinned) {
                // `Scene_Material` é a caixa de volume do personagem, não se desenha
                if mesh.material.ends_with("Scene_Material") {
                    continue;
                }
                let pos: Vec<V3> = verts
                    .iter()
                    .map(|v| {
                        let (x, y, z) = (v[0] * o.scale, v[1] * o.scale, v[2] * o.scale);
                        [
                            x * cs + z * sn + o.pos[0],
                            y + o.pos[1],
                            -x * sn + z * cs + o.pos[2],
                        ]
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
                        blend: false,
                        tint: [1.0; 3],
                    },
                );
            }
        }
        for (dst, src) in top.iter_mut().zip(&frame.color) {
            if *src != KEY {
                *dst = *src;
            }
        }
    }
}
