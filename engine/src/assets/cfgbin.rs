//! `cfg.bin` (T2B) da Level-5: tabela de entradas `nome(crc32) + parâmetros tipados`. Usado por `*_mapenv.bin`,
//! `*_funcpt.bin`, `*.npcbin`, `data/res/**`. Porte de `tools/map_probe.py::parse_cfg` (validado lá: os nomes
//! batem com o CRC-32 em todos os arquivos testados). Formato em `docs/FORMATS.md`.

use super::{cstr, u32le, Result};

#[derive(Clone, Debug)]
pub enum Param {
    Str(String),
    Int(i32),
    Float(f32),
}

impl Param {
    pub fn as_f32(&self) -> Option<f32> {
        match self {
            Param::Int(i) => Some(*i as f32),
            Param::Float(f) => Some(*f),
            Param::Str(_) => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Param::Str(s) => Some(s),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub params: Vec<Param>,
}

fn align(x: usize, a: usize) -> usize {
    x.div_ceil(a) * a
}

pub fn parse(d: &[u8]) -> Result<Vec<Entry>> {
    let n = u32le(d, 0)? as usize;
    let str_off = u32le(d, 4)? as usize;
    let str_len = u32le(d, 8)? as usize;
    let strings = d
        .get(str_off..str_off + str_len)
        .ok_or("cfg.bin: strings truncadas")?;
    let kt = align(str_off + str_len, 16);
    let name_count = u32le(d, kt + 4)? as usize;
    let names_off = u32le(d, kt + 8)? as usize;
    let mut names = std::collections::HashMap::new();
    for i in 0..name_count {
        let crc = u32le(d, kt + 16 + 8 * i)?;
        let off = u32le(d, kt + 20 + 8 * i)? as usize;
        names.insert(crc, cstr(d, kt + names_off + off)?);
    }
    let mut out = Vec::with_capacity(n);
    let mut p = 16;
    for _ in 0..n {
        let crc = u32le(d, p)?;
        let count = *d.get(p + 4).ok_or("cfg.bin: entrada truncada")? as usize;
        let types_len = count.div_ceil(4);
        let types = d
            .get(p + 5..p + 5 + types_len)
            .ok_or("cfg.bin: tipos truncados")?;
        p += align(5 + types_len, 4);
        let mut params = Vec::with_capacity(count);
        for i in 0..count {
            let raw = u32le(d, p)?;
            p += 4;
            params.push(match (types[i / 4] >> (2 * (i % 4))) & 3 {
                0 if raw == u32::MAX => Param::Str(String::new()),
                0 => Param::Str(cstr(strings, raw as usize)?),
                1 => Param::Int(raw as i32),
                _ => Param::Float(f32::from_bits(raw)),
            });
        }
        out.push(Entry {
            name: names
                .get(&crc)
                .cloned()
                .unwrap_or_else(|| format!("{crc:08x}")),
            params,
        });
    }
    Ok(out)
}

/// Câmera do mapa (`CAMERA` dentro do `PTREE` do `*_mapenv.bin`). Unidades: graus e unidades de mundo.
/// Significado exato dos campos (FOV vertical? `Dist` até o alvo?) é HIPÓTESE — ver `room.rs`.
#[derive(Clone, Copy, Debug)]
pub struct MapCamera {
    pub fov: f32,
    pub dist: f32,
    pub rot_h: f32,
    pub rot_v: f32,
    pub near: f32,
    pub far: f32,
}

impl Default for MapCamera {
    fn default() -> Self {
        MapCamera {
            fov: 35.0,
            dist: 80.0,
            rot_h: 0.0,
            rot_v: 45.0,
            near: 10.0,
            far: 1000.0,
        }
    }
}

pub fn map_camera(entries: &[Entry]) -> MapCamera {
    let mut cam = MapCamera::default();
    let mut stack: Vec<&str> = Vec::new();
    for e in entries {
        match e.name.as_str() {
            "PTREE" => stack.push(e.params.first().and_then(Param::as_str).unwrap_or("")),
            "_PTREE" => {
                stack.pop();
            }
            "PTVAL" if stack.last() == Some(&"CAMERA") => {
                let value = e.params.first().and_then(Param::as_f32);
                let name = e.params.get(1).and_then(Param::as_str);
                if let (Some(v), Some(n)) = (value, name) {
                    match n {
                        "Fov" => cam.fov = v,
                        "Dist" => cam.dist = v,
                        "RotH" => cam.rot_h = v,
                        "RotV" => cam.rot_v = v,
                        "Near" => cam.near = v,
                        "Far" => cam.far = v,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    cam
}
