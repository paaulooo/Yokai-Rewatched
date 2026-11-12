//! FSR 1 (AMD FidelityFX Super Resolution 1.0: EASU + RCAS, espacial) na apresentação da janela.
//!
//! O algoritmo é o do FSR 1 da AMD (MIT), reescrito aqui em WGSL com aritmética f32 (sem as aproximações de meia
//! precisão do original):
//!  1. EASU (Edge-Adaptive Spatial Upsampling): para cada pixel da tela, 12 amostras da imagem de entrada em volta da
//!     posição; a direção e a "esticada" da borda saem dos gradientes de luma dos 4 quadrantes, e as amostras entram
//!     num núcleo tipo Lanczos orientado pela borda;
//!  2. RCAS (Robust Contrast-Adaptive Sharpening): nitidez com limite contra halos, na imagem já ampliada.
//!
//! Só o FSR 1: as versões 2/3 (temporais, geração de quadros) exigem vetores de movimento, jitter de câmera e
//! profundidade fornecidos pelo motor do jogo, que aqui são matrizes dentro do código recompilado; o FSR 4 é um modelo
//! de aprendizado de máquina que só roda em GPUs RDNA 4 pelo SDK fechado da AMD.
//!
//! Integração: `Fsr` é um `egui_wgpu::CallbackTrait`. `prepare` envia a imagem da tela (se mudou) e roda EASU → RCAS em
//! texturas do tamanho do retângulo da tela na janela, no codificador do egui; `paint` só desenha o resultado dentro
//! da passada do egui.
//!
//! Sem FSR, as telas usam o mesmo caminho pela GPU (`Direta`): a imagem vai uma vez para uma textura persistente e o
//! filtro é feito no shader — Nítido (vizinho mais próximo), Suave (bilinear) e Misto (*sharp bilinear*: o mesmo que
//! ampliar n× por vizinho e depois bilinear, que antes era feito na CPU). Antes as telas passavam pelo egui: cópia do
//! RGBA, conversão para `ColorImage`, ampliação na CPU e reenvio — 1,4–2,9 ms por imagem na thread da janela a 3×
//! (medido na linha `[janela]`), o que atrasava os passos da interpolação.

use egui_wgpu::{CallbackResources, CallbackTrait, ScreenDescriptor};
use std::collections::HashMap;
use std::sync::Arc;

/// Imagem RGBA8 de uma tela (linhas de cima para baixo).
pub struct ScreenImage {
    pub w: u32,
    pub h: u32,
    pub rgba: Bytes,
}

/// Os bytes de uma tela: a imagem em alta resolução já publicada (compartilhada, sem cópia) ou uma nativa montada agora.
pub enum Bytes {
    Own(Vec<u8>),
    Hi(Arc<crate::render::vulkan::HiRes>),
}

impl std::ops::Deref for Bytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Bytes::Own(v) => v,
            Bytes::Hi(h) => &h.rgba,
        }
    }
}

/// Como uma tela vai para a janela pela GPU.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Modo {
    /// FSR 1 (EASU + RCAS)
    Fsr,
    /// filtro no shader: 0 = vizinho mais próximo, 1 = bilinear, 2 = sharp bilinear com fator inteiro `n`
    Direto { filtro: u32, n: u32 },
}

/// Pedido de desenho de uma tela com FSR 1.
pub struct Fsr {
    /// 0 = tela de cima, 1 = de baixo (uma pilha de recursos por tela)
    pub id: u8,
    pub img: Arc<ScreenImage>,
    /// muda quando `img` muda (reenvia a textura)
    pub version: u64,
    /// tamanho do retângulo na janela, em pixels físicos
    pub dst: [u32; 2],
    /// fator do RCAS: 1 = nitidez máxima, 0,25 = mínima (2^−paradas)
    pub sharp: f32,
    /// formato da superfície da janela
    pub format: wgpu::TextureFormat,
}

const VS: &str = r#"
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn vs(@builtin(vertex_index) i: u32) -> VOut {
    var o: VOut;
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    o.pos = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    o.uv = vec2<f32>(x, y);
    return o;
}
"#;

