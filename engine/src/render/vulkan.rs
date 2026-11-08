//! Backend Vulkan do PICA (o principal). Fora da tela: não há swapchain — o resultado volta para a memória guest e
//! o GSP (DisplayTransfer etc.) e a janela continuam iguais.
//!
//! Desenho (simples e correto primeiro):
//! - por draw chegam vértices já sombreados/recortados/projetados (`render::prepare`) e os uniforms do ubershader
//!   (`render::uniforms`); estados fixos (blending, logic op, máscaras, depth/stencil) viram pipelines em cache;
//!   viewport, scissor, cor de blending e referência/máscaras do stencil são dinâmicos;
//! - render targets ("superfícies") são imagens RGBA8 / D32F+S8 por (endereço, largura, altura, formato). Linha da
//!   imagem = y do rasterizador do PICA; na memória fica em blocos 8×8 com linhas de baixo para cima
//!   (`tiled_offset(x, altura − 1 − y)`), exatamente como o soft escreve;
//! - "lote" = o que é gravado entre dois `flush`. No 1º uso de uma superfície no lote, a memória guest é comparada
//!   com a cópia do último sincronismo: se mudou (MemoryFill, CPU, outra superfície), é carregada de novo. No fim de
//!   cada lista com draws (`render::end_list`) tudo o que foi desenhado volta para a memória no formato do PICA;
//! - se um draw lê (textura) ou usa como alvo uma faixa de memória que outra superfície do lote sujou, o lote é
//!   descarregado antes (render-to-texture e aliasing de endereços continuam corretos);
//! - texturas decodificadas na CPU (`pica::texture::texel`) para RGBA8, em cache por (endereço, tamanho, formato,
//!   hash do conteúdo); wrap/borda/bilinear são feitos no shader com texelFetch (iguais ao `texture::sample`).
//!
//! Profundidade: o inteiro do formato do PICA (16 ou 24 bits, truncado como no soft) é guardado no D32F como
//! zi·2⁻²⁴ — exato, então as comparações são as mesmas do soft. Todas as imagens ficam em `GENERAL`, com barreiras
//! globais (sem otimizar ainda).
//!
//! [H] / diferenças conhecidas para o soft: blending em ponto flutuante (arredonda em vez de truncar, ±1);
//! dentro de um lote o destino de formatos com menos bits (RGB565, RGBA4, RGB5A1) não é requantizado entre draws;
//! `color_read`/`depth_read` desligados (o soft lê 0 do destino) não são reproduzidos.

use super::convert;
use super::{color_bpp, depth_bpp, hash_bytes, uniforms, DrawUniforms, GpuVtx};
use crate::hle::gpu::{pica_color_fmt_to_gsp, read_pixel, tiled_offset, write_pixel};
use crate::hle::mem;
use crate::pica::raster::Ctx;
use crate::pica::texture::{self, Tex};
use ash::vk;
use std::collections::HashMap;
use std::sync::Mutex;
use yw1_rt::{r16, r32, r8, w16, w32, w8};

static VS_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/pica_vert.spv"));
static FS_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/pica_frag.spv"));

const VBUF_SIZE: u64 = 32 << 20;
const UBUF_SIZE: u64 = 8 << 20;
const RING_SIZE: u64 = 96 << 20;
/// Limite de memória das texturas em cache (imagens RGBA8). Acima disso descarta as menos usadas recentemente.
const TEX_BUDGET: u64 = 384 << 20;
const MAX_SETS: u32 = 8192;
const COLOR_FMT: vk::Format = vk::Format::R8G8B8A8_UNORM;
const DEPTH_FMT: vk::Format = vk::Format::D32_SFLOAT_S8_UINT;
const VTX_STRIDE: u32 = std::mem::size_of::<GpuVtx>() as u32;
const UBO_SIZE: u64 = std::mem::size_of::<DrawUniforms>() as u64;

/// funções de comparação do PICA (Never, Always, Equal, NotEqual, Less, LEqual, Greater, GEqual) → Vulkan
const CMP: [i32; 8] = [0, 7, 2, 5, 1, 3, 4, 6];
/// logic ops do PICA (Clear, And, AndReverse, Copy, Set, CopyInverted, NoOp, Invert, Nand, Or, Nor, Xor, Equiv,
/// AndInverted, OrReverse, OrInverted) → Vulkan
const LOGIC: [i32; 16] = [0, 1, 2, 3, 15, 12, 5, 10, 14, 7, 8, 6, 9, 4, 11, 13];

static VK: Mutex<Option<Vk>> = Mutex::new(None);

/// Inicializa o dispositivo (uma vez). Devolve o nome do GPU.
pub fn init() -> Result<String, String> {
    let mut g = VK.lock().unwrap();
    if let Some(v) = g.as_ref() {
        return Ok(v.name.clone());
    }
    // SAFETY: chamadas Vulkan com estruturas válidas; o objeto vive até o fim do processo
    let v = unsafe { Vk::new()? };
    let n = v.name.clone();
    *g = Some(v);
    Ok(n)
}

/// Grava um draw (vértices já preparados) no lote atual.
pub fn draw(ctx: &Ctx, verts: &[GpuVtx]) {
    let mut g = VK.lock().unwrap();
    let Some(v) = g.as_mut() else { return };
    // SAFETY: ver `init`
    if let Err(e) = unsafe { v.draw(ctx, verts) } {
        v.warn(&format!("draw: {e}"));
    }
}

/// Fim de uma lista de comandos com draws. O lote NÃO é executado aqui: continua aberto até alguém precisar da
/// memória (`before_read`/`before_write`, textura que lê uma superfície) ou os buffers encherem — normalmente uma vez
/// por quadro, no DisplayTransfer. `YW_VK_EAGER=1` executa e devolve tudo aqui (comportamento antigo).
pub fn end_list() {
    let mut g = VK.lock().unwrap();
    let Some(v) = g.as_mut() else { return };
    v.list += 1;
    if eager() {
        // SAFETY: ver `init`
        if let Err(e) = unsafe { v.flush() } {
            v.warn(&format!("flush: {e}"));
        }
    }
}

/// Executa o lote e devolve os render targets para a memória guest.
pub fn flush() {
    let mut g = VK.lock().unwrap();
    let Some(v) = g.as_mut() else { return };
    // SAFETY: ver `init`
    if let Err(e) = unsafe { v.flush() } {
        v.warn(&format!("flush: {e}"));
    }
}

/// Antes de alguém (DisplayTransfer, TextureCopy) LER a faixa `[lo, hi)` da memória guest: superfícies com conteúdo
/// só na GPU que a tocam voltam para a memória agora.
pub fn before_read(lo: u32, hi: u32) {
    with(|v| unsafe { v.sync_range(lo, hi, false) });
}

/// Antes de alguém (MemoryFill, DisplayTransfer, TextureCopy) ESCREVER em `[lo, hi)`: superfícies pendentes cobertas
/// por inteiro são descartadas (a escrita vence); as parcialmente cobertas voltam para a memória antes.
pub fn before_write(lo: u32, hi: u32) {
    with(|v| unsafe { v.sync_range(lo, hi, true) });
}

/// Depois de um MemoryFill em `[lo, hi)` com padrão de `fill_bpp` bytes: superfícies inteiramente cobertas cujo
/// pixel tem esse tamanho ficam uniformes — no próximo uso viram um clear na GPU em vez de upload pela CPU.
pub fn after_fill(lo: u32, hi: u32, fill_bpp: u32) {
    with(|v| v.fill_hint(lo, hi, fill_bpp));
}

/// Tudo o que só está na GPU volta para a memória (diagnóstico / antes de algo ler a memória inteira).
pub fn sync_all() {
    with(|v| unsafe { v.sync_range(0, u32::MAX, false) });
}

fn with(f: impl FnOnce(&mut Vk) -> Result<(), String>) {
    let mut g = VK.lock().unwrap();
    let Some(v) = g.as_mut() else { return };
    if let Err(e) = f(v) {
        v.warn(&format!("sincronismo: {e}"));
    }
}

/// Contadores: draws, lotes, uploads de superfície, texturas criadas, pipelines, µs esperando o GPU, µs convertendo superfícies (memória ↔ imagem), µs decodificando texturas.
pub fn stats() -> [u64; 8] {
    let g = VK.lock().unwrap();
    g.as_ref().map_or([0; 8], |v| v.stats)
}

