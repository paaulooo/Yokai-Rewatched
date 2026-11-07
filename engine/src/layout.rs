//! Layout 2D de menu (XPCK com `RES.bin` = ANMC00, `NNN.xi`, `000.pvb`, `NNN.pbi`) e reprodução
//! dos *flipbooks* das instâncias. Durações em quadros de 60 Hz.
//!
//! Posição na tela (origem no canto superior esquerdo, y para baixo) =
//! posição da instância + deslocamento do keyframe + coordenada local da malha. Conferido nas duas
//! telas do título (ex.: botão 1 = (78,94) + (82,0) + local [-83..83]×[0..55] → centrado na tela de baixo).
//!
//! HIPÓTESE (não lida no código do jogo): ordem de desenho = z decrescente (mais longe primeiro);
//! empate → instância declarada depois fica atrás. É o que põe `main_bg` (última instância) atrás dos botões.

use crate::assets::anmc::{Anmc, Key};
use crate::assets::{crc32, imgc, xpck::Pack, xpvb, Result};
use crate::render2d::{draw_triangle, draw_triangle_flat, Target, Vertex};
use std::collections::HashMap;

struct Tri {
    pos: [[f32; 3]; 3],
    uv: [[f32; 2]; 3],
}

struct Mesh {
    tris: Vec<Tri>,
    /// Índice em `textures`; `None` = malha do material sem textura (desenha cor sólida).
    texture: Option<usize>,
}

pub struct Layout {
    textures: Vec<imgc::Image>,
    meshes: Vec<Mesh>,
    node_meshes: HashMap<u32, Vec<usize>>,
    anmc: Anmc,
}

impl Layout {
    pub fn load(xpck: &[u8]) -> Result<Layout> {
        let pack = Pack::parse(xpck)?;
        let anmc = Anmc::parse(pack.get("RES.bin")?)?;
        let textures = (0..anmc.textures.len())
            .map(|i| imgc::decode(pack.get(&format!("{i:03}.xi"))?))
            .collect::<Result<Vec<_>>>()?;
        let vb = xpvb::VertexBuffer::parse(pack.get("000.pvb")?)?;
        let uv_slot = vb.slot_of_kind(2).ok_or("layout sem UV")?;
        let mut meshes = Vec::with_capacity(anmc.meshes.len());
        for (i, m) in anmc.meshes.iter().enumerate() {
            let material = anmc
                .materials
                .iter()
                .find(|x| x.hash == m.material)
                .ok_or_else(|| format!("malha {i}: material {:08x} ausente", m.material))?;
            // Textura por hash do material; hash 0 = "sem textura" no material real
            // (ex. `#cover 0` em warp_d00_en/mm_quest_d00_en) → desenha cor sólida (ver draw).
            // Hash presente na tabela mas ausente do pacote virou erro que derrubava o layout
            // inteiro; agora fica sem textura e avisa, para não quebrar o resto do menu.
            let texture = if material.texture == 0 {
                None
            } else {
                match anmc.textures.iter().position(|&t| t == material.texture) {
                    Some(k) => Some(k),
                    None => {
                        eprintln!(
                            "layout: textura {:08x} não está no pacote (malha {i}, material {:08x})",
                            material.texture, m.material
                        );
                        None
                    }
                }
            };
            let indices = xpvb::parse_indices(pack.get(&format!("{i:03}.pbi"))?)?;
            let vertex = |k: u16| -> Result<([f32; 3], [f32; 2])> {
                let k = k as usize;
                if k >= vb.count {
                    return Err(format!("malha {i}: índice {k} fora do buffer"));
                }
                let p = [vb.float(0, k, 0), vb.float(0, k, 1), vb.float(0, k, 2)];
                Ok((p, [vb.float(uv_slot, k, 0), vb.float(uv_slot, k, 1)]))
            };
            let mut tris = Vec::with_capacity(indices.len() / 3);
            for t in indices.as_chunks::<3>().0 {
                let (a, b, c) = (vertex(t[0])?, vertex(t[1])?, vertex(t[2])?);
                tris.push(Tri {
                    pos: [a.0, b.0, c.0],
                    uv: [a.1, b.1, c.1],
                });
            }
            meshes.push(Mesh { tris, texture });
        }
        let by_hash: HashMap<u32, usize> = anmc
            .meshes
            .iter()
            .enumerate()
            .map(|(i, m)| (m.hash, i))
            .collect();
        let node_meshes = anmc
            .nodes
            .iter()
            .map(|n| {
                let list = (0..n.mesh_count)
                    .filter_map(|k| {
                        by_hash
                            .get(&crc32(format!("#{} {k}", n.name).as_bytes()))
                            .copied()
                    })
                    .collect();
                (n.hash, list)
            })
            .collect();
        Ok(Layout {
            textures,
            meshes,
            node_meshes,
            anmc,
        })
    }