const COMMON: &str = r#"
struct Uni { a: vec4<f32>, b: vec4<f32> }; // a = (origem w, origem h, destino w, destino h); b.x = fator do RCAS
@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var<uniform> u: Uni;
fn px(p: vec2<i32>) -> vec3<f32> {
    let d = vec2<i32>(textureDimensions(tex));
    return textureLoad(tex, clamp(p, vec2<i32>(0), d - vec2<i32>(1)), 0).rgb;
}
"#;

const EASU: &str = r#"
fn lum(c: vec3<f32>) -> f32 { return c.b * 0.5 + (c.r * 0.5 + c.g); }

fn easu_set(dir: ptr<function, vec2<f32>>, len: ptr<function, f32>, w: f32, lA: f32, lB: f32, lC: f32, lD: f32, lE: f32) {
    var lenX = max(abs(lD - lC), abs(lC - lB));
    let dirX = lD - lB;
    (*dir).x += dirX * w;
    lenX = clamp(abs(dirX) / max(lenX, 1e-5), 0.0, 1.0);
    lenX *= lenX;
    *len += lenX * w;
    var lenY = max(abs(lE - lC), abs(lC - lA));
    let dirY = lE - lA;
    (*dir).y += dirY * w;
    lenY = clamp(abs(dirY) / max(lenY, 1e-5), 0.0, 1.0);
    lenY *= lenY;
    *len += lenY * w;
}

fn easu_tap(acc: ptr<function, vec3<f32>>, aw: ptr<function, f32>, off: vec2<f32>, dir: vec2<f32>, len: vec2<f32>, lob: f32, clp: f32, c: vec3<f32>) {
    var v = vec2<f32>(off.x * dir.x + off.y * dir.y, off.x * (-dir.y) + off.y * dir.x);
    v = v * len;
    var d2 = v.x * v.x + v.y * v.y;
    d2 = min(d2, clp);
    var wB = 2.0 / 5.0 * d2 - 1.0;
    var wA = lob * d2 - 1.0;
    wB *= wB;
    wA *= wA;
    wB = 25.0 / 16.0 * wB - (25.0 / 16.0 - 1.0);
    let w = wB * wA;
    *acc += c * w;
    *aw += w;
}