macro_rules! vkr {
    ($e:expr, $what:expr) => {
        $e.map_err(|e| format!("{}: {:?}", $what, e))?
    };
}

struct HostBuf {
    buf: vk::Buffer,
    mem: vk::DeviceMemory,
    ptr: *mut u8,
    size: u64,
}

struct Img {
    image: vk::Image,
    mem: vk::DeviceMemory,
    view: vk::ImageView,
}

#[derive(Hash, Eq, PartialEq, Clone, Copy, Debug)]
struct SurfKey {
    addr: u32,
    w: u32,
    h: u32,
    fmt: u32,
    depth: bool,
}

impl SurfKey {
    fn bpp(&self) -> u32 {
        if self.depth {
            depth_bpp(self.fmt)
        } else {
            color_bpp(self.fmt)
        }
    }
    fn bytes(&self) -> u32 {
        self.w * self.h * self.bpp()
    }
    fn range(&self) -> (u32, u32) {
        (self.addr, self.addr + self.bytes())
    }
}

struct Surface {
    img: Img,
    readback: HostBuf,
    /// memória guest no último sincronismo (vazio = forçar upload)
    snapshot: Vec<u8>,
    synced: bool,
    /// desenhada no lote que está sendo gravado
    dirty: bool,
    /// a imagem da GPU é mais nova que a memória guest: o conteúdo está no `readback` (copiado no fim do lote) e só
    /// volta para a memória quando alguém precisa ler/escrever essa faixa (`sync_range`) — devolução preguiçosa
    pending: bool,
    /// a memória foi preenchida por inteiro (MemoryFill) com este valor já decodificado: no próximo uso, clear na GPU
    clear: Option<Clear>,
    last_used: u64,
}

#[derive(Clone, Copy, Debug)]
enum Clear {
    Color([f32; 4]),
    Depth(f32, u32),
}

#[derive(Hash, Eq, PartialEq, Clone, Copy)]
struct TexKey {
    addr: u32,
    w: u32,
    h: u32,
    fmt: u32,
    hash: u64,
}

struct TexEntry {
    img: Img,
    last_used: u64,
    bytes: u64,
}

#[derive(Hash, Eq, PartialEq, Clone, Copy, Debug)]
struct PipeKey {
    blend: bool,
    eq: [u32; 2],
    fac: [u32; 4],
    logic: u32,
    mask: u32,
    dtest: bool,
    dfunc: u32,
    dwrite: bool,
    stencil: bool,
    sfunc: u32,
    sops: [u32; 3],
}

struct Vk {
    _entry: ash::Entry,
    instance: ash::Instance,
    device: ash::Device,
    queue: vk::Queue,
    mem_props: vk::PhysicalDeviceMemoryProperties,
    name: String,
    logic_ok: bool,
    render_pass: vk::RenderPass,
    dsl: vk::DescriptorSetLayout,
    playout: vk::PipelineLayout,
    vs: vk::ShaderModule,
    fs: vk::ShaderModule,
    pipelines: HashMap<PipeKey, vk::Pipeline>,
    cmd_pre: vk::CommandBuffer,
    cmd_main: vk::CommandBuffer,
    fence: vk::Fence,
    desc_pool: vk::DescriptorPool,
    sets_used: u32,
    sampler: vk::Sampler,
    vbuf: HostBuf,
    vused: u64,
    ubuf: HostBuf,
    uused: u64,
    ustride: u64,
    ring: HostBuf,
    rused: u64,
    surfaces: HashMap<SurfKey, Surface>,
    fbs: HashMap<(SurfKey, SurfKey), vk::Framebuffer>,
    textures: HashMap<TexKey, TexEntry>,
    /// soma de `TexEntry::bytes` (memória de GPU das texturas em cache)
    tex_bytes: u64,
    /// (endereço, largura, altura, formato) → (lista, hash) — hash recalculado uma vez por lista de comandos (o lote
    /// pode cobrir várias listas e a CPU pode mudar uma textura entre elas)
    tex_hash: HashMap<(u32, u32, u32, u32), (u64, u64)>,
    /// listas de comandos terminadas (`end_list`)
    list: u64,
    dummy: Img,
    dummy_ready: bool,
    recording: bool,
    pass: Option<(SurfKey, SurfKey)>,
    batch: u64,
    warned: u32,
    stats: [u64; 8],
}

// SAFETY: o objeto só é usado com o mutex `VK`; os ponteiros mapeados apontam para memória do dispositivo que vive
// junto com ele.
unsafe impl Send for Vk {}

/// `YW_VK_EAGER=1`: devolve todos os render targets no fim de cada lote (comportamento antigo, para diagnóstico).
fn eager() -> bool {
    static E: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *E.get_or_init(|| std::env::var("YW_VK_EAGER").is_ok_and(|v| v != "0"))
}

fn align(v: u64, a: u64) -> u64 {
    v.div_ceil(a) * a
}

fn guest_slice<'a>(addr: u32, n: u32) -> Option<&'a [u8]> {
    if n == 0 || !mem::is_mapped(addr) || !mem::is_mapped(addr + n - 1) {
        return None;
    }
    // SAFETY: faixa com commit (verificada acima) dentro da reserva de 4 GB
    Some(unsafe { std::slice::from_raw_parts(yw1_rt::host_ptr(addr), n as usize) })
}

impl Vk {
    fn warn(&mut self, m: &str) {
        if self.warned < 20 {
            eprintln!("[vulkan] {m}");
            self.warned += 1;
        }
    }

    unsafe fn new() -> Result<Vk, String> {
        let entry = ash::Entry::load().map_err(|e| format!("vulkan-1.dll: {e}"))?;
        let app = vk::ApplicationInfo::default()
            .application_name(c"yw1r")
            .engine_name(c"yw1-pica")
            .api_version(vk::make_api_version(0, 1, 1, 0));
        let mut layers = Vec::new();
        if std::env::var_os("YW_VK_VALIDATION").is_some() {
            layers.push(c"VK_LAYER_KHRONOS_validation".as_ptr());
        }
        let ici = vk::InstanceCreateInfo::default().application_info(&app).enabled_layer_names(&layers);
        let instance = vkr!(entry.create_instance(&ici, None), "vkCreateInstance");

        // escolhe o dispositivo: YW_VK_DEVICE=n, senão o 1º discreto que serve, senão o 1º que serve
        let pdevs = vkr!(instance.enumerate_physical_devices(), "enumerate_physical_devices");
        let want: Option<usize> = std::env::var("YW_VK_DEVICE").ok().and_then(|s| s.parse().ok());
        let mut cands = Vec::new();
        for (i, &pd) in pdevs.iter().enumerate() {
            let props = instance.get_physical_device_properties(pd);
            let feats = instance.get_physical_device_features(pd);
            let qf = instance
                .get_physical_device_queue_family_properties(pd)
                .iter()
                .position(|q| q.queue_flags.contains(vk::QueueFlags::GRAPHICS));
            let dfp = instance.get_physical_device_format_properties(pd, DEPTH_FMT);
            let ok_depth =
                dfp.optimal_tiling_features.contains(vk::FormatFeatureFlags::DEPTH_STENCIL_ATTACHMENT);
            if let (Some(qf), true, true) = (qf, ok_depth, feats.depth_clamp == vk::TRUE) {
                cands.push((i, pd, qf as u32, props, feats));
            }
        }
        let pick = match want {
            Some(n) => cands.iter().find(|c| c.0 == n),
            None => cands
                .iter()
                .find(|c| c.3.device_type == vk::PhysicalDeviceType::DISCRETE_GPU)
                .or(cands.first()),
        };
        let Some(&(_, pdev, qf, props, feats)) = pick else {
            return Err("nenhum GPU com fila gráfica, depthClamp e D32_SFLOAT_S8_UINT".into());
        };
        let name = props
            .device_name_as_c_str()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "?".into());
        let logic_ok = feats.logic_op == vk::TRUE;
        let enabled = vk::PhysicalDeviceFeatures { depth_clamp: vk::TRUE, logic_op: feats.logic_op, ..Default::default() };
        let prio = [1.0f32];
        let qci = [vk::DeviceQueueCreateInfo::default().queue_family_index(qf).queue_priorities(&prio)];
        let dci = vk::DeviceCreateInfo::default().queue_create_infos(&qci).enabled_features(&enabled);
        let device = vkr!(instance.create_device(pdev, &dci, None), "vkCreateDevice");
        let queue = device.get_device_queue(qf, 0);
        let mem_props = instance.get_physical_device_memory_properties(pdev);

