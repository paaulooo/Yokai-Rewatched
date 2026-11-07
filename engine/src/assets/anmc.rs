//! `ANMC00` (`RES.bin` dos layouts de menu, comprimido em L5): layout 2D + animação por *flipbook*.
//!
//! Decifrado contra `data/menu/title_{u,d}00_en.xa` (ver docs/FORMATS.md). Palavras 5..0x18 do
//! cabeçalho: 20 u32 = (quantidade << 16 | offset em palavras), alternando "seção" e "(tipo, bytes por
//! entrada)". Seções usadas: 0 texturas, 2 materiais, 8 malhas, 12 nós, 14 keyframes, 16 instâncias,
//! 18 tabela de strings. Hashes = CRC-32 dos nomes; a malha `k` do nó `nome` tem hash
//! `crc32("#nome k")`. Keyframes são consumidos em ordem pelas instâncias (soma bate com o total).

use super::{cstr, f32le, l5, u32le, Result};

pub struct Material {
    pub hash: u32,
    pub texture: u32,
}

pub struct Mesh {
    pub hash: u32,
    pub material: u32,
}

pub struct Node {
    pub hash: u32,
    pub name: String,
    pub mesh_count: usize,
}

pub struct Key {
    pub node: u32,
    pub frames: u32,
    pub offset: [f32; 3],
}

pub struct Instance {
    pub name: String,
    pub position: [f32; 3],
    pub keys: Vec<Key>,
}

pub struct Anmc {
    pub textures: Vec<u32>,
    pub materials: Vec<Material>,
    pub meshes: Vec<Mesh>,
    pub nodes: Vec<Node>,
    pub instances: Vec<Instance>,
}

impl Anmc {
    pub fn parse(compressed: &[u8]) -> Result<Anmc> {
        let d = l5::decompress(compressed)?;
        if d.get(0..6) != Some(b"ANMC00".as_slice()) {
            return Err("não é um ANMC00".into());
        }
        let sec = |i: usize| -> Result<(usize, usize)> {
            let v = u32le(&d, (5 + i) * 4)?;
            Ok(((v & 0xffff) as usize * 4, (v >> 16) as usize))
        };
        let stride = |i: usize| -> Result<usize> { Ok(sec(i + 1)?.1) };
        let w = |o: usize| u32le(&d, o);
        let strings = sec(18)?.0;
        let name_at = |o: usize| cstr(&d, strings + o);

        let (o, n) = sec(0)?;
        let textures = (0..n)
            .map(|i| w(o + i * stride(0)?))
            .collect::<Result<_>>()?;
        let (o, n) = sec(2)?;
        let materials = (0..n)
            .map(|i| {
                let e = o + i * stride(2)?;
                Ok(Material {
                    hash: w(e)?,
                    texture: w(e + 16)?,
                })
            })
            .collect::<Result<_>>()?;
        let (o, n) = sec(8)?;
        let meshes = (0..n)
            .map(|i| {
                let e = o + i * stride(8)?;
                Ok(Mesh {
                    hash: w(e)?,
                    material: w(e + 12)?,
                })
            })
            .collect::<Result<_>>()?;
        let (o, n) = sec(12)?;
        let nodes = (0..n)
            .map(|i| {
                let e = o + i * stride(12)?;
                let v = w(e + 4)?;
                Ok(Node {
                    hash: w(e)?,
                    name: name_at((v & 0xffff) as usize)?,
                    mesh_count: (v >> 16) as usize,
                })
            })
            .collect::<Result<_>>()?;

        let (ko, kn) = sec(14)?;
        let ks = stride(14)?;
        let mut next_key = 0;
        let (o, n) = sec(16)?;
        let mut instances = Vec::with_capacity(n);
        for i in 0..n {
            let e = o + i * stride(16)?;
            let v = w(e + 4)?;
            let count = (v >> 16) as usize;
            if next_key + count > kn {
                return Err("ANMC: keyframes insuficientes".into());
            }
            let keys = (next_key..next_key + count)
                .map(|k| {
                    let e = ko + k * ks;
                    Ok(Key {
                        node: w(e)?,
                        frames: w(e + 4)?,
                        offset: [f32le(&d, e + 8)?, f32le(&d, e + 12)?, f32le(&d, e + 16)?],
                    })
                })
                .collect::<Result<_>>()?;
            next_key += count;
            instances.push(Instance {
                name: name_at((v & 0xffff) as usize)?,
                position: [f32le(&d, e + 12)?, f32le(&d, e + 16)?, f32le(&d, e + 20)?],
                keys,
            });
        }
        Ok(Anmc {
            textures,
            materials,
            meshes,
            nodes,
            instances,
        })
    }
}