@fragment fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let scale = u.a.xy / u.a.zw;
    var pp = (floor(pos.xy) + vec2<f32>(0.5)) * scale - vec2<f32>(0.5);
    let fp = floor(pp);
    pp = pp - fp;
    let ip = vec2<i32>(fp);
    //      b c
    //  e f g h
    //  i j k l
    //      n o
    let b = px(ip + vec2<i32>(0, -1));
    let c = px(ip + vec2<i32>(1, -1));
    let e = px(ip + vec2<i32>(-1, 0));
    let f = px(ip);
    let g = px(ip + vec2<i32>(1, 0));
    let h = px(ip + vec2<i32>(2, 0));
    let i = px(ip + vec2<i32>(-1, 1));
    let j = px(ip + vec2<i32>(0, 1));
    let k = px(ip + vec2<i32>(1, 1));
    let l = px(ip + vec2<i32>(2, 1));
    let n = px(ip + vec2<i32>(0, 2));
    let o = px(ip + vec2<i32>(1, 2));
    let bL = lum(b); let cL = lum(c); let eL = lum(e); let fL = lum(f); let gL = lum(g); let hL = lum(h);
    let iL = lum(i); let jL = lum(j); let kL = lum(k); let lL = lum(l); let nL = lum(n); let oL = lum(o);
    var dir = vec2<f32>(0.0);
    var len = 0.0;
    easu_set(&dir, &len, (1.0 - pp.x) * (1.0 - pp.y), bL, eL, fL, gL, jL);
    easu_set(&dir, &len, pp.x * (1.0 - pp.y), cL, fL, gL, hL, kL);
    easu_set(&dir, &len, (1.0 - pp.x) * pp.y, fL, iL, jL, kL, nL);
    easu_set(&dir, &len, pp.x * pp.y, gL, jL, kL, lL, oL);
    let dir2 = dir * dir;
    var dirR = dir2.x + dir2.y;
    let zro = dirR < (1.0 / 32768.0);
    dirR = inverseSqrt(max(dirR, 1e-9));
    if (zro) { dirR = 1.0; dir.x = 1.0; }
    dir = dir * vec2<f32>(dirR);
    len = len * 0.5;
    len = len * len;
    let stretch = (dir.x * dir.x + dir.y * dir.y) / max(abs(dir.x), abs(dir.y));
    let len2 = vec2<f32>(1.0 + (stretch - 1.0) * len, 1.0 - 0.5 * len);
    let lob = 0.5 + ((1.0 / 4.0 - 0.04) - 0.5) * len;
    let clp = 1.0 / lob;
    let min4 = min(min(f, g), min(j, k));
    let max4 = max(max(f, g), max(j, k));
    var aC = vec3<f32>(0.0);
    var aW = 0.0;
    easu_tap(&aC, &aW, vec2<f32>(0.0, -1.0) - pp, dir, len2, lob, clp, b);
    easu_tap(&aC, &aW, vec2<f32>(1.0, -1.0) - pp, dir, len2, lob, clp, c);
    easu_tap(&aC, &aW, vec2<f32>(-1.0, 0.0) - pp, dir, len2, lob, clp, e);
    easu_tap(&aC, &aW, vec2<f32>(0.0, 0.0) - pp, dir, len2, lob, clp, f);
    easu_tap(&aC, &aW, vec2<f32>(1.0, 0.0) - pp, dir, len2, lob, clp, g);
    easu_tap(&aC, &aW, vec2<f32>(2.0, 0.0) - pp, dir, len2, lob, clp, h);
    easu_tap(&aC, &aW, vec2<f32>(-1.0, 1.0) - pp, dir, len2, lob, clp, i);
    easu_tap(&aC, &aW, vec2<f32>(0.0, 1.0) - pp, dir, len2, lob, clp, j);
    easu_tap(&aC, &aW, vec2<f32>(1.0, 1.0) - pp, dir, len2, lob, clp, k);
    easu_tap(&aC, &aW, vec2<f32>(2.0, 1.0) - pp, dir, len2, lob, clp, l);
    easu_tap(&aC, &aW, vec2<f32>(0.0, 2.0) - pp, dir, len2, lob, clp, n);
    easu_tap(&aC, &aW, vec2<f32>(1.0, 2.0) - pp, dir, len2, lob, clp, o);
    let pix = min(max4, max(min4, aC / aW));
    return vec4<f32>(pix, 1.0);
}
"#;

const RCAS: &str = r#"
@fragment fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let ip = vec2<i32>(floor(pos.xy));
    let b = px(ip + vec2<i32>(0, -1));
    let d = px(ip + vec2<i32>(-1, 0));
    let e = px(ip);
    let f = px(ip + vec2<i32>(1, 0));
    let h = px(ip + vec2<i32>(0, 1));
    let mn4 = min(min(b, d), min(f, h));
    let mx4 = max(max(b, d), max(f, h));
    // limite contra halos: quanto cada canal pode "empurrar" antes de estourar 0 ou 1
    let hitMin = min(mn4, e) / (4.0 * max(mx4, vec3<f32>(1e-4)));
    let hitMax = (vec3<f32>(1.0) - max(mx4, e)) / min(4.0 * mn4 - vec3<f32>(4.0), vec3<f32>(-1e-4));
    let lobeRGB = max(-hitMin, hitMax);
    let lobe = max(-0.1875, min(max(lobeRGB.r, max(lobeRGB.g, lobeRGB.b)), 0.0)) * u.b.x;
    let rcpL = 1.0 / (4.0 * lobe + 1.0);
    return vec4<f32>((lobe * (b + d + f + h) + e) * rcpL, 1.0);
}
"#;

const BLIT: &str = r#"
@group(0) @binding(0) var tex: texture_2d<f32>;
@fragment fn fs(in: VOut) -> @location(0) vec4<f32> {
    let d = vec2<f32>(textureDimensions(tex));
    let ip = vec2<i32>(floor(in.uv * d));
    return vec4<f32>(textureLoad(tex, clamp(ip, vec2<i32>(0), vec2<i32>(d) - vec2<i32>(1)), 0).rgb, 1.0);
}
"#;