        // passada única: cor RGBA8 + profundidade/stencil, tudo em GENERAL, carregando e guardando o conteúdo
        let atts = [
            vk::AttachmentDescription::default()
                .format(COLOR_FMT)
                .samples(vk::SampleCountFlags::TYPE_1)
                .load_op(vk::AttachmentLoadOp::LOAD)
                .store_op(vk::AttachmentStoreOp::STORE)
                .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
                .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
                .initial_layout(vk::ImageLayout::GENERAL)
                .final_layout(vk::ImageLayout::GENERAL),
            vk::AttachmentDescription::default()
                .format(DEPTH_FMT)
                .samples(vk::SampleCountFlags::TYPE_1)
                .load_op(vk::AttachmentLoadOp::LOAD)
                .store_op(vk::AttachmentStoreOp::STORE)
                .stencil_load_op(vk::AttachmentLoadOp::LOAD)
                .stencil_store_op(vk::AttachmentStoreOp::STORE)
                .initial_layout(vk::ImageLayout::GENERAL)
                .final_layout(vk::ImageLayout::GENERAL),
        ];
        let cref = [vk::AttachmentReference { attachment: 0, layout: vk::ImageLayout::GENERAL }];
        let dref = vk::AttachmentReference { attachment: 1, layout: vk::ImageLayout::GENERAL };
        let sub = [vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(&cref)
            .depth_stencil_attachment(&dref)];
        let render_pass = vkr!(
            device.create_render_pass(&vk::RenderPassCreateInfo::default().attachments(&atts).subpasses(&sub), None),
            "vkCreateRenderPass"
        );

