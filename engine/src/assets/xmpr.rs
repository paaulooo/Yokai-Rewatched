//! `XMPR` (`.prm`): uma malha com um material. Porte de `tools/prm.py` + `tools/map_probe.py::parse_prm_ext`.
//! Vértices de mapa (stride 36): slot 0 posição f32×3 · slot 1 cor de vértice RGBA f32 (iluminação "assada") ·
//! slot 4 UV f32×2 (V direto: v=0 = linha 0 da imagem). Malhas de colisão/sombra só têm posição.
//! Índices: XPVI +4 = formato — 2 = triangle strip com degenerados (todas as malhas visuais), 0 = lista de
//! triângulos (as malhas de colisão `cl`; conferido: 5/5 com trios sem repetição, 114/114 visuais em strip).

use super::{cstr, f32le, u16le, u32le, xpvb, Result};

#[allow(dead_code)] // campos lidos por ferramentas/testes futuros (hash da malha, flag de UV)
pub struct Mesh {
    pub mesh_hash: u32,
    pub material_hash: u32,
    pub node_hash: u32,
    pub name: String,
    pub material_name: String,
    pub pos: Vec<[f32; 3]>,
    pub color: Vec<[f32; 4]>,
    pub uv: Vec<[f32; 2]>,
    pub has_uv: bool,
    pub tris: Vec<[u32; 3]>,
}

pub fn parse(d: &[u8]) -> Result<Mesh> {
    if d.get(0..4) != Some(b"XMPR".as_slice()) {
        return Err("não é um XMPR".into());
    }
    let info = u32le(d, 12)? as usize;
    let name_at = |slot: usize| -> Result<String> { cstr(d, u32le(d, 4 * slot)? as usize) };
    // Header: palavras 12..15 = (off, tam) do nome da malha e do nome do material.
    let name = name_at(12)?;
    let material_name = name_at(14)?;
    let vb_at = d
        .windows(4)
        .position(|w| w == b"XPVB")
        .ok_or("XMPR sem XPVB")?;
    let vi_at = d
        .windows(4)
        .position(|w| w == b"XPVI")
        .ok_or("XMPR sem XPVI")?;
    let vb = xpvb::VertexBuffer::parse(&d[vb_at..])?;
    let strip = xpvb::parse_indices(&d[vi_at..])?;
    let list = u16le(&d[vi_at..], 4)? == 0;
    let uv_slot = vb.slot_of_kind(2);
    let color_slot = vb
        .attributes
        .get(1)
        .and_then(|a| a.as_ref())
        .filter(|a| !a.constant)
        .map(|_| 1);
    let mut pos = Vec::with_capacity(vb.count);
    let mut color = Vec::with_capacity(vb.count);
    let mut uv = Vec::with_capacity(vb.count);
    for v in 0..vb.count {
        pos.push([vb.float(0, v, 0), vb.float(0, v, 1), vb.float(0, v, 2)]);
        color.push(match color_slot {
            Some(s) => [
                vb.float(s, v, 0),
                vb.float(s, v, 1),
                vb.float(s, v, 2),
                vb.float(s, v, 3),
            ],
            None => [1.0; 4],
        });
        uv.push(match uv_slot {
            Some(s) => [vb.float(s, v, 0), vb.float(s, v, 1)],
            None => [0.0; 2],
        });
    }
    let mut tris = Vec::new();
    if list {
        for &[a, b, c] in strip.as_chunks::<3>().0 {
            let (a, b, c) = (a as u32, b as u32, c as u32);
            if a != b && b != c && a != c {
                tris.push([a, b, c]);
            }
        }
    }
    for (k, w) in strip.windows(3).enumerate().filter(|_| !list) {
        let (a, b, c) = (w[0] as u32, w[1] as u32, w[2] as u32);
        if a == b || b == c || a == c {
            continue;
        }
        tris.push(if k % 2 == 0 { [a, b, c] } else { [b, a, c] });
    }
    let _ = f32le; // (AABB do INFO fica para quando houver culling)
    Ok(Mesh {
        mesh_hash: u32le(d, info)?,
        material_hash: u32le(d, info + 4)?,
        node_hash: u32le(d, info + 12)?,
        name,
        material_name,
        pos,
        color,
        uv,
        has_uv: uv_slot.is_some(),
        tris,
    })
}