struct Pipes {
    easu: wgpu::RenderPipeline,
    rcas: wgpu::RenderPipeline,
    blit: wgpu::RenderPipeline,
    bgl_tex_uni: wgpu::BindGroupLayout,
    bgl_tex: wgpu::BindGroupLayout,
}

struct Screen {
    version: u64,
    sharp: f32,
    src: wgpu::Texture,
    src_dims: [u32; 2],
    dst_dims: [u32; 2],
    mid_view: wgpu::TextureView,
    out_view: wgpu::TextureView,
    #[allow(dead_code)] // só o teste lê o resultado
    out_tex: wgpu::Texture,
    uni: wgpu::Buffer,
    bg_easu: wgpu::BindGroup,
    bg_rcas: wgpu::BindGroup,
    bg_blit: wgpu::BindGroup,
    /// precisa rodar EASU/RCAS de novo (imagem, tamanho ou nitidez mudaram)
    dirty: bool,
}

#[derive(Default)]
struct Res {
    pipes: Option<Pipes>,
    screens: HashMap<u8, Screen>,
}

fn make_pipes(device: &wgpu::Device, format: wgpu::TextureFormat) -> Pipes {
    let tex_entry = |binding| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    };
    let uni_entry = wgpu::BindGroupLayoutEntry {
        binding: 1,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    };
    let bgl_tex_uni = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("fsr tex+uni"), entries: &[tex_entry(0), uni_entry] });
    let bgl_tex = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("fsr tex"), entries: &[tex_entry(0)] });
    let module = |name: &str, src: String| {
        device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some(name), source: wgpu::ShaderSource::Wgsl(src.into()) })
    };
    let m_easu = module("fsr easu", format!("{VS}{COMMON}{EASU}"));
    let m_rcas = module("fsr rcas", format!("{VS}{COMMON}{RCAS}"));
    let m_blit = module("fsr blit", format!("{VS}{BLIT}"));
    let pipe = |label: &str, m: &wgpu::ShaderModule, bgl: &wgpu::BindGroupLayout, fmt: wgpu::TextureFormat| {
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some(label), bind_group_layouts: &[Some(bgl)], immediate_size: 0 });
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&layout),
            vertex: wgpu::VertexState { module: m, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: m,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format: fmt, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        })
    };
    let mid = wgpu::TextureFormat::Rgba8Unorm;
    Pipes {
        easu: pipe("fsr easu", &m_easu, &bgl_tex_uni, mid),
        rcas: pipe("fsr rcas", &m_rcas, &bgl_tex_uni, mid),
        blit: pipe("fsr blit", &m_blit, &bgl_tex, format),
        bgl_tex_uni,
        bgl_tex,
    }
}

fn texture(device: &wgpu::Device, w: u32, h: u32, usage: wgpu::TextureUsages, label: &str) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage,
        view_formats: &[],
    })
}

fn make_screen(device: &wgpu::Device, p: &Pipes, src_dims: [u32; 2], dst_dims: [u32; 2]) -> Screen {
    let src = texture(device, src_dims[0], src_dims[1], wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST, "fsr src");
    let rt = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC;
    let mid = texture(device, dst_dims[0], dst_dims[1], rt, "fsr mid");
    let out = texture(device, dst_dims[0], dst_dims[1], rt, "fsr out");
    let src_view = src.create_view(&Default::default());
    let mid_view = mid.create_view(&Default::default());
    let out_view = out.create_view(&Default::default());
    let uni = device.create_buffer(&wgpu::BufferDescriptor { label: Some("fsr uni"), size: 32, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let bg = |layout: &wgpu::BindGroupLayout, view: &wgpu::TextureView, with_uni: bool| {
        let mut e = vec![wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(view) }];
        if with_uni {
            e.push(wgpu::BindGroupEntry { binding: 1, resource: uni.as_entire_binding() });
        }
        device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("fsr bg"), layout, entries: &e })
    };
    Screen {
        version: u64::MAX,
        sharp: -1.0,
        bg_easu: bg(&p.bgl_tex_uni, &src_view, true),
        bg_rcas: bg(&p.bgl_tex_uni, &mid_view, true),
        bg_blit: bg(&p.bgl_tex, &out_view, false),
        src,
        src_dims,
        dst_dims,
        mid_view,
        out_view,
        out_tex: out,
        uni,
        dirty: true,
    }
}

