//! `XPVB` (vértices) e `XPVI` (índices). Atributos: 10 entradas `[tipo, offset, tamanho, flag]`
//! (tipo 2/3/4 = float2/3/4; `flag & 1` = constante, valor na tabela fixa — nos layouts de menu é
//! só a cor (1,1,1,1), por isso a tabela fixa ainda não é lida). Ver docs/FORMATS.md.

use super::{l5, u16le, u32le, Result};

pub struct Attribute {
    pub kind: u8,
    pub offset: usize,
    pub size: usize,
    pub constant: bool,
}

pub struct VertexBuffer {
    pub stride: usize,
    pub count: usize,
    pub attributes: Vec<Option<Attribute>>,
    pub data: Vec<u8>,
}

impl VertexBuffer {
    pub fn parse(buf: &[u8]) -> Result<VertexBuffer> {
        if buf.get(0..4) != Some(b"XPVB".as_slice()) {
            return Err("não é um XPVB".into());
        }
        let blob = |o: usize| l5::decompress(buf.get(o..).ok_or("XPVB: offset inválido")?);
        let table = blob(u16le(buf, 4)? as usize)?;
        let stride = u16le(buf, 10)? as usize;
        let count = u32le(buf, 12)? as usize;
        let data = blob(u16le(buf, 8)? as usize)?;
        if data.len() < stride * count {
            return Err("XPVB: vértices truncados".into());
        }
        let attributes = table
            .as_chunks::<4>()
            .0
            .iter()
            .map(|a| {
                (a[2] != 0).then(|| Attribute {
                    kind: a[0],
                    offset: a[1] as usize,
                    size: a[2] as usize,
                    constant: a[3] & 1 != 0,
                })
            })
            .collect();
        Ok(VertexBuffer {
            stride,
            count,
            attributes,
            data,
        })
    }

    /// Componentes float de um atributo por vértice (`None` se ausente ou constante).
    pub fn floats(&self, slot: usize, vertex: usize) -> Option<&[u8]> {
        let a = self.attributes.get(slot)?.as_ref()?;
        if a.constant {
            return None;
        }
        let o = vertex * self.stride + a.offset;
        self.data.get(o..o + a.size)
    }

    pub fn float(&self, slot: usize, vertex: usize, component: usize) -> f32 {
        self.floats(slot, vertex)
            .and_then(|b| b.get(component * 4..component * 4 + 4))
            .map_or(0.0, |b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Primeiro atributo não constante do tipo pedido.
    pub fn slot_of_kind(&self, kind: u8) -> Option<usize> {
        self.attributes
            .iter()
            .position(|a| a.as_ref().is_some_and(|a| a.kind == kind && !a.constant))
    }
}

/// Índices u16 (nos layouts de menu o campo de formato vale 0 mas os dados são u16: decide-se
/// pelo tamanho descomprimido).
pub fn parse_indices(buf: &[u8]) -> Result<Vec<u16>> {
    if buf.get(0..4) != Some(b"XPVI".as_slice()) {
        return Err("não é um XPVI".into());
    }
    let off = u16le(buf, 6)? as usize;
    let count = u32le(buf, 8)? as usize;
    let raw = l5::decompress(buf.get(off..).ok_or("XPVI: offset inválido")?)?;
    if raw.len() >= count * 2 {
        Ok(raw
            .as_chunks::<2>()
            .0
            .iter()
            .take(count)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect())
    } else {
        Ok(raw.iter().take(count).map(|&b| b as u16).collect())
    }
}
