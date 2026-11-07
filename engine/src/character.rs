//! Personagem 3D com esqueleto e animação: `data/character/<id>/<id>_p00.xc` (malhas XMPR com pesos, `.mbn`,
//! `.xi`) + movimentos `data/character/c000010/c000010_p10.xc` (`000.mtn2` XMTN + clipes `.mtninf` MINF).
//! Protótipo validado em `tools/mtn_probe.py` (Nate andando com a textura certa: work/chara/nate_anim.png).
//!
//! Formatos (ver docs/FORMATS.md, "Personagens"):
//! * `.mbn` (168 B): hash, hash do pai, flags, translação local (0x0c), rotação local 3×3 em convenção LINHA
//!   (0x18, v' = v·A), escala (0x3c). Composição local→mundo confere com a matriz de mundo gravada (erro 1e-7).
//! * XMPR de personagem: slot 0 posição (espaço do modelo, pose de bind), 4 UV (V direto, sem inverter — nos mapas
//!   é invertido), 7 pesos ×4, 8 índices ×4 numa tabela de hashes de osso (header 0x28 = offset, tamanho).
//! * XMTN: faixas de posição (f32×3), rotação (quatérnion i16/32767) e escala (f32×3); por faixa: i16 osso, u16
//!   (12 bits nº de chaves | 4 bits flag), quadros i16 absolutos na linha do tempo do arquivo inteiro.
//! * MINF: nome Shift-JIS em 0x20, início 0x4c, fim 0x50, velocidade f32 0x54 (0.5 = 30 quadros/s).

use crate::assets::chrc::Manifest;
use crate::assets::imgc::{self, Image};
use crate::assets::xpck::Pack;
use crate::assets::{f32le, l5, u16le, u32le, xpvb, Result};
use crate::gfx3d::V3;
use std::collections::HashMap;

type M3 = [[f32; 3]; 3];

fn mul(a: &M3, b: &M3) -> M3 {
    let mut r = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    r
}

fn vmul(v: V3, m: &M3) -> V3 {
    [
        v[0] * m[0][0] + v[1] * m[1][0] + v[2] * m[2][0],
        v[0] * m[0][1] + v[1] * m[1][1] + v[2] * m[2][1],
        v[0] * m[0][2] + v[1] * m[1][2] + v[2] * m[2][2],
    ]
}

