//! Leitor de BCSTM (`CSTM`, little-endian) com decodificação DSP-ADPCM, tudo em Rust.
//!
//! Layout conferido contra `snd/product/stream/yw1_op_en.dspadpcm.bcstm` real (ver docs/FORMATS.md):
//! cabeçalho com tabela de blocos (INFO 0x4000, DATA 0x4002); INFO tem 3 referências relativas a
//! INFO+8 (stream info, track info, tabela de canais); cada canal aponta para 16 coeficientes i16 +
//! histórico inicial. Os dados são blocos intercalados por canal; o último bloco usa o tamanho com padding.

use std::fmt;

pub struct Stream {
    pub sample_rate: u32,
    pub channels: usize,
    /// PCM intercalado (canal 0, canal 1, ...).
    pub pcm: Vec<i16>,
}

impl Stream {
    pub fn frames(&self) -> usize {
        self.pcm.len() / self.channels
    }
}

#[derive(Debug)]
pub struct Error(String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BCSTM: {}", self.0)
    }
}

impl std::error::Error for Error {}

fn err<T>(msg: impl Into<String>) -> Result<T, Error> {
    Err(Error(msg.into()))
}

fn u16le(d: &[u8], o: usize) -> Result<u16, Error> {
    match d.get(o..o + 2) {
        Some(b) => Ok(u16::from_le_bytes([b[0], b[1]])),
        None => err(format!("leitura fora do arquivo em 0x{o:x}")),
    }
}

fn u32le(d: &[u8], o: usize) -> Result<u32, Error> {
    match d.get(o..o + 4) {
        Some(b) => Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
        None => err(format!("leitura fora do arquivo em 0x{o:x}")),
    }
}

struct Channel {
    coefs: [i32; 16],
    hist1: i32,
    hist2: i32,
}

pub fn decode(d: &[u8]) -> Result<Stream, Error> {
    if d.get(0..4) != Some(b"CSTM".as_slice()) {
        return err("assinatura CSTM ausente");
    }
    if u16le(d, 4)? != 0xFEFF {
        return err("só a variante little-endian (3DS) é suportada");
    }
    let (mut info, mut data) = (None, None);
    for i in 0..u16le(d, 0x10)? as usize {
        let e = 0x14 + i * 12;
        let off = u32le(d, e + 4)? as usize;
        match u16le(d, e)? {
            0x4000 => info = Some(off),
            0x4002 => data = Some(off),
            _ => {}
        }
    }
    let (Some(info), Some(data)) = (info, data) else {
        return err("blocos INFO/DATA ausentes");
    };
    if d.get(info..info + 4) != Some(b"INFO".as_slice()) {
        return err("bloco INFO inválido");
    }

    let refs = info + 8;
    let si = refs + u32le(d, refs + 4)? as usize;
    let encoding = *d.get(si).ok_or(Error("stream info truncado".into()))?;
    if encoding != 2 {
        return err(format!(
            "codificação {encoding} não suportada (só DSP-ADPCM = 2)"
        ));
    }
    let channels = d[si + 2] as usize;
    if channels == 0 {
        return err("stream sem canais");
    }
    let sample_rate = u32le(d, si + 0x04)?;
    let sample_count = u32le(d, si + 0x0c)? as usize;
    let block_count = u32le(d, si + 0x10)? as usize;
    let block_size = u32le(d, si + 0x14)? as usize;
    let block_samples = u32le(d, si + 0x18)? as usize;
    let last_block_padded = u32le(d, si + 0x24)? as usize;
    let data_start = data + 8 + u32le(d, si + 0x34)? as usize;

    let table = refs + u32le(d, refs + 0x14)? as usize;
    if u32le(d, table)? as usize != channels {
        return err("tabela de canais não bate com o número de canais");
    }
    let mut chans = Vec::with_capacity(channels);
    for c in 0..channels {
        let ch_info = table + u32le(d, table + 4 + c * 8 + 4)? as usize;
        let adpcm = ch_info + u32le(d, ch_info + 4)? as usize;
        let mut coefs = [0i32; 16];
        for (k, coef) in coefs.iter_mut().enumerate() {
            *coef = u16le(d, adpcm + k * 2)? as i16 as i32;
        }
        chans.push(Channel {
            coefs,
            hist1: u16le(d, adpcm + 34)? as i16 as i32,
            hist2: u16le(d, adpcm + 36)? as i16 as i32,
        });
    }

    let mut pcm = vec![0i16; sample_count * channels];
    for b in 0..block_count {
        let last = b + 1 == block_count;
        let stride = if last { last_block_padded } else { block_size };
        let first = b * block_samples;
        let n = if last {
            sample_count.saturating_sub(first)
        } else {
            block_samples
        };
        for (c, ch) in chans.iter_mut().enumerate() {
            let start = data_start + b * block_size * channels + c * stride;
            let src = d
                .get(start..start + n.div_ceil(14) * 8)
                .ok_or_else(|| Error(format!("bloco {b} canal {c} truncado")))?;
            decode_dsp(src, n, ch, |i, s| pcm[(first + i) * channels + c] = s);
        }
    }
    Ok(Stream {
        sample_rate,
        channels,
        pcm,
    })
}

/// Quadros de 8 bytes: 1 byte de cabeçalho (índice do par de coeficientes << 4 | escala) + 14 nibbles.
fn decode_dsp(src: &[u8], n: usize, ch: &mut Channel, mut out: impl FnMut(usize, i16)) {
    for i in 0..n {
        let frame = &src[(i / 14) * 8..];
        let header = frame[0];
        let scale = 1i32 << (header & 0x0f);
        let pair = ((header >> 4) & 7) as usize;
        let k = i % 14;
        let byte = frame[1 + k / 2];
        let nib = if k % 2 == 0 { byte >> 4 } else { byte & 0x0f } as i32;
        let nib = if nib >= 8 { nib - 16 } else { nib };
        let pred = ch.coefs[pair * 2] * ch.hist1 + ch.coefs[pair * 2 + 1] * ch.hist2;
        let s = (((nib * scale) << 11) + 1024 + pred) >> 11;
        let s = s.clamp(i16::MIN as i32, i16::MAX as i32);
        ch.hist2 = ch.hist1;
        ch.hist1 = s;
        out(i, s as i16);
    }
}
