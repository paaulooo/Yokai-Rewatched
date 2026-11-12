//! Resolução interna N× (e FXAA de cena) do backend Vulkan.
//!
//! Ideia: os alvos de render NATIVOS (o que o jogo lê de volta: DisplayTransfer, texturas que vêm de alvos, DMA…)
//! ficam exatamente como são — é a referência dos testes (`pica_frame*`) e do Azahar. Cada alvo de TELA (cor 480×400 ou
//! 240×400 + a profundidade dele) ganha uma "sombra" de N× o tamanho que recebe os MESMOS draws, só para a imagem que vai
//! à janela:
//!
//! * `draw` grava cada draw duas vezes: no buffer de comandos principal (nativo) e em `cmd_shadow` (sombra, viewport e
//!   scissor × N, push constant `n` para o scissor de exclusão do `pica.frag`). Os pipelines são os mesmos (passadas
//!   compatíveis: mesmos formatos e 1 amostra).
//! * Tudo que reinicia o conteúdo nativo (MemoryFill → clear, upload da memória) reinicia também a sombra (`ShadowSrc`);
//!   o upload decodifica a memória e replica cada pixel N×N (`expand`).
//! * No DisplayTransfer que copia o alvo para o framebuffer do LCD, `present` roda `present.comp`: gira/inverte/reduz
//!   a sombra como o DisplayTransfer faz no nativo, em N× — a imagem final (400N × 240N) vai para `published`, com o
//!   hash do framebuffer nativo escrito pela transferência; a janela só a usa se o hash ainda bate (o jogo pode ter
//!   escrito outra coisa ali).
//! * FXAA (opção): `fxaa.comp` roda sobre a sombra na passagem da cena 3D (draws com teste de profundidade) para a
//!   interface (draws sem), ou no `present` se a cena não teve interface — nunca sobre o texto/HUD.
//!
//! Mudar N/FXAA no meio do jogo (`hires_reset`): executa o lote, devolve tudo à memória, descarta as sombras e
//! recarrega as superfícies da memória (já atualizada) — as sombras renascem no próximo uso.
//!
//! Lotes em voo (F3): o `present.comp` escreve num anel de saídas (`Outs`, HOST_CACHED) e não espera a GPU. No modo
//! assíncrono, uma thread publicadora espera o valor de timeline do grupo (real + intermediários), copia as imagens e
//! publica com o hash calculado na conferência; até lá o endereço fica em `PUB_PENDING` (`publish_pending`), e
//! `published`/`published_group` esperam essa publicação. No modo síncrono a publicação é na hora, como antes.

use super::*;
use std::sync::{Arc, Condvar, Mutex};

static PRESENT_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/present_comp.spv"));
static FXAA_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/fxaa_comp.spv"));

/// O que a sombra ainda precisa receber para espelhar o alvo nativo.
#[derive(Clone, Copy)]
pub(super) enum ShadowSrc {
    /// decodificar a memória guest (criação da sombra / upload nativo)
    Memory,
    /// o alvo nativo foi limpo com este valor
    Clear(Clear),
}

pub(super) struct Shadow {
    pub img: Img,
    pub n: u32,
    pub src: Option<ShadowSrc>,
    /// geração de escrita: sobe a cada draw gravado na sombra e a cada recarga. A apresentação em dois passos confere
    /// que nada mudou a sombra entre `present_check` e `present_finish`.
    pub wgen: u64,
}

/// Alvo de re-renderização da interpolação (imagens N× próprias, fora da memória guest): `draw` grava nele (e só nele)
/// pelo caminho da sombra. As chaves são "falsas" (endereço fora do guest) para as passadas e o FXAA.
#[derive(Clone, Copy)]
pub(super) struct ReplayTarget {
    pub ck: SurfKey,
    pub dk: SurfKey,
    pub cview: vk::ImageView,
    pub dview: vk::ImageView,
    pub cimage: vk::Image,
    pub dimage: vk::Image,
    pub n: u32,
}

/// Imagem final de uma tela na resolução interna (RGBA8, linhas de cima para baixo).
pub struct HiRes {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
    /// hash do framebuffer nativo (memória guest) que a mesma transferência escreveu: se mudou, a imagem não vale mais
    pub hash: u64,
    /// tamanho da região do framebuffer nativo que o hash cobre
    pub bytes: u32,
}

/// Buffers `rgba` de imagens já descartadas (≈ 3,5 MB cada em 3×), reaproveitados por `present_image`: sem isso cada
/// imagem publicada (real + intermediários, até ~150 por segundo) alocava e soltava um buffer novo.
static RGBA_POOL: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());
// 6 = um grupo a 180 Hz (5 intermediários + o real); antes 12 (40 MB retidos a 3×, medido com YW_MEM_MAP)
const RGBA_POOL_MAX: usize = 6;

impl Drop for HiRes {
    fn drop(&mut self) {
        let v = std::mem::take(&mut self.rgba);
        if v.capacity() == 0 {
            return;
        }
        let mut p = RGBA_POOL.lock().unwrap_or_else(|e| e.into_inner());
        if p.len() >= RGBA_POOL_MAX {
            // o mais antigo sai (pode ser de outro tamanho, de antes de uma troca de escala)
            p.remove(0);
        }
        p.push(v);
    }
}

/// Cópia de `src` num buffer do `RGBA_POOL` (ou num novo, se nenhum couber). O conteúdo é exatamente `src`.
fn rgba_from(src: &[u8]) -> Vec<u8> {
    let reused = {
        let mut p = RGBA_POOL.lock().unwrap_or_else(|e| e.into_inner());
        p.iter().position(|b| b.capacity() >= src.len()).map(|i| p.swap_remove(i))
    };
    let mut v = reused.unwrap_or_else(|| Vec::with_capacity(src.len()));
    v.clear();
    v.extend_from_slice(src);
    v
}

static PUBLISHED: Mutex<Vec<(u32, Arc<HiRes>)>> = Mutex::new(Vec::new());

/// Framebuffers (endereço virtual) com imagem N× enviada à GPU e ainda não publicada (modo assíncrono; um por grupo em
/// voo, pode repetir). A thread publicadora tira e avisa em `PUB_CV`.
static PUB_PENDING: Mutex<Vec<u32>> = Mutex::new(Vec::new());
static PUB_CV: Condvar = Condvar::new();