impl CallbackTrait for Fsr {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &ScreenDescriptor,
        encoder: &mut wgpu::CommandEncoder,
        resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if resources.get::<Res>().is_none() {
            resources.insert(Res::default());
        }
        let res = resources.get_mut::<Res>().unwrap();
        if res.pipes.is_none() {
            res.pipes = Some(make_pipes(device, self.format));
        }
        let pipes = res.pipes.as_ref().unwrap();
        let src_dims = [self.img.w, self.img.h];
        let need_new = res.screens.get(&self.id).is_none_or(|s| s.src_dims != src_dims || s.dst_dims != self.dst);
        if need_new {
            res.screens.insert(self.id, make_screen(device, pipes, src_dims, self.dst));
        }
        let s = res.screens.get_mut(&self.id).unwrap();
        if s.version != self.version {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture: &s.src, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                &self.img.rgba[..],
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(self.img.w * 4), rows_per_image: Some(self.img.h) },
                wgpu::Extent3d { width: self.img.w, height: self.img.h, depth_or_array_layers: 1 },
            );
            s.version = self.version;
            s.dirty = true;
        }
        if (s.sharp - self.sharp).abs() > f32::EPSILON {
            s.sharp = self.sharp;
            s.dirty = true;
        }
        if s.dirty {
            s.dirty = false;
            let u: [f32; 8] = [self.img.w as f32, self.img.h as f32, self.dst[0] as f32, self.dst[1] as f32, self.sharp, 0.0, 0.0, 0.0];
            queue.write_buffer(&s.uni, 0, &u.iter().flat_map(|f| f.to_le_bytes()).collect::<Vec<u8>>());
            let mut pass = |view: &wgpu::TextureView, pipe: &wgpu::RenderPipeline, bg: &wgpu::BindGroup| {
                let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("fsr"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                rp.set_pipeline(pipe);
                rp.set_bind_group(0, bg, &[]);
                rp.draw(0..3, 0..1);
            };
            pass(&s.mid_view, &pipes.easu, &s.bg_easu);
            pass(&s.out_view, &pipes.rcas, &s.bg_rcas);
        }
        Vec::new()
    }

    fn paint(&self, _info: egui::PaintCallbackInfo, rp: &mut wgpu::RenderPass<'static>, resources: &CallbackResources) {
        let Some(res) = resources.get::<Res>() else { return };
        let (Some(p), Some(s)) = (res.pipes.as_ref(), res.screens.get(&self.id)) else { return };
        rp.set_pipeline(&p.blit);
        rp.set_bind_group(0, &s.bg_blit, &[]);
        rp.draw(0..3, 0..1);
    }
}

const DIRETA: &str = r#"
struct U { a: vec4<f32> }; // a.xy = tamanho da imagem; a.z = filtro (0 vizinho, 1 bilinear, 2 sharp bilinear); a.w = n
@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var smp: sampler;
@group(0) @binding(2) var<uniform> u: U;
@fragment fn fs(in: VOut) -> @location(0) vec4<f32> {
    let d = u.a.xy;
    if (u.a.z < 0.5) {
        let ip = vec2<i32>(floor(in.uv * d));
        return vec4<f32>(textureLoad(tex, clamp(ip, vec2<i32>(0), vec2<i32>(d) - vec2<i32>(1)), 0).rgb, 1.0);
    }
    var uv = in.uv;
    if (u.a.z > 1.5) {
        // sharp bilinear: dentro de cada texel fica o centro (vizinho); só a faixa de 1/n texel junto à borda mistura
        // com o vizinho — igual a ampliar n× por vizinho mais próximo e amostrar bilinear o resultado
        let texel = in.uv * d;
        let c = fract(texel) - vec2<f32>(0.5);
        let r = 0.5 - 0.5 / u.a.w;
        let f = (c - clamp(c, vec2<f32>(-r), vec2<f32>(r))) * u.a.w + vec2<f32>(0.5);
        uv = (floor(texel) + f) / d;
    }
    return vec4<f32>(textureSampleLevel(tex, smp, uv, 0.0).rgb, 1.0);
}
"#;