fn inverse(m: &M3) -> M3 {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    let d = if det.abs() < 1e-12 { 1.0 } else { 1.0 / det };
    [
        [
            (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * d,
            (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * d,
            (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * d,
        ],
        [
            (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * d,
            (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * d,
            (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * d,
        ],
        [
            (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * d,
            (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * d,
            (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * d,
        ],
    ]
}

/// Quatérnion (x,y,z,w) → 3×3 em convenção linha (transposta da forma coluna), como no protótipo.
fn quat_row(q: [f32; 4]) -> M3 {
    let [x, y, z, w] = q;
    [
        [
            1.0 - 2.0 * (y * y + z * z),
            2.0 * (x * y + z * w),
            2.0 * (x * z - y * w),
        ],
        [
            2.0 * (x * y - z * w),
            1.0 - 2.0 * (x * x + z * z),
            2.0 * (y * z + x * w),
        ],
        [
            2.0 * (x * z + y * w),
            2.0 * (y * z - x * w),
            1.0 - 2.0 * (x * x + y * y),
        ],
    ]
}

struct Bone {
    hash: u32,
    parent: Option<usize>,
    t: V3,
    r: M3,
    s: V3,
}

pub struct SkinMesh {
    pub pos: Vec<V3>,
    pub uv: Vec<[f32; 2]>,
    pub tris: Vec<[u32; 3]>,
    /// Até 4 (osso, peso) por vértice.
    influences: Vec<[(u16, f32); 4]>,
    pub texture: Option<usize>,
    /// Nome do material (ex. `DefaultLib.c001000_01`) — o protagonista de evento junta menino e menina.
    pub material: String,
    /// Hash do nó (osso) a que a malha pertence.
    pub node: u32,
    /// Malha rígida: nenhum vértice tem peso de osso (planos de efeito, objetos). Os vértices estão no espaço do
    /// PRÓPRIO nó e vão ao mundo pela pose dele; nas malhas com peso eles já estão no espaço do modelo.
    pub rigid: bool,
}

#[derive(Clone, Debug)]
pub struct Clip {
    pub name: String,
    /// Nome em bytes Shift-JIS (como nos scripts).
    pub raw: Vec<u8>,
    pub start: f32,
    pub end: f32,
    pub speed: f32,
}

struct Track {
    frames: Vec<i16>,
    values: Vec<f32>,
    comp: usize,
}

pub struct Motion {
    /// (tipo 1 pos / 2 rot / 3 escala, hash do osso) → faixa
    tracks: HashMap<(u8, u32), Track>,
    pub clips: Vec<Clip>,
}

pub struct Character {
    bones: Vec<Bone>,
    bind: Vec<(M3, V3)>,
    pub meshes: Vec<SkinMesh>,
    pub textures: Vec<Option<Image>>,
}

fn f32s(d: &[u8], o: usize, n: usize) -> Result<Vec<f32>> {
    (0..n).map(|i| f32le(d, o + 4 * i)).collect()
}

impl Character {
    pub fn load(xc: &[u8]) -> Result<Character> {
        let pack = Pack::parse(xc)?;
        let manifest = Manifest::parse(pack.get("RES.bin")?)?;
        let mut bones = Vec::new();
        // Todos os NNN.mbn presentes (o manifesto de objetos de evento lista menos nós do que há).
        let nbones = pack
            .files
            .iter()
            .filter(|(n, _)| n.ends_with(".mbn"))
            .count()
            .max(manifest.nodes.len());
        for i in 0..nbones {
            let Ok(d) = pack.get(&format!("{i:03}.mbn")) else {
                continue;
            };
            let a = f32s(d, 0x18, 9)?;
            bones.push((
                u32le(d, 0)?,
                u32le(d, 4)?,
                Bone {
                    hash: u32le(d, 0)?,
                    parent: None,
                    t: [f32le(d, 0x0c)?, f32le(d, 0x10)?, f32le(d, 0x14)?],
                    r: [[a[0], a[1], a[2]], [a[3], a[4], a[5]], [a[6], a[7], a[8]]],
                    s: [f32le(d, 0x3c)?, f32le(d, 0x40)?, f32le(d, 0x44)?],
                },
            ));
        }
        let index: HashMap<u32, usize> = bones.iter().enumerate().map(|(i, b)| (b.0, i)).collect();
        let bones: Vec<Bone> = bones
            .into_iter()
            .map(|(_, parent, mut b)| {
                b.parent = index.get(&parent).copied();
                b
            })
            .collect();
        let textures = (0..manifest.textures.len())
            .map(|i| pack.get(&format!("{i:03}.xi")).and_then(imgc::decode).ok())
            .collect();
        let mut meshes = Vec::new();
        for i in 0..manifest.meshes.len() {
            // Objetos só de esqueleto (ex. `point_a` de evento) anunciam malhas sem ter o .prm.
            let Ok(d) = pack.get(&format!("{i:03}.prm")) else {
                continue;
            };
            meshes.push(parse_skin_mesh(d, &index, &manifest)?);
        }
        let mut c = Character {
            bind: Vec::new(),
            bones,
            meshes,
            textures,
        };
        c.bind = c.pose(None, 0.0);
        Ok(c)
    }

    /// Pose de mundo (rotação linha, posição) de cada osso; sem movimento = pose local do `.mbn` (bind).
    fn pose(&self, motion: Option<&Motion>, frame: f32) -> Vec<(M3, V3)> {
        let mut w: Vec<(M3, V3)> = Vec::with_capacity(self.bones.len());
        for b in &self.bones {
            let key = |kind: u8| motion.and_then(|m| m.tracks.get(&(kind, b.hash)));
            let t = key(1).map_or(b.t, |tr| {
                let v = tr.sample(frame);
                [v[0], v[1], v[2]]
            });
            let r = key(2).map_or(b.r, |tr| {
                let v = tr.sample(frame);
                quat_row([v[0], v[1], v[2], v[3]])
            });
            let s = key(3).map_or(b.s, |tr| {
                let v = tr.sample(frame);
                [v[0], v[1], v[2]]
            });
            let sr = [
                [r[0][0] * s[0], r[0][1] * s[0], r[0][2] * s[0]],
                [r[1][0] * s[1], r[1][1] * s[1], r[1][2] * s[1]],
                [r[2][0] * s[2], r[2][1] * s[2], r[2][2] * s[2]],
            ];
            w.push(match b.parent {
                Some(p) if p < w.len() => {
                    let (pr, pp) = w[p];
                    let tp = vmul(t, &pr);
                    (mul(&sr, &pr), [tp[0] + pp[0], tp[1] + pp[1], tp[2] + pp[2]])
                }
                _ => (sr, t),
            });
        }
        w
    }

    /// Vértices deformados (espaço do modelo) de cada malha no quadro `frame` do movimento.
    /// Pose de mundo (rotação linha, posição) do osso/nó de hash `node`.
    pub fn node_world(
        &self,
        motion: Option<&Motion>,
        frame: f32,
        node: u32,
    ) -> Option<([[f32; 3]; 3], V3)> {
        let i = self.bones.iter().position(|b| b.hash == node)?;
        self.pose(motion, frame).get(i).copied()
    }

    pub fn skin(&self, motion: &Motion, frame: f32) -> Vec<Vec<V3>> {
        self.skin_with(Some(motion), frame)
    }

    /// Pose de repouso (sem movimento): malhas rígidas ficam no lugar do seu nó.
    pub fn skin_bind(&self) -> Vec<Vec<V3>> {
        self.skin_with(None, 0.0)
    }

    fn skin_with(&self, motion: Option<&Motion>, frame: f32) -> Vec<Vec<V3>> {
        let cur = self.pose(motion, frame);
        // v' = (v − Pb)·Rb⁻¹·Ra + Pa  →  v·M + o
        let mats: Vec<(M3, V3)> = self
            .bind
            .iter()
            .zip(&cur)
            .map(|((rb, pb), (ra, pa))| {
                let m = mul(&inverse(rb), ra);
                let o = vmul(*pb, &m);
                (m, [pa[0] - o[0], pa[1] - o[1], pa[2] - o[2]])
            })
            .collect();
        self.meshes
            .iter()
            .map(|mesh| {
                if mesh.rigid {
                    // espaço do nó → mundo: v·R + t (se o nó não existir, a malha fica como veio)
                    if let Some(i) = self.bones.iter().position(|b| b.hash == mesh.node) {
                        let (r, t) = cur[i];
                        return mesh
                            .pos
                            .iter()
                            .map(|v| {
                                let p = vmul(*v, &r);
                                [p[0] + t[0], p[1] + t[1], p[2] + t[2]]
                            })
                            .collect();
                    }
                    return mesh.pos.clone();
                }
                mesh.pos
                    .iter()
                    .zip(&mesh.influences)
                    .map(|(v, inf)| {
                        let mut acc = [0.0f32; 3];
                        let mut ws = 0.0;
                        for &(b, w) in inf {
                            if w <= 0.0 {
                                continue;
                            }
                            let Some((m, o)) = mats.get(b as usize) else {
                                continue;
                            };
                            let p = vmul(*v, m);
                            for k in 0..3 {
                                acc[k] += w * (p[k] + o[k]);
                            }
                            ws += w;
                        }
                        if ws > 0.0 {
                            [acc[0] / ws, acc[1] / ws, acc[2] / ws]
                        } else {
                            *v
                        }
                    })
                    .collect()
            })
            .collect()
    }
}

fn parse_skin_mesh(d: &[u8], index: &HashMap<u32, usize>, manifest: &Manifest) -> Result<SkinMesh> {
    let base = crate::assets::xmpr::parse(d)?;
    let vb_at = d
        .windows(4)
        .position(|w| w == b"XPVB")
        .ok_or("XMPR sem XPVB")?;
    let vb = xpvb::VertexBuffer::parse(&d[vb_at..])?;
    let (no, ns) = (u32le(d, 0x28)? as usize, u32le(d, 0x2c)? as usize);
    let table: Vec<u32> = (0..ns / 4)
        .map(|i| u32le(d, no + 4 * i))
        .collect::<Result<_>>()?;
    let has = |slot: usize| {
        vb.attributes
            .get(slot)
            .and_then(|a| a.as_ref())
            .is_some_and(|a| !a.constant)
    };
    let rigid = !has(7);
    let influences = (0..vb.count)
        .map(|v| {
            let mut inf = [(0u16, 0.0f32); 4];
            for (k, slot) in inf.iter_mut().enumerate() {
                let w = if has(7) {
                    vb.float(7, v, k)
                } else if k == 0 {
                    1.0
                } else {
                    0.0
                };
                let local = if has(8) {
                    vb.float(8, v, k) as usize
                } else {
                    0
                };
                let bone = table
                    .get(local)
                    .and_then(|h| index.get(h))
                    .copied()
                    .unwrap_or(0);
                *slot = (bone as u16, w);
            }
            inf
        })
        .collect();
    Ok(SkinMesh {
        uv: base.uv,
        pos: base.pos,
        tris: base.tris,
        influences,
        texture: manifest.texture_of_material(base.material_hash).or(Some(0)),
        material: base.material_name.clone(),
        node: base.node_hash,
        rigid,
    })
}

impl Track {
    fn sample(&self, frame: f32) -> Vec<f32> {
        let n = self.frames.len();
        let at = |k: usize| &self.values[k * self.comp..(k + 1) * self.comp];
        if n == 0 {
            return vec![0.0; self.comp];
        }
        if frame <= self.frames[0] as f32 {
            return at(0).to_vec();
        }
        if frame >= self.frames[n - 1] as f32 {
            return at(n - 1).to_vec();
        }
        let k = self.frames.partition_point(|&f| (f as f32) <= frame) - 1;
        let (f0, f1) = (self.frames[k] as f32, self.frames[k + 1] as f32);
        let t = if f1 > f0 {
            (frame - f0) / (f1 - f0)
        } else {
            0.0
        };
        let (a, mut b) = (at(k).to_vec(), at(k + 1).to_vec());
        if self.comp == 4 && a.iter().zip(&b).map(|(x, y)| x * y).sum::<f32>() < 0.0 {
            b.iter_mut().for_each(|x| *x = -*x);
        }
        let mut v: Vec<f32> = a.iter().zip(&b).map(|(x, y)| x + (y - x) * t).collect();
        if self.comp == 4 {
            let l = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
            v.iter_mut().for_each(|x| *x /= l);
        }
        v
    }
}

impl Motion {
    /// Pacote de movimentos (`.xc` com `000.mtn2` e `NNN.mtninf`).
    pub fn load(xc: &[u8]) -> Result<Motion> {
        let pack = Pack::parse(xc)?;
        let m = pack.get("000.mtn2")?;
        if m.get(0..4) != Some(b"XMTN".as_slice()) {
            return Err("000.mtn2 não é XMTN".into());
        }
        let counts = [
            u32le(m, 0x14)? as usize,
            u32le(m, 0x18)? as usize,
            u32le(m, 0x1c)? as usize,
        ];
        let d = l5::decompress(m.get(u32le(m, 0x10)? as usize..).ok_or("XMTN: offset")?)?;
        let (bh, ti, table) = (
            u32le(&d, 0)? as usize,
            u32le(&d, 4)? as usize,
            u32le(&d, 8)? as usize,
        );
        let hashes: Vec<u32> = (0..(ti - bh) / 4)
            .map(|i| u32le(&d, bh + 4 * i))
            .collect::<Result<_>>()?;
        let mut info = Vec::new();
        for i in 0..3 {
            let o = u16le(&d, ti + 2 * i)? as usize;
            info.push((d[o + 1], d[o + 3] as usize)); // (tipo de dado, nº de componentes)
        }
        let mut tracks = HashMap::new();
        let mut entry = 0;
        for (kind, &count) in counts.iter().enumerate() {
            let (dtype, comp) = info[kind];
            for _ in 0..count {
                let e = table + 16 * entry;
                entry += 1;
                let (fo, ko, vo) = (
                    u32le(&d, e)? as usize,
                    u32le(&d, e + 4)? as usize,
                    u32le(&d, e + 8)? as usize,
                );
                let bone = u16le(&d, fo)? as usize;
                let nk = (u16le(&d, fo + 2)? & 0xfff) as usize;
                let frames = (0..nk)
                    .map(|k| u16le(&d, ko + 2 * k).map(|v| v as i16))
                    .collect::<Result<Vec<_>>>()?;
                let values = (0..nk * comp)
                    .map(|k| match dtype {
                        2 => f32le(&d, vo + 4 * k),
                        _ => u16le(&d, vo + 2 * k).map(|v| v as i16 as f32 / 32767.0),
                    })
                    .collect::<Result<Vec<_>>>()?;
                if let Some(&h) = hashes.get(bone) {
                    tracks.insert(
                        (kind as u8 + 1, h),
                        Track {
                            frames,
                            values,
                            comp,
                        },
                    );
                }
            }
        }
        let mut clips = Vec::new();
        let mut names: Vec<&String> = pack
            .files
            .iter()
            .map(|(n, _)| n)
            .filter(|n| n.ends_with(".mtninf"))
            .collect();
        names.sort();
        for n in names {
            let b = pack.get(n)?;
            let raw = &b[0x20..0x44];
            let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
            clips.push(Clip {
                name: decode_sjis(&raw[..end]),
                raw: raw[..end].to_vec(),
                start: u32le(b, 0x4c)? as f32,
                end: u32le(b, 0x50)? as f32,
                speed: f32le(b, 0x54)?,
            });
        }
        Ok(Motion { tracks, clips })
    }

    /// Por nome: UTF-8 conhecido (立ち1L...) ou bytes Shift-JIS em latin-1 vindos de script.
    pub fn clip(&self, name: &str) -> Option<&Clip> {
        let raw = crate::xq::raw_bytes(name);
        self.clips.iter().find(|c| c.name == name || c.raw == raw)
    }
}

/// Shift-JIS mínimo para nomes de clipe: só compara bytes — converte para uma chave legível estável.
/// Os nomes usados pelo port (立ち1L, 歩き1L, 走り1L) são comparados pelos bytes Shift-JIS.
fn decode_sjis(b: &[u8]) -> String {
    // Tabela mínima dos kanji/kana dos clipes usados; o resto vira hexadecimal (não é exibido).
    const KNOWN: [(&[u8], &str); 16] = [
        (&[0x97, 0xa7, 0x82, 0xbf, 0x31, 0x4c], "立ち1L"),
        (&[0x95, 0xe0, 0x82, 0xab, 0x31, 0x4c], "歩き1L"),
        (&[0x91, 0x96, 0x82, 0xe8, 0x31, 0x4c], "走り1L"),
        // batalha (todos os Yo-kai têm o mesmo conjunto de clipes `戦1…`)
        (
            &[0x90, 0xed, 0x31, 0x97, 0xa7, 0x82, 0xbf, 0x31, 0x4c],
            "戦1立ち1L",
        ),
        (
            &[
                0x90, 0xed, 0x31, 0x82, 0xb1, 0x82, 0xa4, 0x82, 0xb0, 0x82, 0xab, 0x31,
            ],
            "戦1こうげき1",
        ),
        (
            &[
                0x90, 0xed, 0x31, 0x82, 0xe6, 0x82, 0xa4, 0x82, 0xb6, 0x82, 0xe3, 0x82, 0xc2, 0x31,
            ],
            "戦1ようじゅつ1",
        ),
        (
            &[
                0x90, 0xed, 0x31, 0x83, 0x4b, 0x81, 0x5b, 0x83, 0x68, 0x31, 0x4c,
            ],
            "戦1ガード1L",
        ),
        (
            &[
                0x90, 0xed, 0x31, 0x83, 0x5f, 0x83, 0x81, 0x81, 0x5b, 0x83, 0x57, 0x31,
            ],
            "戦1ダメージ1",
        ),
        (&[0x90, 0xed, 0x31, 0x8e, 0x80, 0x31], "戦1死1"),
        (&[0x90, 0xed, 0x31, 0x8e, 0x80, 0x31, 0x4c], "戦1死1L"),
        (
            &[0x90, 0xed, 0x31, 0x8f, 0x9f, 0x97, 0x98, 0x31],
            "戦1勝利1",
        ),
        (
            &[0x90, 0xed, 0x31, 0x8f, 0x9f, 0x97, 0x98, 0x31, 0x4c],
            "戦1勝利1L",
        ),
        (
            &[0x90, 0xed, 0x31, 0x82, 0xbd, 0x82, 0xdf, 0x31, 0x4c],
            "戦1ため1L",
        ),
        (
            &[
                0x90, 0xed, 0x31, 0x82, 0xd0, 0x82, 0xc1, 0x82, 0xb3, 0x82, 0xc2, 0x31,
            ],
            "戦1ひっさつ1",
        ),
        (
            &[0x90, 0xed, 0x31, 0x8a, 0xec, 0x82, 0xd1, 0x31],
            "戦1喜び1",
        ),
        (
            &[0x90, 0xed, 0x31, 0x94, 0xe6, 0x82, 0xea, 0x31, 0x4c],
            "戦1疲れ1L",
        ),
    ];
    KNOWN
        .iter()
        .find(|(k, _)| *k == b)
        .map(|(_, s)| s.to_string())
        .unwrap_or_else(|| b.iter().map(|c| format!("{c:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nate() -> Option<(Character, Motion)> {
        let romfs = crate::romfs::locate(None)?;
        let fa = crate::assets::arc0::Archive::open(&romfs.join("yw1_a.fa")).ok()?;
        Some((
            Character::load(&fa.read("data/character/c001000/c001000_p00.xc").ok()?).unwrap(),
            Motion::load(&fa.read("data/character/c000010/c000010_p10.xc").ok()?).unwrap(),
        ))
    }

    #[test]
    fn nate_walk_clip_moves_the_legs() {
        let Some((c, m)) = nate() else { return };
        assert_eq!(c.bones.len(), 23);
        let walk = m.clip("歩き1L").expect("clipe de andar");
        assert_eq!((walk.start, walk.end, walk.speed), (465.0, 491.0, 0.5));
        assert!(m.clip("立ち1L").is_some() && m.clip("走り1L").is_some());
        let a = c.skin(&m, walk.start);
        let b = c.skin(&m, walk.start + 6.0);
        let moved = a
            .iter()
            .flatten()
            .zip(b.iter().flatten())
            .map(|(p, q)| (p[2] - q[2]).abs())
            .fold(0.0, f32::max);
        assert!(moved > 0.5, "nada se mexeu: {moved}");
        // altura do modelo em pé ~15 unidades
        let top = a.iter().flatten().map(|p| p[1]).fold(f32::MIN, f32::max);
        assert!((12.0..18.0).contains(&top), "altura {top}");
    }
}
