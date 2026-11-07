//! Câmera de cutscene (`.xv`, formato XCMA). A cutscene em si roda pelo script real no host de eventos
//! (`event_host.rs`); este módulo só lê os keyframes de câmera.
//!
//! Estrutura lida do script `seq/event/ev01_0005a.xq`: um objeto âncora (`point_a`, só esqueleto, com o
//! próprio movimento) e atores presos a nós dele (`op2110(11046, ator, 'point_a', 'evp_aNN')`), cada um com o
//! movimento do evento (`ev01_0005_c0010_<ator>.xc`); câmera por keyframes (`.xv` → `XCMA`).
//!
//! XCMA (decifrado aqui): 4 descritores em 0x60 (8 B: tipo, tipo de dado, ?, componentes, início, fim) — no
//! 0005: vec3, vec3, escalar, escalar = posição, alvo, FOV (constante 0,785 rad = 45°), rolagem (HIPÓTESE
//! pela ordem e pelos valores). Depois, por faixa: cabeçalho de 16 B `(16, 4, offset dos valores, tamanho)`
//! + bloco L5 com `i16 ?, u16 (12 bits nº de chaves | flag)`, quadros u16 e valores f32.
//!
//! Tempo: a lógica do jogo roda a 30 quadros/s — `WaitFrame 473` = 15,8 s e os movimentos têm velocidade 0.5
//! por tique de 60 Hz. Ainda não: o efeito de céu (`ev010005001000.xc`, materiais animados), o cenário.

use crate::assets::{f32le, l5, u16le, u32le, Result};
use crate::gfx3d::V3;

struct Track {
    frames: Vec<u16>,
    values: Vec<f32>,
    comp: usize,
}

impl Track {
    fn sample(&self, f: f32) -> Vec<f32> {
        let n = self.frames.len();
        let at = |k: usize| self.values[k * self.comp..(k + 1) * self.comp].to_vec();
        if n == 0 {
            return vec![0.0; self.comp];
        }
        if f <= self.frames[0] as f32 {
            return at(0);
        }
        if f >= self.frames[n - 1] as f32 {
            return at(n - 1);
        }
        let k = self.frames.partition_point(|&x| x as f32 <= f) - 1;
        let (f0, f1) = (self.frames[k] as f32, self.frames[k + 1] as f32);
        let t = if f1 > f0 { (f - f0) / (f1 - f0) } else { 0.0 };
        at(k)
            .iter()
            .zip(at(k + 1))
            .map(|(a, b)| a + (b - a) * t)
            .collect()
    }
}

pub struct CameraAnim {
    tracks: Vec<Track>,
    #[allow(dead_code)]
    pub frames: u32,
}

impl CameraAnim {
    pub fn parse_xv(xv: &[u8]) -> Result<CameraAnim> {
        let pack = crate::assets::xpck::Pack::parse(xv)?;
        let c = pack.get("000.cmr2")?;
        if c.get(0..4) != Some(b"XCMA".as_slice()) {
            return Err("000.cmr2 não é XCMA".into());
        }
        let frames = u32le(c, 0x28)?;
        let comps: Vec<usize> = (0..4).map(|i| c[0x60 + 8 * i + 3] as usize).collect();
        let mut tracks = Vec::new();
        let mut pos = 0x88usize;
        for comp in comps {
            // próximo cabeçalho (16, 4, ...) a partir de `pos`
            let at = c[pos..]
                .windows(8)
                .position(|w| w == [16, 0, 0, 0, 4, 0, 0, 0])
                .map(|i| pos + i)
                .ok_or("XCMA: cabeçalho de faixa ausente")?;
            let values_off = u32le(c, at + 8)? as usize;
            let d = l5::decompress(&c[at + 16..])?;
            let nk = (u16le(&d, 2)? & 0xfff) as usize;
            let frames = (0..nk)
                .map(|k| u16le(&d, 4 + 2 * k))
                .collect::<Result<Vec<_>>>()?;
            let values = (0..nk * comp)
                .map(|k| f32le(&d, values_off + 4 * k))
                .collect::<Result<Vec<_>>>()?;
            tracks.push(Track {
                frames,
                values,
                comp,
            });
            pos = at + 16 + 4;
        }
        Ok(CameraAnim { tracks, frames })
    }

    /// (olho, alvo, fov em graus) no quadro `f`.
    pub fn at(&self, f: f32) -> (V3, V3, f32) {
        let v = |i: usize| self.tracks.get(i).map(|t| t.sample(f)).unwrap_or_default();
        let (p, t, fov) = (v(0), v(1), v(2));
        let g = |x: &Vec<f32>, k: usize| x.get(k).copied().unwrap_or(0.0);
        let fov = g(&fov, 0);
        (
            [g(&p, 0), g(&p, 1), g(&p, 2)],
            [g(&t, 0), g(&t, 1), g(&t, 2)],
            if fov > 0.01 { fov.to_degrees() } else { 45.0 },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::arc0::Archive;

    #[test]
    fn camera_track_layout() {
        let Some(romfs) = crate::romfs::locate(None) else {
            return;
        };
        let Ok(fa) = Archive::open(&romfs.join("yw1_a.fa")) else {
            return;
        };
        let cam = CameraAnim::parse_xv(
            &fa.read("data/event/ev01/ev01_0005/ev01_0005_c0010_camera.xv")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(cam.frames, 503);
        let (_, _, fov) = cam.at(100.0);
        assert!((fov - 45.0).abs() < 0.1, "fov {fov}");
    }
}
