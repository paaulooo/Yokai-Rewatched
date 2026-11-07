//! Mixers do DSP (`audio_core/hle/mixers.cpp` e `effects.cpp` do Citra/Azahar): 3 mixers intermediários
//! quadrifônicos (0 = principal, 1 e 2 = barramentos auxiliares, que o jogo pode processar no ARM), o efeito de
//! atraso dos auxiliares e a mixagem final em PCM16 estéreo.

use super::layout::{cfg, rd16, rd32, rdf32, wr16, wr32, SAMPLES_PER_FRAME};
use super::source::{Frame, QuadFrame};

/// Atraso com realimentação: `H(z) = a z^-N / (1 - b z^-1 + a g z^-N)`, N = quadros · 160.
#[derive(Clone, Default)]
pub struct DelayEffect {
    enabled: bool,
    samples: usize,
    g: f32,
    a: f32,
    b: f32,
    line: [Vec<f32>; 4],
    last: [f32; 4],
    pos: usize,
}

const MAX_DELAY_SAMPLES: usize = 4 * 32728;

impl DelayEffect {
    fn reset(&mut self) {
        *self = DelayEffect::default();
    }

    /// `e` = os 20 bytes do efeito na configuração (o campo `dirty` é zerado aqui).
    fn parse(&mut self, e: &mut [u8], idx: usize) {
        use cfg::delay::*;
        if rd16(e, DIRTY) == 0 {
            return;
        }
        let was = self.enabled;
        self.enabled = rd16(e, ENABLE) != 0;
        wr16(e, DIRTY, 0);
        if !self.enabled {
            if was {
                self.reset();
            }
            return;
        }
        let mut n = rd16(e, FRAME_COUNT) as usize * SAMPLES_PER_FRAME;
        if n == 0 {
            self.reset();
            return;
        }
        if n > MAX_DELAY_SAMPLES {
            eprintln!("[dsp] atraso {idx}: {} quadros, limitado", rd16(e, FRAME_COUNT));
            n = MAX_DELAY_SAMPLES;
        }
        let q7 = |o| rd16(e, o) as i16 as f32 / 128.0;
        (self.g, self.a, self.b) = (q7(G), q7(A), q7(B));
        if n != self.samples || self.line[0].is_empty() {
            self.samples = n;
            self.pos = 0;
            self.last = [0.0; 4];
            for l in &mut self.line {
                *l = vec![0.0; n];
            }
        }
    }

    fn process(&mut self, frame: &mut QuadFrame) {
        if !self.enabled {
            return;
        }
        for (i, s) in frame.iter_mut().enumerate() {
            let p = (self.pos + i) % self.samples;
            for c in 0..4 {
                // y[n] = b·y[n-1] + a·v[n-N];  v[n] = x[n] - g·y[n]
                let x = s[c] as f32;
                let y = self.b * self.last[c] + self.a * self.line[c][p];
                self.line[c][p] = x - self.g * y;
                self.last[c] = y;
                s[c] = y.clamp(-2147483648.0, 2147483520.0) as i32;
            }
        }
        self.pos = (self.pos + SAMPLES_PER_FRAME) % self.samples;
    }
}

#[derive(Clone)]
pub struct Mixers {
    volume: [f32; 3],
    aux_bus_enable: [bool; 2],
    buffer: [QuadFrame; 3],
    /// 0 mono, 1 estéreo, 2 surround (tratado como estéreo, como no Citra)
    output_format: u16,
    delay: [DelayEffect; 2],
    frame: Frame,
}

impl Default for Mixers {
    fn default() -> Self {
        Mixers {
            volume: [0.0; 3],
            aux_bus_enable: [false; 2],
            buffer: [[[0; 4]; SAMPLES_PER_FRAME]; 3],
            output_format: 1,
            delay: Default::default(),
            frame: [[0; 2]; SAMPLES_PER_FRAME],
        }
    }
}

fn clamp16(v: i32) -> i16 {
    v.clamp(-32768, 32767) as i16
}