        let fsf = vk::ShaderStageFlags::FRAGMENT;
        let binds = [
            vk::DescriptorSetLayoutBinding::default()
                .binding(0)
                .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER_DYNAMIC)
                .descriptor_count(1)
                .stage_flags(fsf),
            vk::DescriptorSetLayoutBinding::default()
                .binding(1)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .descriptor_count(1)
                .stage_flags(fsf),
            vk::DescriptorSetLayoutBinding::default()
                .binding(2)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .descriptor_count(1)
                .stage_flags(fsf),
            vk::DescriptorSetLayoutBinding::default()
                .binding(3)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .descriptor_count(1)
                .stage_flags(fsf),
        ];
        let dsl = vkr!(
            device.create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&binds), None),
            "vkCreateDescriptorSetLayout"
        );
        let dsls = [dsl];
        let playout = vkr!(
            device.create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(&dsls), None),
            "vkCreatePipelineLayout"
        );
        let module = |bytes: &[u8]| -> Result<vk::ShaderModule, String> {
            let words = ash::util::read_spv(&mut std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
            Ok(vkr!(
                device.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&words), None),
                "vkCreateShaderModule"
            ))
        };
        let vs = module(VS_SPV)?;
        let fs = module(FS_SPV)?;

        let pool = vkr!(
            device.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .queue_family_index(qf)
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                None
            ),
            "vkCreateCommandPool"
        );
        let cbs = vkr!(
            device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(2)
            ),
            "vkAllocateCommandBuffers"
        );
        let fence = vkr!(device.create_fence(&vk::FenceCreateInfo::default(), None), "vkCreateFence");
        let sizes = [
            vk::DescriptorPoolSize { ty: vk::DescriptorType::UNIFORM_BUFFER_DYNAMIC, descriptor_count: MAX_SETS },
            vk::DescriptorPoolSize { ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER, descriptor_count: 3 * MAX_SETS },
        ];
        let desc_pool = vkr!(
            device.create_descriptor_pool(
                &vk::DescriptorPoolCreateInfo::default().max_sets(MAX_SETS).pool_sizes(&sizes),
                None
            ),
            "vkCreateDescriptorPool"
        );
        let sampler = vkr!(
            device.create_sampler(
                &vk::SamplerCreateInfo::default()
                    .mag_filter(vk::Filter::NEAREST)
                    .min_filter(vk::Filter::NEAREST)
                    .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
                    .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .max_lod(0.0),
                None
            ),
            "vkCreateSampler"
        );
        let ustride = align(UBO_SIZE, props.limits.min_uniform_buffer_offset_alignment.max(16));

        let mut v = Vk {
            _entry: entry,
            instance,
            device,
            queue,
            mem_props,
            name,
            logic_ok,
            render_pass,
            dsl,
            playout,
            vs,
            fs,
            pipelines: HashMap::new(),
            cmd_pre: cbs[0],
            cmd_main: cbs[1],
            fence,
            desc_pool,
            sets_used: 0,
            sampler,
            vbuf: HostBuf { buf: vk::Buffer::null(), mem: vk::DeviceMemory::null(), ptr: std::ptr::null_mut(), size: 0 },
            vused: 0,
            ubuf: HostBuf { buf: vk::Buffer::null(), mem: vk::DeviceMemory::null(), ptr: std::ptr::null_mut(), size: 0 },
            uused: 0,
            ustride,
            ring: HostBuf { buf: vk::Buffer::null(), mem: vk::DeviceMemory::null(), ptr: std::ptr::null_mut(), size: 0 },
            rused: 0,
            surfaces: HashMap::new(),
            fbs: HashMap::new(),
            textures: HashMap::new(),
            tex_bytes: 0,
            tex_hash: HashMap::new(),
            list: 0,
            dummy: Img { image: vk::Image::null(), mem: vk::DeviceMemory::null(), view: vk::ImageView::null() },
            dummy_ready: false,
            recording: false,
            pass: None,
            batch: 1,
            warned: 0,
            stats: [0; 8],
        };
        v.vbuf = v.host_buf(VBUF_SIZE, vk::BufferUsageFlags::VERTEX_BUFFER, false)?;
        v.ubuf = v.host_buf(UBUF_SIZE, vk::BufferUsageFlags::UNIFORM_BUFFER, false)?;
        v.ring = v.host_buf(RING_SIZE, vk::BufferUsageFlags::TRANSFER_SRC, false)?;
        v.dummy = v.image(1, 1, COLOR_FMT, vk::ImageUsageFlags::SAMPLED, vk::ImageAspectFlags::COLOR)?;
        if !logic_ok {
            eprintln!("[vulkan] {}: sem logicOp — só a op Copy será usada", v.name);
        }
        Ok(v)
    }

    fn mem_type(&self, bits: u32, want: vk::MemoryPropertyFlags) -> Option<u32> {
        (0..self.mem_props.memory_type_count).find(|&i| {
            bits >> i & 1 != 0 && self.mem_props.memory_types[i as usize].property_flags.contains(want)
        })
    }

    unsafe fn host_buf(&self, size: u64, usage: vk::BufferUsageFlags, cached: bool) -> Result<HostBuf, String> {
        let d = &self.device;
        let buf = vkr!(
            d.create_buffer(
                &vk::BufferCreateInfo::default().size(size).usage(usage).sharing_mode(vk::SharingMode::EXCLUSIVE),
                None
            ),
            "vkCreateBuffer"
        );
        let req = d.get_buffer_memory_requirements(buf);
        let hv = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
        let ty = if cached { self.mem_type(req.memory_type_bits, hv | vk::MemoryPropertyFlags::HOST_CACHED) } else { None }
            .or_else(|| self.mem_type(req.memory_type_bits, hv))
            .ok_or("sem memória host-visible")?;
        let mem = vkr!(
            d.allocate_memory(&vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(ty), None),
            "vkAllocateMemory(buffer)"
        );
        vkr!(d.bind_buffer_memory(buf, mem, 0), "vkBindBufferMemory");
        let ptr = vkr!(d.map_memory(mem, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty()), "vkMapMemory") as *mut u8;
        Ok(HostBuf { buf, mem, ptr, size })
    }

    unsafe fn free_buf(&self, b: &HostBuf) {
        self.device.unmap_memory(b.mem);
        self.device.destroy_buffer(b.buf, None);
        self.device.free_memory(b.mem, None);
    }

    unsafe fn image(
        &self,
        w: u32,
        h: u32,
        format: vk::Format,
        usage: vk::ImageUsageFlags,
        aspect: vk::ImageAspectFlags,
    ) -> Result<Img, String> {
        let d = &self.device;
        let image = vkr!(
            d.create_image(
                &vk::ImageCreateInfo::default()
                    .image_type(vk::ImageType::TYPE_2D)
                    .format(format)
                    .extent(vk::Extent3D { width: w, height: h, depth: 1 })
                    .mip_levels(1)
                    .array_layers(1)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .tiling(vk::ImageTiling::OPTIMAL)
                    .usage(usage)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE)
                    .initial_layout(vk::ImageLayout::UNDEFINED),
                None
            ),
            "vkCreateImage"
        );
        let req = d.get_image_memory_requirements(image);
        let ty = self
            .mem_type(req.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL)
            .or_else(|| self.mem_type(req.memory_type_bits, vk::MemoryPropertyFlags::empty()))
            .ok_or("sem memória para imagem")?;
        let mem = vkr!(
            d.allocate_memory(&vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(ty), None),
            "vkAllocateMemory(imagem)"
        );
        vkr!(d.bind_image_memory(image, mem, 0), "vkBindImageMemory");
        // superfície de profundidade: a vista cobre DEPTH|STENCIL (é o que o framebuffer pede)
        let view = vkr!(
            d.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(format)
                    .subresource_range(range(aspect)),
                None
            ),
            "vkCreateImageView"
        );
        Ok(Img { image, mem, view })
    }

    unsafe fn free_img(&self, i: &Img) {
        self.device.destroy_image_view(i.view, None);
        self.device.destroy_image(i.image, None);
        self.device.free_memory(i.mem, None);
    }

    /// UNDEFINED → GENERAL (gravado no buffer de uploads, que roda antes dos draws do lote).
    unsafe fn init_layout(&self, image: vk::Image, aspect: vk::ImageAspectFlags) {
        let b = vk::ImageMemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::empty())
            .dst_access_mask(vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE)
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::GENERAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(image)
            .subresource_range(range(aspect));
        self.device.cmd_pipeline_barrier(
            self.cmd_pre,
            vk::PipelineStageFlags::TOP_OF_PIPE,
            vk::PipelineStageFlags::ALL_COMMANDS,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[b],
        );
    }

    unsafe fn full_barrier(&self, cb: vk::CommandBuffer) {
        let m = vk::MemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
            .dst_access_mask(vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE);
        self.device.cmd_pipeline_barrier(
            cb,
            vk::PipelineStageFlags::ALL_COMMANDS,
            vk::PipelineStageFlags::ALL_COMMANDS,
            vk::DependencyFlags::empty(),
            &[m],
            &[],
            &[],
        );
    }

    unsafe fn begin(&mut self) -> Result<(), String> {
        if self.recording {
            return Ok(());
        }
        let d = &self.device;
        let bi = vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        for cb in [self.cmd_pre, self.cmd_main] {
            vkr!(d.reset_command_buffer(cb, vk::CommandBufferResetFlags::empty()), "vkResetCommandBuffer");
            vkr!(d.begin_command_buffer(cb, &bi), "vkBeginCommandBuffer");
        }
        // tudo o que os uploads (buffer `pre`, enviado antes) escreverem fica visível para os draws
        self.full_barrier(self.cmd_main);
        if !self.dummy_ready {
            self.init_layout(self.dummy.image, vk::ImageAspectFlags::COLOR);
            // conteúdo definido para o dummy (vai em descritores de unidades desligadas; nunca recebe upload)
            self.device.cmd_clear_color_image(
                self.cmd_pre,
                self.dummy.image,
                vk::ImageLayout::GENERAL,
                &vk::ClearColorValue { float32: [0.0, 0.0, 0.0, 1.0] },
                &[range(vk::ImageAspectFlags::COLOR)],
            );
            self.dummy_ready = true;
        }
        self.recording = true;
        Ok(())
    }

    /// Reserva `n` bytes no anel de upload (alinhado a 16).
    fn ring_alloc(&mut self, n: u64) -> Option<u64> {
        let off = align(self.rused, 16);
        if off + n > self.ring.size {
            return None;
        }
        self.rused = off + n;
        Some(off)
    }

    fn overlaps_dirty(&self, lo: u32, hi: u32, except: Option<SurfKey>) -> bool {
        self.surfaces.iter().any(|(k, s)| {
            if !s.dirty || Some(*k) == except || k.addr == 0 {
                return false;
            }
            let (a, b) = k.range();
            a < hi && lo < b
        })
    }

    unsafe fn draw(&mut self, ctx: &Ctx, verts: &[GpuVtx]) -> Result<(), String> {
        if verts.is_empty() {
            return Ok(());
        }
        let (w, h) = (ctx.width, ctx.height_m1 + 1);
        if w == 0 || h == 0 || w > 1024 || h > 1024 {
            return Ok(());
        }
        let vbytes = verts.len() as u64 * VTX_STRIDE as u64;
        if vbytes > VBUF_SIZE {
            self.warn(&format!("draw com {} vértices ignorado (buffer pequeno)", verts.len()));
            return Ok(());
        }
        let ck = SurfKey { addr: ctx.color_addr, w, h, fmt: ctx.color_fmt, depth: false };
        let dk = SurfKey { addr: ctx.depth_addr, w, h, fmt: ctx.depth_fmt, depth: true };
        let texs: Vec<(usize, Tex)> = ctx
            .tex
            .iter()
            .enumerate()
            .filter_map(|(i, t)| t.filter(|t| t.addr != 0 && t.w != 0 && t.h != 0).map(|t| (i, t)))
            .collect();

        // Pior caso de uso do anel de upload por ESTE draw: as texturas dele (RGBA8, até 2047² cada — os campos de
        // tamanho de 0x82/0x92/0x9A têm 11 bits) e as duas superfícies (cor 4 B/pixel; profundidade 4 B + 1 B de
        // estêncil), mais o alinhamento de 16 de cada reserva. Uma margem fixa (antes: 20 MB) ficava abaixo disso.
        let ring_need: u64 = texs.iter().map(|(_, t)| t.w as u64 * t.h as u64 * 4 + 16).sum::<u64>()
            + w as u64 * h as u64 * (4 + 5)
            + 32;
        if ring_need > RING_SIZE {
            self.warn(&format!("draw ignorado: precisa de {ring_need} bytes no anel de upload (> {RING_SIZE})"));
            return Ok(());
        }
        // descarrega antes se: falta espaço, ou o draw lê/usa memória que outra superfície do lote sujou
        if self.recording {
            let full = self.vused + vbytes > VBUF_SIZE
                || self.uused + self.ustride > UBUF_SIZE
                || self.sets_used + 1 > MAX_SETS
                || align(self.rused, 16) + ring_need > RING_SIZE;
            let tex_hazard = texs.iter().any(|(_, t)| {
                let n = t.w * t.h * texture::bits_per_texel(t.format) / 8;
                self.overlaps_dirty(t.addr, t.addr + n, None)
            });
            let rt_hazard = [ck, dk].iter().any(|k| {
                let (a, b) = k.range();
                k.addr != 0 && self.overlaps_dirty(a, b, Some(*k))
            });
            if full || tex_hazard || rt_hazard {
                self.flush()?;
            }
        }
        // texturas e alvos leem a memória: o que só está na GPU e toca essas faixas volta antes
        for (_, t) in &texs {
            let n = t.w * t.h * texture::bits_per_texel(t.format) / 8;
            self.sync_range(t.addr, t.addr + n, false)?;
        }
        for k in [ck, dk] {
            if k.addr == 0 {
                continue;
            }
            let (a, b) = k.range();
            let others: Vec<SurfKey> = self
                .surfaces
                .iter()
                .filter(|(o, s)| **o != k && s.pending && o.addr != 0 && o.range().0 < b && a < o.range().1)
                .map(|(o, _)| *o)
                .collect();
            for o in others {
                self.materialize(o);
            }
        }
        self.begin()?;

        let mut views = [self.dummy.view; 3];
        // unidades cuja memória não está mapeada: `texture` devolve o dummy 1×1, então o shader não pode usar o
        // tamanho real (texelFetch fora da imagem é indefinido sem robustImageAccess)
        let mut missing = [false; 3];
        for (i, t) in &texs {
            views[*i] = self.texture(t)?;
            missing[*i] = views[*i] == self.dummy.view;
        }
        let cview = self.surface(ck)?;
        let dview = self.surface(dk)?;
        let key = self.pipe_key(ctx);
        let pipe = self.pipeline(key)?;

        // passada: troca de alvo fecha a anterior (com barreira global entre elas)
        let d = self.device.clone();
        let cb = self.cmd_main;
        if self.pass != Some((ck, dk)) {
            if self.pass.is_some() {
                d.cmd_end_render_pass(cb);
                self.full_barrier(cb);
            }
            let fb = self.framebuffer(ck, dk, cview, dview)?;
            d.cmd_begin_render_pass(
                cb,
                &vk::RenderPassBeginInfo::default()
                    .render_pass(self.render_pass)
                    .framebuffer(fb)
                    .render_area(vk::Rect2D { offset: vk::Offset2D { x: 0, y: 0 }, extent: vk::Extent2D { width: w, height: h } }),
                vk::SubpassContents::INLINE,
            );
            self.pass = Some((ck, dk));
        }

        // scissor de inclusão (modo 3) = retângulo do Vulkan; exclusão (modo 1) fica no shader
        let (smode, sc) = ctx.scissor;
        let (mut x0, mut y0, mut x1, mut y1) = (0u32, 0u32, w, h);
        if smode == 3 {
            x0 = sc[0].min(w);
            y0 = sc[1].min(h);
            x1 = (sc[2] + 1).min(w);
            y1 = (sc[3] + 1).min(h);
            if x1 <= x0 || y1 <= y0 {
                return Ok(());
            }
        }

        // vértices e uniforms
        std::ptr::copy_nonoverlapping(verts.as_ptr() as *const u8, self.vbuf.ptr.add(self.vused as usize), vbytes as usize);
        let voff = self.vused;
        self.vused = align(self.vused + vbytes, 16);
        let mut u = uniforms(ctx);
        for (i, m) in missing.iter().enumerate() {
            if *m {
                // largura/altura 0 → o shader devolve (0, 0, 0, 255), como `texture::sample` sem textura
                u.tex[i][0] = 0;
            }
        }
        std::ptr::copy_nonoverlapping(
            &u as *const DrawUniforms as *const u8,
            self.ubuf.ptr.add(self.uused as usize),
            UBO_SIZE as usize,
        );
        let uoff = self.uused as u32;
        self.uused += self.ustride;

        let dsls = [self.dsl];
        let set = vkr!(
            d.allocate_descriptor_sets(&vk::DescriptorSetAllocateInfo::default().descriptor_pool(self.desc_pool).set_layouts(&dsls)),
            "vkAllocateDescriptorSets"
        )[0];
        self.sets_used += 1;
        let binfo = [vk::DescriptorBufferInfo { buffer: self.ubuf.buf, offset: 0, range: UBO_SIZE }];
        let iinfo: Vec<[vk::DescriptorImageInfo; 1]> = views
            .iter()
            .map(|&v| [vk::DescriptorImageInfo { sampler: self.sampler, image_view: v, image_layout: vk::ImageLayout::GENERAL }])
            .collect();
        let mut writes = vec![vk::WriteDescriptorSet::default()
            .dst_set(set)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER_DYNAMIC)
            .buffer_info(&binfo)];
        for (i, inf) in iinfo.iter().enumerate() {
            writes.push(
                vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(1 + i as u32)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .image_info(inf),
            );
        }
        d.update_descriptor_sets(&writes, &[]);

        d.cmd_bind_pipeline(cb, vk::PipelineBindPoint::GRAPHICS, pipe);
        d.cmd_set_viewport(
            cb,
            0,
            &[vk::Viewport { x: 0.0, y: 0.0, width: w as f32, height: h as f32, min_depth: 0.0, max_depth: 1.0 }],
        );
        d.cmd_set_scissor(
            cb,
            0,
            &[vk::Rect2D {
                offset: vk::Offset2D { x: x0 as i32, y: y0 as i32 },
                extent: vk::Extent2D { width: x1 - x0, height: y1 - y0 },
            }],
        );
        d.cmd_set_blend_constants(cb, &ctx.blend_color.map(|c| c as f32 / 255.0));
        let (_, _, st_wmask, st_ref, st_imask, _) = ctx.stencil;
        let ff = vk::StencilFaceFlags::FRONT_AND_BACK;
        d.cmd_set_stencil_reference(cb, ff, st_ref as u32);
        d.cmd_set_stencil_compare_mask(cb, ff, st_imask as u32);
        d.cmd_set_stencil_write_mask(cb, ff, st_wmask as u32);
        d.cmd_bind_descriptor_sets(cb, vk::PipelineBindPoint::GRAPHICS, self.playout, 0, &[set], &[uoff]);
        d.cmd_bind_vertex_buffers(cb, 0, &[self.vbuf.buf], &[voff]);
        d.cmd_draw(cb, verts.len() as u32, 1, 0, 0);
        self.stats[0] += 1;

        if key.mask != 0 && ck.addr != 0 {
            if let Some(s) = self.surfaces.get_mut(&ck) {
                s.dirty = true;
            }
        }
        if (key.dwrite || key.stencil) && dk.addr != 0 {
            if let Some(s) = self.surfaces.get_mut(&dk) {
                s.dirty = true;
            }
        }
        Ok(())
    }

    fn pipe_key(&mut self, c: &Ctx) -> PipeKey {
        let have_d = c.depth_addr != 0;
        let mut mask = if c.color_addr == 0 || !c.color_write { 0 } else { c.color_mask & 0xf };
        // formatos sem alfa (numeração do PICA: RGB8 = 1, RGB565 = 3; o 2 é RGB5A1, que TEM alfa): o soft sempre lê
        // A = 255 do destino — não deixa o A da imagem mudar
        if matches!(c.color_fmt, 1 | 3) {
            mask &= 7;
        }
        let dwrite = have_d && c.depth_write && c.depth_rw;
        let dtest_on = have_d && c.depth_test;
        let stencil = have_d && c.depth_fmt == 3 && c.stencil.0;
        let mut logic = if c.blend { 3 } else { c.logic_op & 0xf };
        if !self.logic_ok && logic != 3 {
            self.warn(&format!("logic op {logic} sem suporte no dispositivo; usando Copy"));
            logic = 3;
        }
        PipeKey {
            blend: c.blend,
            eq: [c.blend_eq.0.min(4), c.blend_eq.1.min(4)],
            fac: c.blend_fac.map(|f| f.min(14)),
            logic,
            mask,
            // sem teste mas com escrita: o Vulkan só escreve com o teste ligado → ALWAYS
            dtest: dtest_on || dwrite,
            dfunc: if dtest_on { c.depth_func & 7 } else { 1 },
            dwrite,
            stencil,
            sfunc: if stencil { c.stencil.1 & 7 } else { 1 },
            sops: if stencil { c.stencil.5.map(|o| o & 7) } else { [0; 3] },
        }
    }

    unsafe fn pipeline(&mut self, k: PipeKey) -> Result<vk::Pipeline, String> {
        if let Some(&p) = self.pipelines.get(&k) {
            return Ok(p);
        }
        let stages = [
            vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::VERTEX).module(self.vs).name(c"main"),
            vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::FRAGMENT).module(self.fs).name(c"main"),
        ];
        let binds = [vk::VertexInputBindingDescription { binding: 0, stride: VTX_STRIDE, input_rate: vk::VertexInputRate::VERTEX }];
        let attr = |location: u32, format: vk::Format, offset: u32| vk::VertexInputAttributeDescription { location, binding: 0, format, offset };
        let attrs = [
            attr(0, vk::Format::R32G32B32A32_SFLOAT, 0),
            attr(1, vk::Format::R32G32B32A32_SFLOAT, 16),
            attr(2, vk::Format::R32G32_SFLOAT, 32),
            attr(3, vk::Format::R32G32_SFLOAT, 40),
            attr(4, vk::Format::R32G32_SFLOAT, 48),
        ];
        let vi = vk::PipelineVertexInputStateCreateInfo::default()
            .vertex_binding_descriptions(&binds)
            .vertex_attribute_descriptions(&attrs);
        let ia = vk::PipelineInputAssemblyStateCreateInfo::default().topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let vp = vk::PipelineViewportStateCreateInfo::default().viewport_count(1).scissor_count(1);
        // recorte em z feito na CPU (planos do PICA) e profundidade saturada como no soft → depth clamp
        let rs = vk::PipelineRasterizationStateCreateInfo::default()
            .depth_clamp_enable(true)
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
            .line_width(1.0);
        let ms = vk::PipelineMultisampleStateCreateInfo::default().rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let sop = vk::StencilOpState {
            fail_op: vk::StencilOp::from_raw(k.sops[0] as i32),
            pass_op: vk::StencilOp::from_raw(k.sops[2] as i32),
            depth_fail_op: vk::StencilOp::from_raw(k.sops[1] as i32),
            compare_op: vk::CompareOp::from_raw(CMP[k.sfunc as usize]),
            compare_mask: 0xff,
            write_mask: 0xff,
            reference: 0,
        };
        let ds = vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(k.dtest)
            .depth_write_enable(k.dwrite)
            .depth_compare_op(vk::CompareOp::from_raw(CMP[k.dfunc as usize]))
            .stencil_test_enable(k.stencil)
            .front(sop)
            .back(sop)
            .max_depth_bounds(1.0);
        let bf = |f: u32| vk::BlendFactor::from_raw(f as i32);
        let att = [vk::PipelineColorBlendAttachmentState {
            blend_enable: k.blend as u32,
            src_color_blend_factor: bf(k.fac[0]),
            dst_color_blend_factor: bf(k.fac[1]),
            color_blend_op: vk::BlendOp::from_raw(k.eq[0] as i32),
            src_alpha_blend_factor: bf(k.fac[2]),
            dst_alpha_blend_factor: bf(k.fac[3]),
            alpha_blend_op: vk::BlendOp::from_raw(k.eq[1] as i32),
            color_write_mask: vk::ColorComponentFlags::from_raw(k.mask),
        }];
        let cbs = vk::PipelineColorBlendStateCreateInfo::default()
            .logic_op_enable(!k.blend && k.logic != 3)
            .logic_op(vk::LogicOp::from_raw(LOGIC[k.logic as usize]))
            .attachments(&att);
        let dyns = [
            vk::DynamicState::VIEWPORT,
            vk::DynamicState::SCISSOR,
            vk::DynamicState::BLEND_CONSTANTS,
            vk::DynamicState::STENCIL_COMPARE_MASK,
            vk::DynamicState::STENCIL_WRITE_MASK,
            vk::DynamicState::STENCIL_REFERENCE,
        ];
        let dy = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dyns);
        let ci = vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vi)
            .input_assembly_state(&ia)
            .viewport_state(&vp)
            .rasterization_state(&rs)
            .multisample_state(&ms)
            .depth_stencil_state(&ds)
            .color_blend_state(&cbs)
            .dynamic_state(&dy)
            .layout(self.playout)
            .render_pass(self.render_pass)
            .subpass(0);
        let p = self
            .device
            .create_graphics_pipelines(vk::PipelineCache::null(), &[ci], None)
            .map_err(|(_, e)| format!("vkCreateGraphicsPipelines: {e:?}"))?[0];
        self.pipelines.insert(k, p);
        self.stats[4] += 1;
        Ok(p)
    }

    unsafe fn framebuffer(&mut self, ck: SurfKey, dk: SurfKey, cv: vk::ImageView, dv: vk::ImageView) -> Result<vk::Framebuffer, String> {
        if let Some(&f) = self.fbs.get(&(ck, dk)) {
            return Ok(f);
        }
        let views = [cv, dv];
        let f = vkr!(
            self.device.create_framebuffer(
                &vk::FramebufferCreateInfo::default()
                    .render_pass(self.render_pass)
                    .attachments(&views)
                    .width(ck.w)
                    .height(ck.h)
                    .layers(1),
                None
            ),
            "vkCreateFramebuffer"
        );
        self.fbs.insert((ck, dk), f);
        Ok(f)
    }

    /// Superfície do alvo: cria se preciso e, no 1º uso do lote, carrega da memória guest se ela mudou.
    unsafe fn surface(&mut self, k: SurfKey) -> Result<vk::ImageView, String> {
        if !self.surfaces.contains_key(&k) {
            let (fmt, usage, aspect, rb) = if k.depth {
                (
                    DEPTH_FMT,
                    vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT,
                    vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL,
                    k.w as u64 * k.h as u64 * 5,
                )
            } else {
                (COLOR_FMT, vk::ImageUsageFlags::COLOR_ATTACHMENT, vk::ImageAspectFlags::COLOR, k.w as u64 * k.h as u64 * 4)
            };
            let img = self.image(
                k.w,
                k.h,
                fmt,
                usage | vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::TRANSFER_DST,
                aspect,
            )?;
            self.init_layout(img.image, aspect);
            let readback = self.host_buf(rb, vk::BufferUsageFlags::TRANSFER_DST, true)?;
            self.surfaces.insert(
                k,
                Surface { img, readback, snapshot: Vec::new(), synced: false, dirty: false, pending: false, clear: None, last_used: 0 },
            );
        }
        let batch = self.batch;
        let s = self.surfaces.get_mut(&k).unwrap();
        s.last_used = batch;
        let view = s.img.view;
        if k.addr == 0 || s.synced {
            return Ok(view);
        }
        // conteúdo só na GPU (devolução pendente): a imagem é a versão mais nova
        if s.pending {
            s.synced = true;
            return Ok(view);
        }
        let Some(mem) = guest_slice(k.addr, k.bytes()) else {
            s.synced = true;
            return Ok(view);
        };
        if let Some(c) = s.clear.take() {
            s.synced = true;
            s.snapshot = mem.to_vec();
            let image = s.img.image;
            match c {
                Clear::Color(rgba) => self.device.cmd_clear_color_image(
                    self.cmd_pre,
                    image,
                    vk::ImageLayout::GENERAL,
                    &vk::ClearColorValue { float32: rgba },
                    &[range(vk::ImageAspectFlags::COLOR)],
                ),
                Clear::Depth(z, st) => self.device.cmd_clear_depth_stencil_image(
                    self.cmd_pre,
                    image,
                    vk::ImageLayout::GENERAL,
                    &vk::ClearDepthStencilValue { depth: z, stencil: st },
                    &[range(vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL)],
                ),
            }
            self.stats[2] += 1;
            return Ok(view);
        }
        if !s.snapshot.is_empty() && s.snapshot.as_slice() == mem {
            s.synced = true;
            return Ok(view);
        }
        let image = s.img.image;
        let t0 = std::time::Instant::now();
        // carrega: memória (blocos 8×8, linhas de baixo para cima) → imagem linear
        let (w, h) = (k.w, k.h);
        let n = (w * h) as u64;
        let bytes = if k.depth { n * 5 } else { n * 4 };
        // Reserva o anel ANTES de marcar a superfície como sincronizada: se faltar espaço, snapshot/synced ficam
        // como estavam e o próximo uso tenta carregar de novo (antes ficava com conteúdo velho para sempre).
        let Some(off) = self.ring_alloc(bytes) else {
            return Err("anel de upload cheio (superfície)".into());
        };
        let s = self.surfaces.get_mut(&k).unwrap();
        s.synced = true;
        s.snapshot = mem.to_vec();
        let dst = std::slice::from_raw_parts_mut(self.ring.ptr.add(off as usize), bytes as usize);
        let bpp = k.bpp();
        let fast = if k.depth {
            convert::depth_to_linear(mem, w, h, bpp, k.fmt, dst)
        } else {
            convert::color_to_linear(mem, w, h, pica_color_fmt_to_gsp(k.fmt), dst)
        };
        for y in 0..if fast { 0 } else { h } {
            for x in 0..w {
                let a = k.addr + tiled_offset(x, h - 1 - y, w, bpp);
                let i = (y * w + x) as usize;
                if k.depth {
                    let (zi, st) = match bpp {
                        2 => (r16(a), 0),
                        3 => (r8(a) | r8(a + 1) << 8 | r8(a + 2) << 16, 0),
                        _ => {
                            let v = r32(a);
                            (v & 0xff_ffff, if k.fmt == 3 { v >> 24 } else { 0 })
                        }
                    };
                    let f = zi as f32 * (1.0 / 16_777_216.0);
                    dst[4 * i..4 * i + 4].copy_from_slice(&f.to_le_bytes());
                    dst[4 * n as usize + i] = st as u8;
                } else {
                    dst[4 * i..4 * i + 4].copy_from_slice(&read_pixel(a, pica_color_fmt_to_gsp(k.fmt)));
                }
            }
        }
        let regions = copy_regions(k, off);
        self.device.cmd_copy_buffer_to_image(self.cmd_pre, self.ring.buf, image, vk::ImageLayout::GENERAL, &regions);
        self.stats[2] += 1;
        self.stats[6] += t0.elapsed().as_micros() as u64;
        Ok(view)
    }

    /// Textura em cache (decodifica e envia se o conteúdo mudou).
    unsafe fn texture(&mut self, t: &Tex) -> Result<vk::ImageView, String> {
        let n = t.w * t.h * texture::bits_per_texel(t.format) / 8;
        let gk = (t.addr, t.w, t.h, t.format);
        let hash = match self.tex_hash.get(&gk) {
            Some(&(l, hv)) if l == self.list => hv,
            _ => {
                let Some(mem) = guest_slice(t.addr, n) else { return Ok(self.dummy.view) };
                let hv = hash_bytes(mem);
                self.tex_hash.insert(gk, (self.list, hv));
                hv
            }
        };
        let key = TexKey { addr: t.addr, w: t.w, h: t.h, fmt: t.format, hash };
        if let Some(e) = self.textures.get_mut(&key) {
            e.last_used = self.batch;
            return Ok(e.img.view);
        }
        let (w, h) = (t.w, t.h);
        let bytes = w as u64 * h as u64 * 4;
        let Some(off) = self.ring_alloc(bytes) else {
            return Err("anel de upload cheio (textura)".into());
        };
        let t0 = std::time::Instant::now();
        let dst = std::slice::from_raw_parts_mut(self.ring.ptr.add(off as usize), bytes as usize);
        // linha y da imagem = coordenada t inteira (texture::fetch lê a linha de memória altura − 1 − t)
        for y in 0..h {
            for x in 0..w {
                let i = 4 * (y * w + x) as usize;
                dst[i..i + 4].copy_from_slice(&texture::texel(t, x, h - 1 - y));
            }
        }
        self.stats[7] += t0.elapsed().as_micros() as u64;
        let img = self.image(
            w,
            h,
            COLOR_FMT,
            vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST,
            vk::ImageAspectFlags::COLOR,
        )?;
        self.init_layout(img.image, vk::ImageAspectFlags::COLOR);
        let region = vk::BufferImageCopy {
            buffer_offset: off,
            buffer_row_length: 0,
            buffer_image_height: 0,
            image_subresource: layers(vk::ImageAspectFlags::COLOR),
            image_offset: vk::Offset3D::default(),
            image_extent: vk::Extent3D { width: w, height: h, depth: 1 },
        };
        self.device.cmd_copy_buffer_to_image(self.cmd_pre, self.ring.buf, img.image, vk::ImageLayout::GENERAL, &[region]);
        let view = img.view;
        self.textures.insert(key, TexEntry { img, last_used: self.batch, bytes });
        self.tex_bytes += bytes;
        self.stats[3] += 1;
        Ok(view)
    }

    /// Executa o lote, espera e devolve as superfícies desenhadas para a memória guest.
    unsafe fn flush(&mut self) -> Result<(), String> {
        if !self.recording {
            return Ok(());
        }
        let d = self.device.clone();
        let cb = self.cmd_main;
        if self.pass.take().is_some() {
            d.cmd_end_render_pass(cb);
        }
        self.full_barrier(cb);
        let dirty: Vec<SurfKey> = self.surfaces.iter().filter(|(_, s)| s.dirty).map(|(k, _)| *k).collect();
        for k in &dirty {
            let s = &self.surfaces[k];
            d.cmd_copy_image_to_buffer(cb, s.img.image, vk::ImageLayout::GENERAL, s.readback.buf, &copy_regions(*k, 0));
        }
        let m = vk::MemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .dst_access_mask(vk::AccessFlags::HOST_READ);
        d.cmd_pipeline_barrier(
            cb,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::HOST,
            vk::DependencyFlags::empty(),
            &[m],
            &[],
            &[],
        );
        vkr!(d.end_command_buffer(self.cmd_pre), "vkEndCommandBuffer");
        vkr!(d.end_command_buffer(cb), "vkEndCommandBuffer");
        let cbs = [self.cmd_pre, cb];
        let si = [vk::SubmitInfo::default().command_buffers(&cbs)];
        let t0 = std::time::Instant::now();
        let res = d.queue_submit(self.queue, &si, self.fence);
        self.recording = false;
        vkr!(res, "vkQueueSubmit");
        vkr!(d.wait_for_fences(&[self.fence], true, u64::MAX), "vkWaitForFences");
        vkr!(d.reset_fences(&[self.fence]), "vkResetFences");
        self.stats[5] += t0.elapsed().as_micros() as u64;
        let t1 = std::time::Instant::now();

        // imagem → memória guest: preguiçoso. O conteúdo fica no `readback` e só é convertido quando alguém precisar
        // da faixa (`sync_range`). `YW_VK_EAGER=1` volta ao comportamento antigo (devolve tudo no fim de cada lote).
        for k in &dirty {
            let s = self.surfaces.get_mut(k).unwrap();
            s.dirty = false;
            s.pending = true;
            s.clear = None;
        }
        if eager() {
            for k in &dirty {
                self.materialize(*k);
            }
        }
        self.stats[6] += t1.elapsed().as_micros() as u64;
        for s in self.surfaces.values_mut() {
            s.synced = false;
        }
        vkr!(d.reset_descriptor_pool(self.desc_pool, vk::DescriptorPoolResetFlags::empty()), "vkResetDescriptorPool");
        self.sets_used = 0;
        self.vused = 0;
        self.uused = 0;
        self.rused = 0;
        self.batch += 1;
        self.stats[1] += 1;
        self.evict();
        Ok(())
    }

    /// Converte o `readback` de uma superfície pendente para a memória guest (mesmo formato que o soft escreveria).
    unsafe fn materialize(&mut self, k: SurfKey) {
        let Some(s) = self.surfaces.get_mut(&k) else { return };
        if !s.pending {
            return;
        }
        s.pending = false;
        let t0 = std::time::Instant::now();
        {
            if guest_slice(k.addr, k.bytes()).is_none() {
                return;
            }
            let (w, h) = (k.w, k.h);
            let n = (w * h) as usize;
            let src = std::slice::from_raw_parts(s.readback.ptr, if k.depth { 5 * n } else { 4 * n });
            let bpp = k.bpp();
            // SAFETY: faixa mapeada (conferida acima); o jogo está parado nesta chamada (lista/GSP)
            let memw = std::slice::from_raw_parts_mut(yw1_rt::host_ptr(k.addr), k.bytes() as usize);
            let fast = if k.depth {
                convert::depth_from_linear(src, w, h, bpp, k.fmt, memw)
            } else {
                convert::color_from_linear(src, w, h, pica_color_fmt_to_gsp(k.fmt), memw)
            };
            if fast {
                let lossy = !k.depth && k.fmt >= 2;
                s.snapshot = if lossy { Vec::new() } else { memw.to_vec() };
                self.stats[6] += t0.elapsed().as_micros() as u64;
                return;
            }
            let (w, h) = (k.w, k.h);
            let n = (w * h) as usize;
            let src = std::slice::from_raw_parts(s.readback.ptr, if k.depth { 5 * n } else { 4 * n });
            let bpp = k.bpp();
            for y in 0..h {
                for x in 0..w {
                    let a = k.addr + tiled_offset(x, h - 1 - y, w, bpp);
                    let i = (y * w + x) as usize;
                    if k.depth {
                        let f = f32::from_le_bytes(src[4 * i..4 * i + 4].try_into().unwrap());
                        let zi = ((f * 16_777_216.0).round() as u32).min(0xff_ffff);
                        match bpp {
                            2 => w16(a, zi.min(0xffff)),
                            3 => {
                                w8(a, zi);
                                w8(a + 1, zi >> 8);
                                w8(a + 2, zi >> 16);
                            }
                            _ => {
                                let st = if k.fmt == 3 { src[4 * n + i] as u32 } else { r32(a) >> 24 };
                                w32(a, st << 24 | zi);
                            }
                        }
                    } else {
                        write_pixel(a, pica_color_fmt_to_gsp(k.fmt), src[4 * i..4 * i + 4].try_into().unwrap());
                    }
                }
            }
            // formatos com menos bits: força recarregar (requantiza a imagem) no próximo lote
            let lossy = !k.depth && k.fmt >= 2;
            s.snapshot = if lossy { Vec::new() } else { guest_slice(k.addr, k.bytes()).unwrap().to_vec() };
        }
        self.stats[6] += t0.elapsed().as_micros() as u64;
    }

    /// Alguém vai ler (`write` = falso) ou escrever em `[lo, hi)` da memória guest. Se o lote gravado desenhou ali,
    /// executa-o; depois devolve as superfícies pendentes que tocam a faixa (numa escrita, as cobertas por inteiro
    /// são só descartadas — a escrita sobrescreve tudo mesmo).
    unsafe fn sync_range(&mut self, lo: u32, hi: u32, write: bool) -> Result<(), String> {
        if self.recording && self.overlaps_dirty(lo, hi, None) {
            self.flush()?;
        }
        let hit: Vec<SurfKey> = self
            .surfaces
            .iter()
            .filter(|(k, s)| {
                let (a, b) = k.range();
                s.pending && k.addr != 0 && a < hi && lo < b
            })
            .map(|(k, _)| *k)
            .collect();
        for k in hit {
            let (a, b) = k.range();
            if write && lo <= a && b <= hi {
                let s = self.surfaces.get_mut(&k).unwrap();
                s.pending = false;
                s.snapshot = Vec::new();
                s.synced = false;
            } else {
                self.materialize(k);
            }
        }
        Ok(())
    }

    /// MemoryFill terminou em `[lo, hi)`: superfícies cobertas por inteiro, sem conteúdo pendente, com pixel do mesmo
    /// tamanho do padrão → guarda o valor (decodificado da própria memória) para limpar na GPU no próximo uso.
    fn fill_hint(&mut self, lo: u32, hi: u32, fill_bpp: u32) -> Result<(), String> {
        for (k, s) in self.surfaces.iter_mut() {
            let (a, b) = k.range();
            if k.addr == 0 || s.pending || s.dirty || !(lo <= a && b <= hi) || k.bpp() != fill_bpp {
                continue;
            }
            let Some(m) = guest_slice(k.addr, k.bpp()) else { continue };
            s.clear = Some(if k.depth {
                let (zi, st) = match k.bpp() {
                    2 => (u16::from_le_bytes([m[0], m[1]]) as u32, 0),
                    3 => (m[0] as u32 | (m[1] as u32) << 8 | (m[2] as u32) << 16, 0),
                    _ => {
                        let v = u32::from_le_bytes([m[0], m[1], m[2], m[3]]);
                        (v & 0xff_ffff, if k.fmt == 3 { v >> 24 } else { 0 })
                    }
                };
                Clear::Depth(zi as f32 * (1.0 / 16_777_216.0), st)
            } else {
                let p = convert::decode(pica_color_fmt_to_gsp(k.fmt), m);
                Clear::Color(p.map(|c| c as f32 / 255.0))
            });
            s.synced = false;
        }
        Ok(())
    }

    /// Libera texturas e superfícies sem uso há muito tempo (só entre lotes, com o GPU parado).
    unsafe fn evict(&mut self) {
        let b = self.batch;
        // (1) versões velhas: quando a CPU muda o conteúdo de uma textura (vídeo, texto, atlas), o hash novo cria outra
        // entrada para o mesmo (endereço, tamanho, formato). Num lote só uma versão é usada (o hash é calculado uma
        // vez por lote), então toda versão usada antes da mais recente já não serve e sai agora (o GPU está parado).
        let mut newest: HashMap<(u32, u32, u32, u32), u64> = HashMap::new();
        for (k, e) in &self.textures {
            let n = newest.entry((k.addr, k.w, k.h, k.fmt)).or_insert(0);
            *n = (*n).max(e.last_used);
        }
        let mut old: Vec<TexKey> = self
            .textures
            .iter()
            .filter(|(k, e)| e.last_used < newest[&(k.addr, k.w, k.h, k.fmt)])
            .map(|(k, _)| *k)
            .collect();
        // (2) muitas entradas: as sem uso há 300 lotes
        if self.textures.len() > 1500 {
            old.extend(self.textures.iter().filter(|(_, e)| e.last_used + 300 < b).map(|(k, _)| *k));
        }
        old.sort_unstable_by_key(|k| (k.addr, k.w, k.h, k.fmt, k.hash));
        old.dedup();
        for k in old {
            let e = self.textures.remove(&k).unwrap();
            self.tex_bytes -= e.bytes;
            self.free_img(&e.img);
        }
        // (3) limite de memória: descarta as menos usadas recentemente até caber
        if self.tex_bytes > TEX_BUDGET {
            let mut lru: Vec<(u64, TexKey)> = self.textures.iter().map(|(k, e)| (e.last_used, *k)).collect();
            lru.sort_unstable_by_key(|(u, _)| *u);
            for (_, k) in lru {
                if self.tex_bytes <= TEX_BUDGET {
                    break;
                }
                let e = self.textures.remove(&k).unwrap();
                self.tex_bytes -= e.bytes;
                self.free_img(&e.img);
            }
        }
        if self.tex_hash.len() > 4096 {
            let l = self.list;
            self.tex_hash.retain(|_, (ll, _)| *ll + 2 >= l);
        }
        if self.surfaces.len() > 32 {
            let old: Vec<SurfKey> = self.surfaces.iter().filter(|(_, s)| s.last_used + 3000 < b).map(|(k, _)| *k).collect();
            for k in old {
                let fbs: Vec<(SurfKey, SurfKey)> = self.fbs.keys().filter(|(c, d)| *c == k || *d == k).copied().collect();
                for f in fbs {
                    let fb = self.fbs.remove(&f).unwrap();
                    self.device.destroy_framebuffer(fb, None);
                }
                let s = self.surfaces.remove(&k).unwrap();
                self.free_img(&s.img);
                self.free_buf(&s.readback);
            }
        }
    }
}

