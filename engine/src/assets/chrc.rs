//! `RES.bin` (`CHRC00` em `.xc` de mapa/personagem, `RESC01` em bibliotecas de textura `.xr`): manifesto com
//! tabelas identificadas por TAG. Todo hash = CRC-32 do nome; a ordem de cada tabela = numeração dos arquivos
//! `NNN.prm/.mbn/.xi`. Tags conferidas no quarto, no Nate e num bloco da rua (`t101g45.xc`):
//!   220 nomes de material (8 B) · 230 cópia · 240 texturas (20 B: hash, nome, flags) · 290 registros de material
//!   (224 B, +16 = hash da textura) · 100 malhas · 110 nós (`.mbn`) · 140 `.txp` · 200 atributos.
//! Blocos da rua não têm a 240: as texturas ficam no `.xr` do mapa (`t1010(_en).xr`, `RESC01` com tag 240).

use super::{cstr, l5, u16le, u32le, Result};

pub struct Manifest {
    pub materials: Vec<u32>,
    pub textures: Vec<u32>,
    /// Por material (mesma ordem de `materials`): hash da textura.
    pub material_texture: Vec<u32>,
    pub meshes: Vec<u32>,
    pub nodes: Vec<u32>,
    /// Bibliotecas de textura externas (tag 20: nomes de mapa, ex. `t100g00`, `t101g00`).
    pub libraries: Vec<String>,
}

impl Manifest {
    pub fn parse(res_bin: &[u8]) -> Result<Manifest> {
        let buf = l5::decompress(res_bin)?;
        let magic = buf.get(0..6).ok_or("RES.bin vazio")?;
        if magic != b"CHRC00" && magic != b"RESC01" {
            return Err("RES.bin não é CHRC00/RESC01".into());
        }
        let hdr = |i: usize| u16le(&buf, 8 + 2 * i).map(|v| v as usize);
        // (tag → (offset, n, tamanho da entrada)) dos diretórios A e B.
        let mut tabs: Vec<(u16, usize, usize, usize)> = Vec::new();
        for (off, n) in [(hdr(2)?, hdr(3)?), (hdr(4)?, hdr(5)?)] {
            for i in 0..n {
                let e = off * 4 + 8 * i;
                tabs.push((
                    u16le(&buf, e + 4)?,
                    u16le(&buf, e)? as usize * 4,
                    u16le(&buf, e + 2)? as usize,
                    u16le(&buf, e + 6)? as usize,
                ));
            }
        }
        let table = |tag: u16, field: usize| -> Result<Vec<u32>> {
            match tabs.iter().find(|t| t.0 == tag) {
                Some(&(_, off, n, esz)) => (0..n)
                    .map(|i| u32le(&buf, off + esz.max(8) * i + field))
                    .collect(),
                None => Ok(Vec::new()),
            }
        };
        let strings = hdr(0)? * 4;
        let libraries = table(20, 4)?
            .into_iter()
            .filter_map(|o| cstr(&buf, strings + o as usize).ok())
            .collect();
        Ok(Manifest {
            libraries,
            materials: table(220, 0)?,
            textures: table(240, 0)?,
            material_texture: table(290, 16)?,
            meshes: table(100, 0)?,
            nodes: table(110, 0)?,
        })
    }

    /// Hash da textura usada pelo material `material`.
    pub fn texture_hash_of_material(&self, material: u32) -> Option<u32> {
        let m = self.materials.iter().position(|&h| h == material)?;
        self.material_texture.get(m).copied()
    }

    /// Índice da textura local (`NNN.xi`) usada pelo material de hash `material`.
    pub fn texture_of_material(&self, material: u32) -> Option<usize> {
        let th = self.texture_hash_of_material(material)?;
        self.textures.iter().position(|&h| h == th)
    }
}