/// `IntermediateMixSamples`: mixN.pcm32[canal][amostra], s32 little-endian (dimensões ao contrário do QuadFrame).
fn inter_get(b: &[u8], mix: usize, ch: usize, i: usize) -> i32 {
    rd32(b, mix * 2560 + (ch * SAMPLES_PER_FRAME + i) * 4) as i32
}
fn inter_set(b: &mut [u8], mix: usize, ch: usize, i: usize, v: i32) {
    wr32(b, mix * 2560 + (ch * SAMPLES_PER_FRAME + i) * 4, v as u32);
}

impl Mixers {
    /// Um quadro. `c` = DspConfiguration da região lida (dirty zerado aqui), `read_inter`/`write_inter` = amostras
    /// intermediárias das regiões lida/escrita. Devolve quais barramentos auxiliares foram escritos em `write_inter`.
    pub fn tick(&mut self, c: &mut [u8], read_inter: &[u8], write_inter: &mut [u8], input: &[QuadFrame; 3]) -> [bool; 2] {
        self.parse_config(c);
        // retorno dos auxiliares: o que o jogo deixou na região lida
        for m in 0..2 {
            if self.aux_bus_enable[m] {
                for i in 0..SAMPLES_PER_FRAME {
                    for ch in 0..4 {
                        self.buffer[m + 1][i][ch] = inter_get(read_inter, m, ch, i);
                    }
                }
            }
        }
        // envio: o mixer 0 vai direto; os auxiliares vão para a região escrita (o jogo processa e devolve)
        self.buffer[0] = input[0];
        let mut wrote = [false; 2];
        for m in 0..2 {
            if self.aux_bus_enable[m] {
                for i in 0..SAMPLES_PER_FRAME {
                    for ch in 0..4 {
                        inter_set(write_inter, m, ch, i, input[m + 1][i][ch]);
                    }
                }
                wrote[m] = true;
            } else {
                self.buffer[m + 1] = input[m + 1];
            }
        }
        for m in 0..2 {
            self.delay[m].process(&mut self.buffer[m + 1]);
            // reverb: o Citra também não implementa [H]
        }
        self.frame = [[0; 2]; SAMPLES_PER_FRAME];
        for m in 0..3 {
            self.downmix(m);
        }
        // compressor/limitador: não implementado (o Citra assume desligado)
        wrote
    }

    pub fn output(&self) -> &Frame {
        &self.frame
    }

    fn parse_config(&mut self, c: &mut [u8]) {
        let d = rd32(c, cfg::DIRTY);
        if d == 0 {
            return;
        }
        for m in 0..2 {
            if d & (cfg::AUX_BUS_ENABLE0_DIRTY << m) != 0 {
                self.aux_bus_enable[m] = rd16(c, cfg::AUX_BUS_ENABLE + 2 * m) != 0;
            }
        }
        if d & cfg::MASTER_VOLUME_DIRTY != 0 {
            self.volume[0] = rdf32(c, cfg::MASTER_VOLUME);
        }
        for m in 0..2 {
            if d & (cfg::AUX_RETURN_VOLUME0_DIRTY << m) != 0 {
                self.volume[m + 1] = rdf32(c, cfg::AUX_RETURN_VOLUME + 4 * m);
            }
        }
        if d & cfg::OUTPUT_FORMAT_DIRTY != 0 {
            self.output_format = rd16(c, cfg::OUTPUT_FORMAT);
        }
        for m in 0..2 {
            if d & (cfg::DELAY0_DIRTY << m) != 0 {
                let o = cfg::DELAY + m * cfg::DELAY_SIZE;
                self.delay[m].parse(&mut c[o..o + cfg::DELAY_SIZE], m);
            }
            // reverb (bits 12/13): só registrado no Citra
        }
        wr32(c, cfg::DIRTY, 0);
    }

    fn downmix(&mut self, m: usize) {
        let g = self.volume[m];
        let src = &self.buffer[m];
        for (acc, s) in self.frame.iter_mut().zip(src.iter()) {
            let f = |v: i32| g * v as f32;
            let (l, r) = if self.output_format == 0 {
                let mono = clamp16(((f(s[0]) + f(s[1]) + f(s[2]) + f(s[3])) / 2.0) as i32);
                (mono, mono)
            } else {
                (clamp16((f(s[0]) + f(s[2])) as i32), clamp16((f(s[1]) + f(s[3])) as i32))
            };
            *acc = [clamp16(acc[0] as i32 + l as i32), clamp16(acc[1] as i32 + r as i32)];
        }
    }
}