/// Há imagem N× deste framebuffer a caminho (enviada, não publicada)? A troca de framebuffer da tela de cima espera
/// isto ficar falso, como espera o DisplayTransfer na fila da thread "gpu": trocar antes mostra o nativo na janela.
pub fn publish_pending(fb_addr: u32) -> bool {
    PUB_PENDING.lock().unwrap_or_else(|e| e.into_inner()).contains(&fb_addr)
}

/// Espera a publicação a caminho de `fb_addr`, se houver (no máximo 500 ms: uma GPU travada não prende quem lê).
fn wait_pending(fb_addr: u32) {
    let fim = std::time::Instant::now() + std::time::Duration::from_millis(500);
    let mut p = PUB_PENDING.lock().unwrap_or_else(|e| e.into_inner());
    while p.contains(&fb_addr) {
        let agora = std::time::Instant::now();
        if agora >= fim {
            break;
        }
        p = PUB_CV.wait_timeout(p, fim - agora).unwrap_or_else(|e| e.into_inner()).0;
    }
}

/// Imagem em alta resolução da tela cujo framebuffer nativo está em `fb_addr` (endereço virtual guest), se houver. Se a
/// imagem desse framebuffer ainda está na GPU (modo assíncrono), espera ela ser publicada.
pub fn published(fb_addr: u32) -> Option<Arc<HiRes>> {
    wait_pending(fb_addr);
    PUBLISHED.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(a, _)| *a == fb_addr).map(|(_, h)| h.clone())
}

fn publish(addr: u32, h: HiRes) {
    // sem grupo neste quadro (interpolação desligada, corte de cena…): o grupo anterior deste framebuffer não vale mais
    GROUPS.lock().unwrap_or_else(|e| e.into_inner()).retain(|(a, _)| *a != addr);
    let mut p = PUBLISHED.lock().unwrap_or_else(|e| e.into_inner());
    p.retain(|(a, _)| *a != addr);
    p.push((addr, Arc::new(h)));
    if p.len() > 8 {
        p.remove(0);
    }
}

/// Quadros intermediários + o quadro real de um DisplayTransfer (interpolação de quadros). A janela mostra, em ordem e
/// espaçados por `period_ms / steps` a partir de quando a troca de framebuffer os torna visíveis, `mids[0..]` e por fim
/// `real` (ver `Cadencia` em `hle/window.rs`).
pub struct Group {
    pub mids: Vec<Arc<HiRes>>,
    pub real: Arc<HiRes>,
    pub period_ms: f32,
    pub steps: u32,
    /// instante do DisplayTransfer (só para medir quanto a troca de framebuffer demora a mostrá-lo)
    pub t0: std::time::Instant,
}

static GROUPS: Mutex<Vec<(u32, Arc<Group>)>> = Mutex::new(Vec::new());

fn publish_group(addr: u32, mids: Vec<HiRes>, real: HiRes, period_ms: f32, steps: u32) {
    let real = Arc::new(real);
    let g = Group { mids: mids.into_iter().map(Arc::new).collect(), real: real.clone(), period_ms, steps, t0: std::time::Instant::now() };
    {
        let mut gs = GROUPS.lock().unwrap_or_else(|e| e.into_inner());
        gs.retain(|(a, _)| *a != addr);
        gs.push((addr, Arc::new(g)));
        if gs.len() > 8 {
            gs.remove(0);
        }
    }
    let mut p = PUBLISHED.lock().unwrap_or_else(|e| e.into_inner());
    p.retain(|(a, _)| *a != addr);
    p.push((addr, real));
    if p.len() > 8 {
        p.remove(0);
    }
}

/// Grupo (intermediários + real) da tela cujo framebuffer nativo está em `fb_addr`, se o último quadro foi interpolado
/// (esperando a publicação a caminho, como `published`).
pub fn published_group(fb_addr: u32) -> Option<Arc<Group>> {
    wait_pending(fb_addr);
    GROUPS.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(a, _)| *a == fb_addr).map(|(_, g)| g.clone())
}