    pub fn instance(&self, name: &str) -> Result<usize> {
        self.anmc
            .instances
            .iter()
            .position(|i| i.name == name)
            .ok_or_else(|| format!("instância {name} ausente no layout"))
    }

    pub fn instance_frames(&self, instance: usize) -> u32 {
        self.anmc.instances[instance]
            .keys
            .iter()
            .map(|k| k.frames)
            .sum()
    }

    fn key_at(&self, anim: &Anim) -> Option<&Key> {
        let keys = &self.anmc.instances[anim.instance].keys;
        let total = self.instance_frames(anim.instance).max(1);
        let mut f = if anim.looping {
            anim.frame % total
        } else {
            anim.frame.min(total - 1)
        };
        if anim.reverse {
            f = total - 1 - f;
        }
        for k in keys {
            if f < k.frames {
                return Some(k);
            }
            f -= k.frames;
        }
        keys.last()
    }

    /// Desenha as animações pedidas (ver HIPÓTESE de ordem no topo do arquivo).
    pub fn draw(&self, target: &mut Target, anims: &[&Anim]) {
        let mut list: Vec<(f32, usize, usize, usize, [f32; 3], f32)> = Vec::new();
        for anim in anims {
            if anim.alpha <= 0.0 {
                continue;
            }
            let Some(key) = self.key_at(anim) else {
                continue;
            };
            let base = self.anmc.instances[anim.instance].position;
            let origin = [
                base[0] + key.offset[0] + anim.offset.0,
                base[1] + key.offset[1] + anim.offset.1,
                base[2] + key.offset[2],
            ];
            for &m in self.node_meshes.get(&key.node).into_iter().flatten() {
                for (t, tri) in self.meshes[m].tris.iter().enumerate() {
                    let z = origin[2] + (tri.pos[0][2] + tri.pos[1][2] + tri.pos[2][2]) / 3.0;
                    list.push((z, anim.instance, m, t, origin, anim.alpha));
                }
            }
        }
        list.sort_by(|a, b| {
            b.0.total_cmp(&a.0)
                .then(b.1.cmp(&a.1))
                .then(a.2.cmp(&b.2))
                .then(a.3.cmp(&b.3))
        });
        for (_, _, m, t, o, alpha) in list {
            let mesh = &self.meshes[m];
            let tri = &mesh.tris[t];
            let v = [0, 1, 2].map(|k| Vertex {
                x: o[0] + tri.pos[k][0],
                y: o[1] + tri.pos[k][1],
                u: tri.uv[k][0],
                v: tri.uv[k][1],
            });
            match mesh.texture {
                Some(k) => draw_triangle(target, &self.textures[k], v, alpha.min(1.0)),
                // Material sem textura (#cover 0, usado por bg_black/cover): quadrado sólido.
                // Cor = preto é HIPÓTESE (só confere com os nomes das instâncias); a cor real
                // seria uma propriedade do material .xa ainda não decifrada.
                None => draw_triangle_flat(target, v, alpha.min(1.0), 0xff00_0000),
            };
        }
    }

    pub fn instance_position(&self, instance: usize) -> [f32; 3] {
        self.anmc.instances[instance].position
    }
}

/// Reprodução de uma instância: `frame` em quadros de 60 Hz.
pub struct Anim {
    pub instance: usize,
    pub frame: u32,
    pub looping: bool,
    /// Toca de trás para frente (do último quadro ao primeiro).
    pub reverse: bool,
    /// Opacidade do objeto inteiro (0..1).
    pub alpha: f32,
    /// Deslocamento extra em pixels (o `op5247` dos scripts).
    pub offset: (f32, f32),
}

impl Anim {
    pub fn new(layout: &Layout, name: &str, looping: bool) -> Result<Anim> {
        Ok(Anim {
            instance: layout.instance(name)?,
            frame: 0,
            looping,
            reverse: false,
            alpha: 1.0,
            offset: (0.0, 0.0),
        })
    }

    /// Reinicia em um modo de reprodução de não-repetição.
    pub fn restart(&mut self, reverse: bool, at_end: bool) {
        self.looping = false;
        self.reverse = reverse;
        self.frame = if at_end { u32::MAX } else { 0 };
    }