impl Drop for Vk {
    fn drop(&mut self) {
        // SAFETY: espera o dispositivo antes de destruir; objetos que sobram são liberados com o processo
        unsafe {
            let _ = self.device.device_wait_idle();
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}

fn range(aspect: vk::ImageAspectFlags) -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange { aspect_mask: aspect, base_mip_level: 0, level_count: 1, base_array_layer: 0, layer_count: 1 }
}

fn layers(aspect: vk::ImageAspectFlags) -> vk::ImageSubresourceLayers {
    vk::ImageSubresourceLayers { aspect_mask: aspect, mip_level: 0, base_array_layer: 0, layer_count: 1 }
}

/// Regiões de cópia buffer ↔ superfície: cor RGBA8 no início; profundidade f32 no início e stencil u8 logo depois.
fn copy_regions(k: SurfKey, off: u64) -> Vec<vk::BufferImageCopy> {
    let ext = vk::Extent3D { width: k.w, height: k.h, depth: 1 };
    let reg = |o: u64, a: vk::ImageAspectFlags| vk::BufferImageCopy {
        buffer_offset: o,
        buffer_row_length: 0,
        buffer_image_height: 0,
        image_subresource: layers(a),
        image_offset: vk::Offset3D::default(),
        image_extent: ext,
    };
    if k.depth {
        vec![
            reg(off, vk::ImageAspectFlags::DEPTH),
            reg(off + k.w as u64 * k.h as u64 * 4, vk::ImageAspectFlags::STENCIL),
        ]
    } else {
        vec![reg(off, vk::ImageAspectFlags::COLOR)]
    }
}