pub(super) fn clear_published() {
    GROUPS.lock().unwrap_or_else(|e| e.into_inner()).clear();
    PUBLISHED.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// Vagas do anel de saídas do `present.comp` (um grupo usa no máximo 6: o real + 5 intermediários).
const OUT_SLOTS: usize = 8;

/// Anel de saídas do `present.comp` (HOST_CACHED, `HostMem::Readback`): cada imagem final (real ou intermediário) ocupa
/// uma vaga de `out_acquire` até alguém copiá-la (`out_take`, ou a thread publicadora). Os buffers são criados sob
/// demanda, todos do mesmo tamanho (`size`).
#[derive(Default)]
pub(super) struct Outs {
    bufs: Vec<HostBuf>,
    size: u64,
    state: Arc<OutState>,
}

/// Ocupação das vagas, compartilhada com a thread publicadora.
#[derive(Default)]
pub(super) struct OutState {
    busy: Mutex<Vec<bool>>,
    cv: Condvar,
}

impl OutState {
    fn release(&self, slot: usize) {
        let mut b = self.busy.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(x) = b.get_mut(slot) {
            *x = false;
        }
        drop(b);
        self.cv.notify_all();
    }
}

/// Uma imagem final numa vaga do anel de saídas: pronta quando a GPU passar de `value` (o envio que a grava).
pub(super) struct OutRef {
    slot: usize,
    /// ponteiro mapeado da vaga (vive enquanto a vaga estiver ocupada)
    ptr: usize,
    len: usize,
    w: u32,
    h: u32,
    /// hash do framebuffer nativo do quadro real e tamanho que ele cobre (vão para `HiRes`)
    pub hash: u64,
    bytes: u32,
    pub value: u64,
}

/// Cópia da imagem (a GPU já passou de `o.value`) e liberação da vaga.
fn take_out(o: &OutRef, st: &OutState) -> HiRes {
    // SAFETY: a vaga está ocupada (o buffer não é destruído) e a GPU terminou de escrevê-la; memória HOST_CACHED e
    // coerente, com a barreira COMPUTE → HOST no envio
    let rgba = rgba_from(unsafe { std::slice::from_raw_parts(o.ptr as *const u8, o.len) });
    st.release(o.slot);
    HiRes { w: o.w, h: o.h, rgba, hash: o.hash, bytes: o.bytes }
}

/// Publicação pedida: o quadro real (e os intermediários), prontos quando a GPU passar de `wait`.
struct PubJob {
    addr: u32,
    wait: u64,
    real: OutRef,
    mids: Option<Vec<OutRef>>,
    period: f32,
}

/// Copia as imagens do pedido, libera as vagas e publica (`publish`/`publish_group`). `ok` = a espera da GPU deu certo
/// (senão só libera).
fn finish_job(j: PubJob, st: &OutState, ok: bool) {
    let real = take_out(&j.real, st);
    let mids: Option<Vec<HiRes>> = j.mids.map(|m| m.iter().map(|o| take_out(o, st)).collect());
    if ok {
        match mids {
            Some(m) => {
                let steps = m.len() as u32 + 1;
                publish_group(j.addr, m, real, j.period, steps);
            }
            None => publish(j.addr, real),
        }
    }
}

/// Thread publicadora (modo assíncrono): espera o semáforo de timeline de cada pedido, em ordem, e publica.
pub(super) struct Publisher {
    tx: std::sync::mpsc::Sender<PubJob>,
    /// pedidos enviados e ainda não publicados
    left: Arc<(Mutex<usize>, Condvar)>,
}

impl Publisher {
    fn start(device: ash::Device, timeline: vk::Semaphore, st: Arc<OutState>) -> Publisher {
        let (tx, rx) = std::sync::mpsc::channel::<PubJob>();
        let left = Arc::new((Mutex::new(0usize), Condvar::new()));
        let l2 = left.clone();
        std::thread::Builder::new()
            .name("vk-publica".into())
            .spawn(move || {
                for j in rx {
                    let sems = [timeline];
                    let vals = [j.wait];
                    // em fatias de 1 s (só um erro do dispositivo sai daqui sem a GPU chegar no valor)
                    let ok = loop {
                        // SAFETY: semáforo e dispositivo vivem até o fim do processo; esperar não pede sincronização
                        match unsafe { device.wait_semaphores(&vk::SemaphoreWaitInfo::default().semaphores(&sems).values(&vals), 1_000_000_000) } {
                            Ok(()) => break true,
                            Err(vk::Result::TIMEOUT) => continue,
                            Err(e) => {
                                eprintln!("[vulkan] publicação: vkWaitSemaphores {e:?}");
                                break false;
                            }
                        }
                    };
                    let addr = j.addr;
                    finish_job(j, &st, ok);
                    {
                        let mut p = PUB_PENDING.lock().unwrap_or_else(|e| e.into_inner());
                        if let Some(i) = p.iter().position(|&a| a == addr) {
                            p.remove(i);
                        }
                    }
                    PUB_CV.notify_all();
                    let (m, cv) = &*l2;
                    *m.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
                    cv.notify_all();
                }
            })
            .expect("thread vk-publica");
        Publisher { tx, left }
    }
}

/// Recursos de computação + estado da resolução interna.
#[derive(Default)]
pub(super) struct Hr {
    /// imagens da re-renderização da interpolação: (largura, altura, cor, profundidade)
    pub rimg: Option<(u32, u32, Img, Img)>,
    /// configuração aplicada (ligada, fator, FXAA): mudou → `hires_reset`
    pub on: bool,
    pub scale: u32,
    pub fxaa: bool,
    pub ready: bool,
    lin: vk::Sampler,
    near: vk::Sampler,
    /// layouts do present/FXAA: push descriptor (sem pool)
    p_dsl: vk::DescriptorSetLayout,
    p_layout: vk::PipelineLayout,
    p_pipe: vk::Pipeline,
    a_dsl: vk::DescriptorSetLayout,
    a_layout: vk::PipelineLayout,
    a_pipe: vk::Pipeline,
    /// cópia da sombra para o FXAA ler enquanto escreve na própria sombra
    tmp: Option<(u32, u32, Img)>,
    /// anel de saídas do present e a thread publicadora (criada no 1º pedido do modo assíncrono)
    outs: Outs,
    publ: Option<Publisher>,
    /// por alvo de cor: (draws de cena desde o último FXAA, FXAA já feito nesta cena)
    pub aa: HashMap<SurfKey, (u32, bool)>,
}

/// Pedido de apresentação (um DisplayTransfer do alvo para o framebuffer do LCD).
#[derive(Clone, Copy, Debug)]
pub struct PresentReq {
    pub inp: u32,
    pub iw: u32,
    pub ih: u32,
    pub sx: u32,
    pub sy: u32,
    pub flip: bool,
    /// colunas e linhas da saída nativa (240 × 400 na tela de cima)
    pub ow: u32,
    pub hh: u32,
    /// framebuffer nativo escrito e quantos bytes (para o hash)
    pub out: u32,
    pub out_bytes: u32,
}

/// Alvo de tela com sombra: cor RGBA8/RGB8 480×400 ou 240×400 (a tela de cima; o alvo é guardado "deitado").
pub(super) fn shadow_eligible(ck: SurfKey) -> bool {
    ck.addr != 0 && !ck.depth && ck.h == 400 && (ck.w == 480 || ck.w == 240) && matches!(ck.fmt, 0 | 1)
}

/// Replica cada pixel (de `bpp` bytes) N×N: `src` w×h → `dst` (w·n)×(h·n). Só ESCREVE em `dst` (o anel de upload pode
/// ser memória sem cache, onde ler é caro): a linha replicada é montada num buffer local e escrita N vezes.
fn expand(src: &[u8], w: usize, h: usize, n: usize, bpp: usize, dst: &mut [u8]) {
    let row = w * n * bpp;
    let mut line = vec![0u8; row];
    for y in 0..h {
        for x in 0..w {
            let p = &src[(y * w + x) * bpp..(y * w + x) * bpp + bpp];
            for k in 0..n {
                let o = (x * n + k) * bpp;
                line[o..o + bpp].copy_from_slice(p);
            }
        }
        for k in 0..n {
            let o = (y * n + k) * row;
            dst[o..o + row].copy_from_slice(&line);
        }
    }
}

/// Regiões de cópia buffer → sombra (como `copy_regions`, em N×).
fn copy_regions_n(k: SurfKey, off: u64, n: u32) -> Vec<vk::BufferImageCopy> {
    let (w, h) = (k.w * n, k.h * n);
    let ext = vk::Extent3D { width: w, height: h, depth: 1 };
    let reg = |o: u64, a: vk::ImageAspectFlags| vk::BufferImageCopy {
        buffer_offset: o,
        buffer_row_length: 0,
        buffer_image_height: 0,
        image_subresource: layers(a),
        image_offset: vk::Offset3D::default(),
        image_extent: ext,
    };
    if k.depth {
        vec![reg(off, vk::ImageAspectFlags::DEPTH), reg(off + w as u64 * h as u64 * 4, vk::ImageAspectFlags::STENCIL)]
    } else {
        vec![reg(off, vk::ImageAspectFlags::COLOR)]
    }
}

impl Vk {
    /// Pipelines de computação e samplers (uma vez).
    pub(super) unsafe fn hr_init(&mut self) -> Result<(), String> {
        if self.hr.ready {
            return Ok(());
        }
        let d = self.device.clone();
        let sam = |f: vk::Filter| {
            d.create_sampler(
                &vk::SamplerCreateInfo::default()
                    .mag_filter(f)
                    .min_filter(f)
                    .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE),
                None,
            )
        };
        self.hr.near = vkr!(sam(vk::Filter::NEAREST), "vkCreateSampler(hires)");
        self.hr.lin = vkr!(sam(vk::Filter::LINEAR), "vkCreateSampler(hires lin)");
        let cs = vk::ShaderStageFlags::COMPUTE;
        let bind = |b: u32, t: vk::DescriptorType| {
            vk::DescriptorSetLayoutBinding::default().binding(b).descriptor_type(t).descriptor_count(1).stage_flags(cs)
        };
        let mk_layout = |b1: vk::DescriptorType, push: u32| -> Result<(vk::DescriptorSetLayout, vk::PipelineLayout), String> {
            let binds = [bind(0, vk::DescriptorType::COMBINED_IMAGE_SAMPLER), bind(1, b1)];
            let dsl = vkr!(
                d.create_descriptor_set_layout(
                    &vk::DescriptorSetLayoutCreateInfo::default().flags(vk::DescriptorSetLayoutCreateFlags::PUSH_DESCRIPTOR_KHR).bindings(&binds),
                    None
                ),
                "vkCreateDescriptorSetLayout(hires)"
            );
            let dsls = [dsl];
            let pcr = [vk::PushConstantRange { stage_flags: cs, offset: 0, size: push }];
            let l = vkr!(
                d.create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(&dsls).push_constant_ranges(&pcr), None),
                "vkCreatePipelineLayout(hires)"
            );
            Ok((dsl, l))
        };
        let (p_dsl, p_layout) = mk_layout(vk::DescriptorType::STORAGE_BUFFER, 28)?;
        let (a_dsl, a_layout) = mk_layout(vk::DescriptorType::STORAGE_IMAGE, 8)?;
        let pipe = |spv: &[u8], layout: vk::PipelineLayout| -> Result<vk::Pipeline, String> {
            let words = ash::util::read_spv(&mut std::io::Cursor::new(spv)).map_err(|e| e.to_string())?;
            let m = vkr!(d.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&words), None), "vkCreateShaderModule(comp)");
            let ci = [vk::ComputePipelineCreateInfo::default()
                .stage(vk::PipelineShaderStageCreateInfo::default().stage(cs).module(m).name(c"main"))
                .layout(layout)];
            let p = d
                .create_compute_pipelines(self.pcache, &ci, None)
                .map_err(|(_, e)| format!("vkCreateComputePipelines: {e:?}"))?[0];
            d.destroy_shader_module(m, None);
            Ok(p)
        };
        self.hr.p_dsl = p_dsl;
        self.hr.p_layout = p_layout;
        self.hr.p_pipe = pipe(PRESENT_SPV, p_layout)?;
        self.hr.a_dsl = a_dsl;
        self.hr.a_layout = a_layout;
        self.hr.a_pipe = pipe(FXAA_SPV, a_layout)?;
        self.hr.ready = true;
        Ok(())
    }

    /// Vista da sombra de `k` (cria se preciso e põe nela o que o alvo nativo já recebeu).
    pub(super) unsafe fn shadow(&mut self, k: SurfKey) -> Result<vk::ImageView, String> {
        let n = self.hr.scale;
        if !self.surfaces.get(&k).is_some_and(|s| s.shadow.is_some()) {
            let (fmt, usage, aspect) = if k.depth {
                (
                    DEPTH_FMT,
                    vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::TRANSFER_DST,
                    vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL,
                )
            } else {
                (
                    COLOR_FMT,
                    vk::ImageUsageFlags::COLOR_ATTACHMENT
                        | vk::ImageUsageFlags::SAMPLED
                        | vk::ImageUsageFlags::STORAGE
                        | vk::ImageUsageFlags::TRANSFER_SRC
                        | vk::ImageUsageFlags::TRANSFER_DST,
                    vk::ImageAspectFlags::COLOR,
                )
            };
            let img = self.image(k.w * n, k.h * n, fmt, usage, aspect)?;
            self.init_layout(img.image, aspect);
            self.surfaces.get_mut(&k).ok_or("sombra: superfície sumiu")?.shadow = Some(Shadow { img, n, src: Some(ShadowSrc::Memory), wgen: 0 });
        }
        let s = self.surfaces.get_mut(&k).ok_or("sombra: superfície sumiu")?;
        let sh = s.shadow.as_mut().unwrap();
        let (view, src) = (sh.img.view, sh.src.take());
        if let Some(src) = src {
            // recarga da memória (ex.: sombra descartada no lote do olho escondido): o conteúdo mais novo pode estar
            // só no `readback` do nativo — volta para a memória antes
            if matches!(src, ShadowSrc::Memory) && self.surfaces.get(&k).is_some_and(|s| s.pending) {
                self.materialize(k);
            }
            self.shadow_init(k, src)?;
        }
        Ok(view)
    }

    /// Põe na sombra o conteúdo que o alvo nativo recebeu (clear ou memória decodificada e replicada N×N).
    unsafe fn shadow_init(&mut self, k: SurfKey, src: ShadowSrc) -> Result<(), String> {
        let (image, n) = {
            let sh = self.surfaces.get_mut(&k).unwrap().shadow.as_mut().unwrap();
            sh.wgen += 1;
            (sh.img.image, sh.n)
        };
        let clear = |me: &Vk, c: Clear| match c {
            Clear::Color(rgba) => me.device.cmd_clear_color_image(
                me.cmd_pre,
                image,
                vk::ImageLayout::GENERAL,
                &vk::ClearColorValue { float32: rgba },
                &[range(vk::ImageAspectFlags::COLOR)],
            ),
            Clear::Depth(z, st) => me.device.cmd_clear_depth_stencil_image(
                me.cmd_pre,
                image,
                vk::ImageLayout::GENERAL,
                &vk::ClearDepthStencilValue { depth: z, stencil: st },
                &[range(vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL)],
            ),
        };
        match src {
            ShadowSrc::Clear(c) => clear(self, c),
            ShadowSrc::Memory => {
                let mem = if k.addr == 0 { None } else { guest_slice(k.addr, k.bytes()) };
                let Some(mem) = mem else {
                    // sem memória (alvo "sem profundidade"/endereço 0): conteúdo definido, nada a copiar
                    clear(self, if k.depth { Clear::Depth(1.0, 0) } else { Clear::Color([0.0; 4]) });
                    return Ok(());
                };
                let (w, h) = (k.w as usize, k.h as usize);
                let npx = w * h;
                let planes = if k.depth { 5 } else { 4 };
                let mut small = vec![0u8; npx * planes];
                decode_surface(k, mem, &mut small);
                let nn = n as usize;
                let big = npx * nn * nn;
                let bytes = (big * planes) as u64;
                let off = self.ring_alloc(bytes).ok_or("anel de upload cheio (sombra)")?;
                let dst = std::slice::from_raw_parts_mut(self.ring.ptr.add(off as usize), bytes as usize);
                expand(&small[..4 * npx], w, h, nn, 4, &mut dst[..4 * big]);
                if k.depth {
                    expand(&small[4 * npx..], w, h, nn, 1, &mut dst[4 * big..]);
                }
                self.device.cmd_copy_buffer_to_image(self.cmd_pre, self.ring.buf, image, vk::ImageLayout::GENERAL, &copy_regions_n(k, off, n));
            }
        }
        Ok(())
    }

    /// Configuração (sombra ligada, N, FXAA) mudou? Então executa o lote, devolve tudo à memória e descarta as sombras.
    pub(super) unsafe fn hires_sync_config(&mut self) -> Result<(), String> {
        let (on, scale, fxaa) = (super::super::hires_on(), super::super::scale(), super::super::fxaa());
        if (on, scale, fxaa) == (self.hr.on, self.hr.scale, self.hr.fxaa) {
            return Ok(());
        }
        self.flush()?;
        let keys: Vec<SurfKey> = self.surfaces.keys().copied().collect();
        for k in keys {
            self.materialize(k);
        }
        let had = self.hr.on;
        self.hires_drop_all();
        self.hr.on = on;
        self.hr.scale = scale;
        self.hr.fxaa = fxaa;
        if on {
            self.hr_init()?;
            if !had || scale > 1 {
                eprintln!("[vulkan] resolução interna {scale}× (sombra dos alvos de tela){}", if fxaa { " + FXAA de cena" } else { "" });
            }
        }
        Ok(())
    }

    /// Libera sombras, cópia do FXAA e as imagens publicadas; força recarga das superfícies. As imagens só somem
    /// quando a GPU terminar os envios em voo (`defer`); a publicação em andamento termina antes de limpar.
    unsafe fn hires_drop_all(&mut self) {
        let shadows: Vec<Img> = self.surfaces.values_mut().filter_map(|s| s.shadow.take().map(|sh| sh.img)).collect();
        for s in self.surfaces.values_mut() {
            s.snapshot = None;
            s.synced = false;
        }
        for img in shadows {
            self.defer(Res::Img(img));
        }
        if let Some((_, _, img)) = self.hr.tmp.take() {
            self.defer(Res::Img(img));
        }
        if let Some((_, _, c, d)) = self.hr.rimg.take() {
            self.defer(Res::Img(c));
            self.defer(Res::Img(d));
        }
        self.ip.reset();
        self.tex_pinned.clear();
        self.hr.aa.clear();
        self.pub_idle();
        clear_published();
    }

    /// Chamado antes de gravar um draw na sombra: se a cena 3D acabou (draw de interface depois de draws de cena) e o
    /// FXAA está ligado, encerra a passada da sombra e roda o FXAA sobre ela.
    pub(super) unsafe fn aa_boundary(&mut self, ck: SurfKey, is_scene: bool, writes_color: bool) -> Result<(), String> {
        if !self.hr.fxaa {
            return Ok(());
        }
        let e = self.hr.aa.entry(ck).or_insert((0, false));
        if is_scene {
            *e = if e.1 { (1, false) } else { (e.0 + 1, false) };
            return Ok(());
        }
        if !writes_color || e.0 == 0 || e.1 {
            return Ok(());
        }
        e.1 = true;
        let cs = self.cmd_shadow;
        self.end_pass_s();
        self.full_barrier(cs);
        self.record_fxaa(cs, ck)
    }

    /// Imagem, vista e fator da sombra de `ck` (ou do alvo de re-renderização, se `ck` é a chave dele).
    pub(super) fn shadow_img(&self, ck: SurfKey) -> Option<(vk::Image, vk::ImageView, u32)> {
        if let Some(r) = &self.replay {
            if r.ck == ck {
                return Some((r.cimage, r.cview, r.n));
            }
        }
        self.surfaces.get(&ck).and_then(|s| s.shadow.as_ref()).map(|sh| (sh.img.image, sh.img.view, sh.n))
    }

    /// FXAA sobre a sombra de `ck` (cópia → compute → de volta). `cb` precisa estar gravando, fora de passada.
    unsafe fn record_fxaa(&mut self, cb: vk::CommandBuffer, ck: SurfKey) -> Result<(), String> {
        let (image, view, n) = self.shadow_img(ck).ok_or("FXAA: sem sombra")?;
        let (w, h) = (ck.w * n, ck.h * n);
        if self.hr.tmp.as_ref().is_none_or(|t| (t.0, t.1) != (w, h)) {
            if let Some((_, _, old)) = self.hr.tmp.take() {
                // pode ainda estar em uso por comandos já gravados neste lote ou em voo
                self.defer(Res::Img(old));
            }
            let img = self.image(w, h, COLOR_FMT, vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST, vk::ImageAspectFlags::COLOR)?;
            // no próprio `cb` (fora de passada nos dois chamadores): pelo `present_image` o lote já foi executado e o
            // `cmd_pre` não está gravando — a transição gravada nele se perdia (achado da revisão da F2)
            self.init_layout_on(cb, img.image, vk::ImageAspectFlags::COLOR);
            self.hr.tmp = Some((w, h, img));
        }
        let tmp = self.hr.tmp.as_ref().unwrap();
        let (timg, tview) = (tmp.2.image, tmp.2.view);
        let d = self.device.clone();
        d.cmd_copy_image(
            cb,
            image,
            vk::ImageLayout::GENERAL,
            timg,
            vk::ImageLayout::GENERAL,
            &[vk::ImageCopy {
                src_subresource: layers(vk::ImageAspectFlags::COLOR),
                src_offset: vk::Offset3D::default(),
                dst_subresource: layers(vk::ImageAspectFlags::COLOR),
                dst_offset: vk::Offset3D::default(),
                extent: vk::Extent3D { width: w, height: h, depth: 1 },
            }],
        );
        self.full_barrier(cb);
        let si = [vk::DescriptorImageInfo { sampler: self.hr.lin, image_view: tview, image_layout: vk::ImageLayout::GENERAL }];
        let di = [vk::DescriptorImageInfo { sampler: vk::Sampler::null(), image_view: view, image_layout: vk::ImageLayout::GENERAL }];
        d.cmd_bind_pipeline(cb, vk::PipelineBindPoint::COMPUTE, self.hr.a_pipe);
        self.pd.cmd_push_descriptor_set(
            cb,
            vk::PipelineBindPoint::COMPUTE,
            self.hr.a_layout,
            0,
            &[
                vk::WriteDescriptorSet::default().dst_binding(0).descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER).image_info(&si),
                vk::WriteDescriptorSet::default().dst_binding(1).descriptor_type(vk::DescriptorType::STORAGE_IMAGE).image_info(&di),
            ],
        );
        let pc: [u32; 2] = [w, h];
        d.cmd_push_constants(cb, self.hr.a_layout, vk::ShaderStageFlags::COMPUTE, 0, std::slice::from_raw_parts(pc.as_ptr() as *const u8, 8));
        d.cmd_dispatch(cb, w.div_ceil(8), h.div_ceil(8), 1);
        self.full_barrier(cb);
        Ok(())
    }

    /// 1º passo do DisplayTransfer do alvo `req.inp` para o framebuffer do LCD (ver `present_hires_check`): roda logo
    /// depois da transferência nativa, antes de o jogo poder reescrever a memória. Fecha o lote, confere que a sombra
    /// espelha o conteúdo nativo que a transferência leu, calcula o hash do framebuffer de saída e leva os desenhos
    /// capturados do quadro. `None` = nada a apresentar (a janela usa o nativo).
    pub(super) unsafe fn present_check(&mut self, req: PresentReq) -> Result<Option<PresentChecked>, String> {
        let dbg = std::env::var_os("YW_HIRES_DEBUG").is_some();
        if !self.hr.on || !self.hr.ready {
            if dbg {
                eprintln!("[hires] present: desligado (on {} ready {})", self.hr.on, self.hr.ready);
            }
            return Ok(None);
        }
        // olho direito do 3D estéreo: a janela não o mostra, então nada de FXAA, leitura nem interpolação (era uma
        // submissão + espera + leitura da imagem inteira a mais por quadro). Os desenhos capturados dele são descartados.
        if crate::hle::gpu::top_hidden(req.out) || crate::hle::gpu::not_shown(req.out) {
            self.interp_discard();
            return Ok(None);
        }
        // alvo de cor com sombra neste endereço/tamanho
        let Some(k) = self
            .surfaces
            .iter()
            .find(|(k, s)| !k.depth && k.addr == req.inp && k.w == req.iw && k.h == req.ih && s.shadow.is_some())
            .map(|(k, _)| *k)
        else {
            if dbg {
                eprintln!("[hires] present: sem alvo com sombra em {:#x} {}x{}", req.inp, req.iw, req.ih);
            }
            return Ok(None);
        };
        if self.recording {
            self.flush()?;
        }
        // a sombra só vale se espelha o conteúdo nativo que a transferência acabou de ler
        let wgen = {
            let s = &self.surfaces[&k];
            let mem = guest_slice(k.addr, k.bytes());
            let cur = s.shadow.as_ref().is_some_and(|sh| sh.src.is_none())
                && s.clear.is_none()
                && (s.pending || mem.is_some_and(|m| snap_eq(s.snapshot, m)));
            if !cur {
                if dbg {
                    eprintln!("[hires] present: sombra não espelha o alvo");
                }
                return Ok(None);
            }
            s.shadow.as_ref().map_or(0, |sh| sh.wgen)
        };
        // o framebuffer nativo que a transferência escreveu (a janela confere a imagem publicada contra ele)
        let hash = guest_slice(req.out, req.out_bytes).map_or(0, hash_bytes);
        let cap = self.interp_take();
        Ok(Some(PresentChecked { req, key: k, wgen, hash, cap }))
    }

    /// 2º passo (ver `present_hires_finish`): imagem final N×, intermediários da interpolação e publicação. Não relê a
    /// memória do jogo: o hash vem do 1º passo. Se a sombra mudou desde a conferência (draws do quadro seguinte nela,
    /// recarga, troca de configuração), não publica nada — a janela usa o nativo. No modo assíncrono só grava e envia:
    /// a publicação acontece quando a GPU terminar (`publish_out`).
    pub(super) unsafe fn present_finish(&mut self, c: PresentChecked) -> Result<(), String> {
        let PresentChecked { req, key: k, wgen, hash, cap } = c;
        let intacta = self.surfaces.get(&k).and_then(|s| s.shadow.as_ref()).is_some_and(|sh| sh.wgen == wgen && sh.src.is_none());
        if !intacta {
            if std::env::var_os("YW_HIRES_DEBUG").is_some() {
                eprintln!("[hires] present: a sombra mudou entre a conferência e a apresentação");
            }
            return Ok(());
        }
        // lote aberto depois da conferência (quadro seguinte, sem tocar esta sombra): executa antes — `present_image`
        // usa um envio próprio e o lote aberto tem de ir antes dele
        if self.recording {
            self.flush()?;
        }
        let real = self.present_image(req, k, hash)?;
        // interpolação de quadros: monta os quadros intermediários entre o anterior e este (re-renderizados)
        let mids = match self.interp_frame_done(req, &real, cap) {
            Ok(m) => m,
            Err(e) => {
                self.warn(&format!("interpolação: {e}"));
                None
            }
        };
        self.publish_out(req.out, real, mids)
    }

    /// Publica o quadro real (e os intermediários) de `addr` quando a GPU os terminar: no modo assíncrono pela thread
    /// publicadora (o endereço fica em `PUB_PENDING` até lá); no síncrono, agora.
    pub(super) unsafe fn publish_out(&mut self, addr: u32, real: OutRef, mids: Option<Vec<OutRef>>) -> Result<(), String> {
        let wait = mids.iter().flatten().map(|o| o.value).fold(real.value, u64::max);
        let job = PubJob { addr, wait, real, mids, period: self.ip.nominal_ms() };
        if !self.async_mode {
            let r = self.wait_value(wait);
            finish_job(job, &self.hr.outs.state, r.is_ok());
            return r;
        }
        let (dev, tl, st) = (self.device.clone(), self.timeline, self.hr.outs.state.clone());
        let p = self.hr.publ.get_or_insert_with(|| Publisher::start(dev, tl, st));
        PUB_PENDING.lock().unwrap_or_else(|e| e.into_inner()).push(addr);
        *p.left.0.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        if let Err(std::sync::mpsc::SendError(job)) = p.tx.send(job) {
            // a thread publicadora morreu: publica aqui
            *p.left.0.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
            {
                let mut pp = PUB_PENDING.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(i) = pp.iter().position(|&a| a == addr) {
                    pp.remove(i);
                }
            }
            PUB_CV.notify_all();
            self.hr.publ = None;
            let r = self.wait_value(wait);
            finish_job(job, &self.hr.outs.state, r.is_ok());
            return r;
        }
        Ok(())
    }

    /// Espera a thread publicadora publicar tudo o que recebeu.
    pub(super) fn pub_idle(&self) {
        if let Some(p) = &self.hr.publ {
            let (m, cv) = &*p.left;
            let mut n = m.lock().unwrap_or_else(|e| e.into_inner());
            while *n > 0 {
                n = cv.wait(n).unwrap_or_else(|e| e.into_inner());
            }
        }
    }

    /// Vaga livre no anel de saídas para uma imagem de `bytes` (cria o buffer sob demanda; espera a publicadora soltar
    /// uma se as `OUT_SLOTS` estão ocupadas). Imagem maior que as vagas (outra escala): espera todas ficarem livres e
    /// refaz o anel. Devolve (vaga, ponteiro mapeado).
    unsafe fn out_acquire(&mut self, bytes: u64) -> Result<(usize, usize), String> {
        let st = self.hr.outs.state.clone();
        if self.hr.outs.size < bytes {
            if !self.hr.outs.bufs.is_empty() {
                // vaga livre = a GPU já terminou de escrevê-la e ninguém mais a lê (são soltas só depois da espera)
                let mut b = st.busy.lock().unwrap_or_else(|e| e.into_inner());
                while b.iter().any(|&x| x) {
                    b = st.cv.wait(b).unwrap_or_else(|e| e.into_inner());
                }
                b.clear();
                drop(b);
                for old in std::mem::take(&mut self.hr.outs.bufs) {
                    self.free_buf(&old);
                }
            }
            self.hr.outs.size = bytes;
        }
        loop {
            let mut b = st.busy.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(i) = b.iter().position(|&x| !x) {
                b[i] = true;
                return Ok((i, self.hr.outs.bufs[i].ptr as usize));
            }
            if b.len() < OUT_SLOTS {
                let i = b.len();
                b.push(true);
                drop(b);
                match self.host_buf(self.hr.outs.size, vk::BufferUsageFlags::STORAGE_BUFFER, HostMem::Readback) {
                    Ok(buf) => {
                        let ptr = buf.ptr as usize;
                        self.hr.outs.bufs.push(buf);
                        return Ok((i, ptr));
                    }
                    Err(e) => {
                        st.busy.lock().unwrap_or_else(|e| e.into_inner()).pop();
                        return Err(e);
                    }
                }
            }
            // todas ocupadas por pedidos já enviados: a publicadora solta uma quando a GPU terminar
            let (b2, t) = st.cv.wait_timeout(b, std::time::Duration::from_secs(2)).unwrap_or_else(|e| e.into_inner());
            if t.timed_out() && b2.iter().all(|&x| x) {
                return Err("anel de saídas ocupado há 2 s".into());
            }
        }
    }

    /// Solta a vaga de uma saída que não vai ser publicada (erro no meio do caminho). A GPU tem de ter terminado o envio
    /// dela (ou ele nunca ter sido feito).
    pub(super) fn out_release(&self, o: &OutRef) {
        self.hr.outs.state.release(o.slot);
    }

    /// Imagem de uma saída (espera a GPU) e solta a vaga: para quem precisa dos pixels na hora (validação, teste).
    pub(super) unsafe fn out_take(&mut self, o: &OutRef) -> Result<HiRes, String> {
        if let Err(e) = self.wait_value(o.value) {
            self.out_release(o);
            return Err(e);
        }
        Ok(take_out(o, &self.hr.outs.state))
    }

    /// Cópia dos pixels de uma saída (espera a GPU) sem soltar a vaga (ela ainda vai ser publicada).
    pub(super) unsafe fn out_pixels(&mut self, o: &OutRef) -> Result<Vec<u8>, String> {
        self.wait_value(o.value)?;
        Ok(std::slice::from_raw_parts(o.ptr as *const u8, o.len).to_vec())
    }

    /// Grava em `cb` (gravando, fora de passada) o FXAA pendente e o `present.comp` da sombra / do alvo de
    /// re-renderização `aa_key` para uma vaga do anel de saídas. A saída fica pronta com o próximo envio
    /// (`use_value`: o de `cb`). Nada de mexer na memória guest (nem ler: `hash` é o do framebuffer nativo, calculado em
    /// `present_check`).
    pub(super) unsafe fn present_record(&mut self, cb: vk::CommandBuffer, req: PresentReq, aa_key: SurfKey, hash: u64) -> Result<OutRef, String> {
        let n = self.hr.scale;
        let (fw, fh) = (req.hh * n, req.ow * n);
        let bytes = fw as u64 * fh as u64 * 4;
        let (slot, ptr) = self.out_acquire(bytes)?;
        let o = OutRef { slot, ptr, len: bytes as usize, w: fw, h: fh, hash, bytes: req.out_bytes, value: self.use_value() };
        if let Err(e) = self.present_record_in(cb, req, aa_key, slot, bytes) {
            self.out_release(&o);
            return Err(e);
        }
        Ok(o)
    }

    unsafe fn present_record_in(&mut self, cb: vk::CommandBuffer, req: PresentReq, aa_key: SurfKey, slot: usize, bytes: u64) -> Result<(), String> {
        let n = self.hr.scale;
        let (fw, fh) = (req.hh * n, req.ow * n);
        let out_buf = self.hr.outs.bufs[slot].buf;
        let d = self.device.clone();
        // FXAA pendente (cena sem interface depois)
        if self.hr.fxaa && self.hr.aa.get(&aa_key).is_some_and(|e| e.0 > 0 && !e.1) {
            self.hr.aa.get_mut(&aa_key).unwrap().1 = true;
            self.record_fxaa(cb, aa_key)?;
        }
        let (_, view, _) = self.shadow_img(aa_key).ok_or("present: sem imagem")?;
        let si = [vk::DescriptorImageInfo { sampler: self.hr.near, image_view: view, image_layout: vk::ImageLayout::GENERAL }];
        let bi = [vk::DescriptorBufferInfo { buffer: out_buf, offset: 0, range: bytes }];
        d.cmd_bind_pipeline(cb, vk::PipelineBindPoint::COMPUTE, self.hr.p_pipe);
        self.pd.cmd_push_descriptor_set(
            cb,
            vk::PipelineBindPoint::COMPUTE,
            self.hr.p_layout,
            0,
            &[
                vk::WriteDescriptorSet::default().dst_binding(0).descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER).image_info(&si),
                vk::WriteDescriptorSet::default().dst_binding(1).descriptor_type(vk::DescriptorType::STORAGE_BUFFER).buffer_info(&bi),
            ],
        );
        let pc: [u32; 7] = [n, req.sx, req.sy, req.flip as u32, req.ih, req.ow, req.hh];
        d.cmd_push_constants(cb, self.hr.p_layout, vk::ShaderStageFlags::COMPUTE, 0, std::slice::from_raw_parts(pc.as_ptr() as *const u8, 28));
        self.full_barrier(cb);
        d.cmd_dispatch(cb, fw.div_ceil(8), fh.div_ceil(8), 1);
        self.mem_barrier(
            cb,
            (vk::PipelineStageFlags2::COMPUTE_SHADER, vk::AccessFlags2::SHADER_WRITE),
            (vk::PipelineStageFlags2::HOST, vk::AccessFlags2::HOST_READ),
        );
        Ok(())
    }

    /// Imagem final (RGBA8, 400N × 240N) da sombra / do alvo de re-renderização `aa_key` num envio próprio (vaga do
    /// anel de envios, buffer `hr`): FXAA pendente + `present.comp` para o anel de saídas. Não espera a GPU no modo
    /// assíncrono. O lote atual tem de estar fechado.
    pub(super) unsafe fn present_image(&mut self, req: PresentReq, aa_key: SurfKey, hash: u64) -> Result<OutRef, String> {
        self.mode_check()?;
        let s = self.take_slot()?;
        let cb = self.slots[s].hr;
        let base = s as u32 * QS;
        let d = self.device.clone();
        vkr!(d.reset_command_buffer(cb, vk::CommandBufferResetFlags::empty()), "vkResetCommandBuffer(hires)");
        vkr!(
            d.begin_command_buffer(cb, &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)),
            "vkBeginCommandBuffer(hires)"
        );
        // barreira de entrada: o FXAA/present lê a sombra escrita pelo lote anterior (achado da revisão da F2)
        self.full_barrier(cb);
        d.cmd_reset_query_pool(cb, self.ts_pool, base, QS);
        d.cmd_write_timestamp(cb, vk::PipelineStageFlags::TOP_OF_PIPE, self.ts_pool, base + TS_PRESENT);
        let o = match self.present_record(cb, req, aa_key, hash) {
            Ok(o) => o,
            Err(e) => {
                let _ = d.end_command_buffer(cb);
                return Err(e);
            }
        };
        d.cmd_write_timestamp(cb, vk::PipelineStageFlags::BOTTOM_OF_PIPE, self.ts_pool, base + TS_PRESENT + 1);
        let r = match d.end_command_buffer(cb) {
            Ok(()) => self.submit(&[cb], s, SubKind::Present, "hires"),
            Err(e) => Err(format!("vkEndCommandBuffer(hires): {e:?}")),
        };
        match r {
            Ok(v) => {
                debug_assert_eq!(v, o.value);
                if !self.async_mode {
                    self.wait_value(v)?;
                }
                Ok(o)
            }
            Err(e) => {
                self.out_release(&o);
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    /// `expand` (só escreve no destino) = a replicação N×N ingênua.
    #[test]
    fn expand_igual_a_replicacao() {
        for (w, h) in [(5usize, 3usize), (8, 8), (1, 7)] {
            for bpp in [1usize, 4] {
                let src: Vec<u8> = (0..w * h * bpp).map(|i| (i * 37 + 11) as u8).collect();
                for n in 1..=4usize {
                    let mut got = vec![0xEEu8; w * h * n * n * bpp];
                    expand(&src, w, h, n, bpp, &mut got);
                    let (bw, bh) = (w * n, h * n);
                    let mut want = vec![0u8; bw * bh * bpp];
                    for y in 0..bh {
                        for x in 0..bw {
                            let s0 = ((y / n) * w + x / n) * bpp;
                            let d0 = (y * bw + x) * bpp;
                            want[d0..d0 + bpp].copy_from_slice(&src[s0..s0 + bpp]);
                        }
                    }
                    assert_eq!(got, want, "{w}x{h} bpp {bpp} n {n}");
                }
            }
        }
    }

    /// O pool de `HiRes::rgba` devolve buffers com exatamente o conteúdo pedido (reaproveitado de outro tamanho).
    #[test]
    fn pool_rgba_conteudo_exato() {
        drop(HiRes { w: 1, h: 1, rgba: vec![7u8; 4096], hash: 0, bytes: 0 });
        let a: Vec<u8> = (0..1000u32).map(|i| i as u8).collect();
        assert_eq!(rgba_from(&a), a);
        let b = vec![3u8; 9000];
        assert_eq!(rgba_from(&b), b);
    }
}