    pub fn tick(&mut self) {
        self.frame = self.frame.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn title_layouts() -> Option<(Layout, Layout)> {
        let romfs = crate::romfs::locate(None)?;
        let fa = crate::assets::arc0::Archive::open(&romfs.join("yw1_a.fa")).ok()?;
        Some((
            Layout::load(&fa.read("data/menu/title_u00_en.xa").ok()?).unwrap(),
            Layout::load(&fa.read("data/menu/title_d00_en.xa").ok()?).unwrap(),
        ))
    }

    #[test]
    fn title_instances_resolve_meshes() {
        let Some((top, bottom)) = title_layouts() else {
            return;
        };
        for l in [&top, &bottom] {
            for inst in &l.anmc.instances {
                let key = &inst.keys[0];
                let meshes = l.node_meshes.get(&key.node).cloned().unwrap_or_default();
                let tris: usize = meshes.iter().map(|&m| l.meshes[m].tris.len()).sum();
                println!(
                    "{:12} nó {:08x} → malhas {:?} ({} triângulos)",
                    inst.name, key.node, meshes, tris
                );
                assert!(tris > 0, "{} sem triângulos", inst.name);
            }
        }
    }

    /// Layouts cujo material (ex. `#cover 0`) não tem textura DEVEM carregar: antes o hash 0
    /// derrubava o menu inteiro com "material/textura não encontrados". Roda no
    /// `cargo test --release` (usa o RomFS local, se existir).
    #[test]
    fn untextured_materials_load() {
        let Some(romfs) = crate::romfs::locate(None) else {
            return;
        };
        let Ok(fa) = crate::assets::arc0::Archive::open(&romfs.join("yw1_a.fa")) else {
            return;
        };
        for path in ["data/menu/warp_d00_en.xa", "data/menu/mm_quest_d00_en.xa"] {
            let l = Layout::load(&fa.read(path).unwrap())
                .unwrap_or_else(|e| panic!("{path} falhou ao carregar: {e}"));
            let sem_tex = l.meshes.iter().filter(|m| m.texture.is_none()).count();
            assert!(
                sem_tex > 0,
                "{path}: sem malhas sem textura (esperado: #cover)"
            );
            assert!(
                l.meshes.len() > sem_tex,
                "{path}: nada com textura — leitura de material estranha"
            );
            println!("{path}: {}/{} malhas sem textura", sem_tex, l.meshes.len());
        }
    }

    /// Grava texturas e um quadro da tela de cima em RGBA cru em work/title/, para comparar com
    /// tools/imgc.py (bateu pixel a pixel). Manual: `cargo test --release -- --ignored dump_title_raw`.
    #[test]
    #[ignore]
    fn dump_title_raw() {
        let Some((top, _)) = title_layouts() else {
            return;
        };
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../work/title");
        if !dir.is_dir() {
            return;
        }
        let rgba = |px: &[u32]| -> Vec<u8> {
            px.iter()
                .flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, (p >> 24) as u8])
                .collect()
        };
        for (i, t) in top.textures.iter().enumerate() {
            std::fs::write(
                dir.join(format!("rust_u{i}_{}x{}.rgba", t.width, t.height)),
                rgba(&t.pixels),
            )
            .unwrap();
        }
        let mut px = vec![0u32; 400 * 240];
        let mut target = Target {
            pixels: &mut px,
            width: 400,
            height: 240,
        };
        let mut anims = vec![
            Anim::new(&top, "bg00", true).unwrap(),
            Anim::new(&top, "title_00", true).unwrap(),
            Anim::new(&top, "youkai_wa", false).unwrap(),
        ];
        for a in &mut anims {
            a.frame = 60;
        }
        let refs: Vec<&Anim> = anims.iter().collect();
        top.draw(&mut target, &refs);
        std::fs::write(
            dir.join("rust_top_400x240.rgba"),
            rgba(&px.iter().map(|p| p | 0xff00_0000).collect::<Vec<_>>()),
        )
        .unwrap();
    }
}

#[cfg(test)]
mod loading_probe {
    #[test]
    #[ignore]
    fn list_loading_instances() {
        let romfs = crate::romfs::locate(None).unwrap();
        let fa = crate::assets::arc0::Archive::open(&romfs.join("yw1_a.fa")).unwrap();
        let path = std::env::var("YW_PROBE_XA").unwrap_or("data/menu/loading_en.xa".into());
        let l = super::Layout::load(&fa.read(&path).unwrap()).unwrap();
        for (i, inst) in l.anmc.instances.iter().enumerate() {
            println!(
                "{i} {} pos {:?} quadros {}",
                inst.name,
                inst.position,
                l.instance_frames(i)
            );
        }
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../work/loading");
        std::fs::create_dir_all(&dir).unwrap();
        for (name, off) in [
            ("load_base", (0.0, 0.0)),
            ("load_01", (0.0, 0.0)),
            ("load_base_c", (200.0, 120.0)),
            ("load_01_c", (200.0, 120.0)),
        ] {
            let mut px = vec![0xff00_ff00u32; 400 * 240];
            let mut t = crate::render2d::Target {
                pixels: &mut px,
                width: 400,
                height: 240,
            };
            let mut a = super::Anim::new(&l, name.trim_end_matches("_c"), true).unwrap();
            a.frame = 60;
            a.offset = off;
            l.draw(&mut t, &[&a]);
            let raw: Vec<u8> = px
                .iter()
                .flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, 255])
                .collect();
            std::fs::write(dir.join(format!("{name}.rgba")), raw).unwrap();
        }
    }
}