/// Pedido de desenho de uma tela pelo caminho direto (sem FSR).
pub struct Direta {
    pub id: u8,
    pub img: Arc<ScreenImage>,
    /// muda quando `img` muda (reenvia a textura)
    pub version: u64,
    /// 0 = vizinho mais próximo, 1 = bilinear, 2 = sharp bilinear
    pub filtro: u32,
    /// fator inteiro do sharp bilinear (1 = bilinear comum)
    pub n: u32,
    pub format: wgpu::TextureFormat,
}

struct DTela {
    version: u64,
    dims: [u32; 2],
    src: wgpu::Texture,
    uni: wgpu::Buffer,
    bg: wgpu::BindGroup,
    params: [f32; 4],
}

#[derive(Default)]
struct DRes {
    pipe: Option<(wgpu::RenderPipeline, wgpu::BindGroupLayout, wgpu::Sampler)>,
    telas: HashMap<u8, DTela>,
}

fn make_direta(device: &wgpu::Device, format: wgpu::TextureFormat) -> (wgpu::RenderPipeline, wgpu::BindGroupLayout, wgpu::Sampler) {
    let entries = [
        wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        },
        wgpu::BindGroupLayoutEntry {
            binding: 1,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        },
        wgpu::BindGroupLayoutEntry {
            binding: 2,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        },
    ];
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("tela direta"), entries: &entries });
    let m = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("tela direta"),
        source: wgpu::ShaderSource::Wgsl(format!("{VS}{DIRETA}").into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("tela direta"), bind_group_layouts: &[Some(&bgl)], immediate_size: 0 });
    let pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("tela direta"),
        layout: Some(&layout),
        vertex: wgpu::VertexState { module: &m, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &m,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
        }),
        multiview_mask: None,
        cache: None,
    });
    let smp = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("tela direta"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    (pipe, bgl, smp)
}

impl CallbackTrait for Direta {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if resources.get::<DRes>().is_none() {
            resources.insert(DRes::default());
        }
        let res = resources.get_mut::<DRes>().unwrap();
        if res.pipe.is_none() {
            res.pipe = Some(make_direta(device, self.format));
        }
        let (_, bgl, smp) = res.pipe.as_ref().unwrap();
        let dims = [self.img.w, self.img.h];
        if res.telas.get(&self.id).is_none_or(|t| t.dims != dims) {
            let src = texture(device, dims[0], dims[1], wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST, "tela direta");
            let view = src.create_view(&Default::default());
            let uni = device.create_buffer(&wgpu::BufferDescriptor { label: Some("tela direta"), size: 16, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("tela direta"),
                layout: bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(smp) },
                    wgpu::BindGroupEntry { binding: 2, resource: uni.as_entire_binding() },
                ],
            });
            res.telas.insert(self.id, DTela { version: u64::MAX, dims, src, uni, bg, params: [-1.0; 4] });
        }
        let t = res.telas.get_mut(&self.id).unwrap();
        if t.version != self.version {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture: &t.src, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                &self.img.rgba[..],
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(self.img.w * 4), rows_per_image: Some(self.img.h) },
                wgpu::Extent3d { width: self.img.w, height: self.img.h, depth_or_array_layers: 1 },
            );
            t.version = self.version;
        }
        let params = [self.img.w as f32, self.img.h as f32, self.filtro as f32, self.n.max(1) as f32];
        if t.params != params {
            queue.write_buffer(&t.uni, 0, &params.iter().flat_map(|f| f.to_le_bytes()).collect::<Vec<u8>>());
            t.params = params;
        }
        Vec::new()
    }

    fn paint(&self, _info: egui::PaintCallbackInfo, rp: &mut wgpu::RenderPass<'static>, resources: &CallbackResources) {
        let Some(res) = resources.get::<DRes>() else { return };
        let (Some((pipe, _, _)), Some(t)) = (res.pipe.as_ref(), res.telas.get(&self.id)) else { return };
        rp.set_pipeline(pipe);
        rp.set_bind_group(0, &t.bg, &[]);
        rp.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Roda EASU + RCAS numa GPU sem janela e devolve o resultado (RGBA, `dst` pixels; largura múltipla de 64).
    fn run(img: &Arc<ScreenImage>, dst: [u32; 2], sharp: f32) -> Vec<u8> {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).expect("sem GPU");
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).expect("sem dispositivo");
        let mut res = CallbackResources::default();
        let fsr = Fsr { id: 0, img: img.clone(), version: 1, dst, sharp, format: wgpu::TextureFormat::Rgba8Unorm };
        let mut enc = device.create_command_encoder(&Default::default());
        let sd = ScreenDescriptor { size_in_pixels: dst, pixels_per_point: 1.0 };
        let _ = fsr.prepare(&device, &queue, &sd, &mut enc, &mut res);
        let out = &res.get::<Res>().unwrap().screens[&0].out_tex;
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (dst[0] * dst[1] * 4) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: out, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(dst[0] * 4), rows_per_image: Some(dst[1]) },
            },
            wgpu::Extent3d { width: dst[0], height: dst[1], depth_or_array_layers: 1 },
        );
        queue.submit([enc.finish()]);
        let slice = buf.slice(..);
        slice.map_async(wgpu::MapMode::Read, |r| r.unwrap());
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        let data = slice.get_mapped_range().expect("mapa").to_vec();
        data
    }

    /// Desenha uma tela pelo caminho direto numa textura `dst` e devolve os pixels.
    fn run_direta(img: &Arc<ScreenImage>, dst: [u32; 2], filtro: u32, n: u32) -> Vec<u8> {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).expect("sem GPU");
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).expect("sem dispositivo");
        let mut res = CallbackResources::default();
        let d = Direta { id: 0, img: img.clone(), version: 1, filtro, n, format: wgpu::TextureFormat::Rgba8Unorm };
        let mut enc = device.create_command_encoder(&Default::default());
        let sd = ScreenDescriptor { size_in_pixels: dst, pixels_per_point: 1.0 };
        let _ = d.prepare(&device, &queue, &sd, &mut enc, &mut res);
        let out = texture(&device, dst[0], dst[1], wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC, "teste");
        let view = out.create_view(&Default::default());
        {
            let mut rp = enc
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: None,
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                })
                .forget_lifetime();
            let info = egui::PaintCallbackInfo {
                viewport: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(dst[0] as f32, dst[1] as f32)),
                clip_rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(dst[0] as f32, dst[1] as f32)),
                pixels_per_point: 1.0,
                screen_size_px: dst,
            };
            d.paint(info, &mut rp, &res);
        }
        let row = (dst[0] * 4).next_multiple_of(256);
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (row * dst[1]) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &out, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(dst[1]) } },
            wgpu::Extent3d { width: dst[0], height: dst[1], depth_or_array_layers: 1 },
        );
        queue.submit([enc.finish()]);
        let slice = buf.slice(..);
        slice.map_async(wgpu::MapMode::Read, |r| r.unwrap());
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        let data = slice.get_mapped_range().expect("mapa").to_vec();
        data.chunks(row as usize).flat_map(|l| l[..(dst[0] * 4) as usize].to_vec()).collect()
    }

    /// Caminho direto, destino exatamente n× maior: "Nítido" e "Misto" (sharp bilinear com o mesmo n) têm de dar a
    /// ampliação por vizinho mais próximo (as amostras caem nos centros dos pixels ampliados), e "Suave" (bilinear)
    /// tem de misturar nas bordas entre texels.
    #[test]
    #[ignore]
    fn tela_direta_filtros() {
        let (w, h, n) = (16u32, 8u32, 4u32);
        let mut rgba = Vec::new();
        for i in 0..w * h {
            let v = (i * 97 % 251) as u8;
            rgba.extend_from_slice(&[v, 255 - v, (i * 13 % 256) as u8, 255]);
        }
        let img = Arc::new(ScreenImage { w, h, rgba: Bytes::Own(rgba.clone()) });
        let dst = [w * n, h * n];
        let ampliada: Vec<u8> = (0..dst[1])
            .flat_map(|y| (0..dst[0]).flat_map(move |x| ((y / n) * w + x / n) as usize * 4..((y / n) * w + x / n) as usize * 4 + 4))
            .map(|i| rgba[i])
            .collect();
        let diff = |a: &[u8], b: &[u8]| a.iter().zip(b).map(|(x, y)| (*x as i32 - *y as i32).abs()).max().unwrap();
        let nitido = run_direta(&img, dst, 0, 1);
        let misto = run_direta(&img, dst, 2, n);
        let suave = run_direta(&img, dst, 1, 1);
        println!("nítido {} · misto {} · suave {} (diferença máxima para a ampliação por vizinho)", diff(&nitido, &ampliada), diff(&misto, &ampliada), diff(&suave, &ampliada));
        assert_eq!(diff(&nitido, &ampliada), 0);
        assert!(diff(&misto, &ampliada) <= 1, "sharp bilinear diferente da ampliação inteira");
        assert!(diff(&suave, &ampliada) > 8, "o bilinear deveria misturar nas bordas");
    }

    fn gradient(w: u32, h: u32) -> Arc<ScreenImage> {
        let mut rgba = Vec::new();
        for y in 0..h {
            for x in 0..w {
                rgba.extend_from_slice(&[(x * 255 / (w - 1)) as u8, (y * 255 / (h - 1)) as u8, ((x + y) * 255 / (w + h - 2)) as u8, 255]);
            }
        }
        Arc::new(ScreenImage { w, h, rgba: Bytes::Own(rgba) })
    }

    /// 1:1 e sem nitidez, o EASU tem de devolver (quase) a própria imagem numa área suave.
    #[test]
    #[ignore]
    fn fsr_easu_identidade_em_gradiente() {
        let img = gradient(64, 48);
        let out = run(&img, [64, 48], 0.0);
        let max = img.rgba.chunks_exact(4).zip(out.chunks_exact(4)).map(|(a, b)| (0..3).map(|c| (a[c] as i32 - b[c] as i32).abs()).max().unwrap()).max().unwrap();
        println!("EASU 1:1 em gradiente: diferença máxima {max}");
        assert!(max <= 3, "o EASU 1:1 mudou a imagem em {max} níveis");
    }

    /// 2× de uma borda vertical: saída em [0, 255], monotônica através da borda e com a transição curta (borda
    /// preservada, sem halo de mais de 2 níveis com o RCAS desligado).
    #[test]
    #[ignore]
    fn fsr_easu_amplia_borda_sem_halo() {
        let (w, h) = (64u32, 48u32);
        let mut rgba = Vec::new();
        for _y in 0..h {
            for x in 0..w {
                let v = if x < w / 2 { 40u8 } else { 215u8 };
                rgba.extend_from_slice(&[v, v, v, 255]);
            }
        }
        let img = Arc::new(ScreenImage { w, h, rgba: Bytes::Own(rgba) });
        let out = run(&img, [128, 96], 0.0);
        let row = &out[(48 * 128 * 4) as usize..(49 * 128 * 4) as usize];
        let r: Vec<i32> = row.chunks_exact(4).map(|p| p[0] as i32).collect();
        println!("EASU 2× na borda: {:?}", &r[58..70]);
        assert!(r.windows(2).all(|p| p[1] >= p[0] - 2), "não monotônica através da borda");
        assert!(r[..50].iter().all(|&v| (v - 40).abs() <= 2), "halo no lado escuro");
        assert!(r[78..].iter().all(|&v| (v - 215).abs() <= 2), "halo no lado claro");
        let trans = r.iter().filter(|&&v| v > 45 && v < 210).count();
        assert!(trans <= 4, "borda borrada: {trans} pixels de transição");
        // com o RCAS no máximo, a borda ganha contraste local mas nada sai de 0..255 (u8 já garante) e o plano fica
        let sharp = run(&img, [128, 96], 1.0);
        let rs: Vec<i32> = sharp[(48 * 128 * 4) as usize..(49 * 128 * 4) as usize].chunks_exact(4).map(|p| p[0] as i32).collect();
        assert!(rs[..40].iter().all(|&v| (v - 40).abs() <= 2) && rs[88..].iter().all(|&v| (v - 215).abs() <= 2), "RCAS mexeu nas áreas planas");
    }
}
