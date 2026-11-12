//! Backend Vulkan do PICA (o principal). Fora da tela: não há swapchain — o resultado volta para a memória guest e
//! o GSP (DisplayTransfer etc.) e a janela continuam iguais.
//!
//! Desenho (simples e correto primeiro):
//! - geometria de dois jeitos:
//!   - **shader de vértice na GPU** (padrão, `draw_vs`): índices em lista de triângulos, os bytes crus dos arranjos de
//!     vértices, o programa do PICA (cache por conteúdo, `ProgCache`) e os uniforms; `shaders/pica_vs.vert`
//!     interpreta o programa e encaixa em 12.4; o hardware recorta (z do PICA −w..0 → 0..w; viewport do Vulkan = o
//!     do PICA) e descarta por orientação (estado do pipeline);
//!   - **shaders AOT** (`render::aot`): se o programa carregado é um dos do jogo traduzidos pelo gerador, o draw usa o
//!     pipeline do shader traduzido (mesmos buffers e o mesmo set de descritores; o programa não é enviado);
//!   - **vértices da CPU** (`draw`; `YW_VK_CPU_VS=1` ou dispositivo sem o necessário): já sombreados, recortados e
//!     projetados por `render::prepare`;
//! - uniforms do ubershader de fragmento (`render::uniforms`); estados fixos (blending, logic op, máscaras,
//!   depth/stencil, recorte e orientação) viram pipelines em cache; viewport, scissor, cor de blending e
//!   referência/máscaras do stencil são dinâmicos;
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
//! Base Vulkan 1.4 (dispositivo com apiVersion < 1.4 é recusado → renderizador soft): barreiras de
//! `synchronization2`, passadas com dynamic rendering (sem VkRenderPass/VkFramebuffer), o set 0 (UBO + 3 samplers) e
//! os sets do FXAA/present em push descriptors (nada de pool por draw), e um semáforo de timeline no lugar da cerca
//! (cada envio sinaliza o valor seguinte).
//!
//! Lotes em voo (F3): `YW_VK_SLOTS` vagas (padrão 3), cada uma com os buffers de comando de um envio (pre, principal,
//! sombra, present) e a sua faixa de timestamps. No modo assíncrono (`render::async_on`, padrão; `YW_VK_ASYNC=0` ou
//! `YW_VK_SLOTS=1` = síncrono, o comportamento de antes) `flush` só envia: espera-se a GPU quando alguém precisa do
//! resultado (devolução de uma superfície — `Surface::pending_v`; reuso de uma vaga; reserva num anel de upload que
//! ainda está em uso — `Arena`; vaga de programa — `ProgCache`). Recursos destruídos enquanto um envio pode usá-los
//! vão para `deferred` com o valor de timeline do próximo envio e só somem quando a GPU passa dele. A imagem final
//! (`present_image`) vai para um anel de saídas e é publicada por uma thread à parte quando fica pronta (`hires`).
//!
//! [H] / diferenças conhecidas para o soft: blending em ponto flutuante (arredonda em vez de truncar, ±1);
//! dentro de um lote o destino de formatos com menos bits (RGB565, RGBA4, RGB5A1) não é requantizado entre draws;
//! `color_read`/`depth_read` desligados (o soft lê 0 do destino) não são reproduzidos.

use super::aot;
use super::convert;
use super::{color_bpp, depth_bpp, hash_bytes, uniforms, DrawUniforms, GpuVtx, VsDraw, VsShading};
use crate::hle::gpu::{pica_color_fmt_to_gsp, read_pixel, tiled_offset, write_pixel};
use crate::hle::mem;
use crate::pica::raster::Ctx;
use crate::pica::texture::{self, Tex};
use ash::vk;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use yw1_rt::{r16, r32, r8, w16, w32, w8};

static VS_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/pica_vert.spv"));
static FS_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/pica_frag.spv"));
static VSG_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/pica_vs_vert.spv"));
/// pica.frag sem escrita de profundidade (`-DFAST_Z`), para a sombra N× e os intermediários (F4)
static FS_FASTZ_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/pica_frag_fastz.spv"));

const VBUF_SIZE: u64 = 32 << 20;
// Tamanhos pelos picos medidos (rota título → quarto a 3× com interpolação 120 Hz, 2026-11-11: maior ocupação ubuf
// alguns KB, ring 16,5 MB, shbuf 0,3, vdbuf 2,5, ibuf 1,1 MB) com folga. Os buffers mapeados contam inteiros na RAM
// privada do processo (estejam na VRAM pelo ReBAR ou não) — antes eram 209 MB.
const UBUF_SIZE: u64 = 2 << 20;
/// Anel de upload inicial: cresce sob demanda (`grow_ring`) quando um draw precisa de mais (ex.: sombra 6× carregada da
/// memória, ~62 MB) e nunca diminui.
const RING_SIZE: u64 = 32 << 20;
/// Limite de memória das texturas em cache (imagens RGBA8). Acima disso descarta as menos usadas recentemente.
const TEX_BUDGET: u64 = 384 << 20;
const COLOR_FMT: vk::Format = vk::Format::R8G8B8A8_UNORM;
const DEPTH_FMT: vk::Format = vk::Format::D32_SFLOAT_S8_UINT;
const VTX_STRIDE: u32 = std::mem::size_of::<GpuVtx>() as u32;
const UBO_SIZE: u64 = std::mem::size_of::<DrawUniforms>() as u64;

// shader de vértice na GPU: parâmetros por draw, bytes dos vértices (cada draw ganha uma janela de `VDATA_RANGE`
// no descritor), índices e programas em cache
const SHBUF_SIZE: u64 = 2 << 20;
/// ≥ 2 janelas de `VDATA_RANGE` (cada draw enxerga 8 MB a partir do seu início)
const VDBUF_SIZE: u64 = 16 << 20;
/// janela do descritor de vértices: mais que isso num draw → caminho da CPU
pub const VDATA_RANGE: u64 = 8 << 20;
const IBUF_SIZE: u64 = 4 << 20;
const SH_SIZE: u64 = std::mem::size_of::<VsShading>() as u64;
const PROG_BYTES: u64 = (4096 + 128) * 4;
const PROG_SLOTS: usize = 64;

/// Timestamps por vaga do anel de envios: 0 = começo do lote, 1 = fim da cena nativa, 2 = fim da sombra N×,
/// `TS_MID..TS_MID_END` = fronteiras dos intermediários da interpolação gravados na sombra, `TS_PRESENT` e o seguinte =
/// começo e fim do present da imagem final.
const QS: u32 = 16;
const TS_MID: u32 = 3;
const TS_MID_END: u32 = 14;
const TS_PRESENT: u32 = 14;

/// Vagas do anel de envios (lotes em voo): `YW_VK_SLOTS`, 1..=8, padrão 3. Com 1, todo envio é esperado na hora.
/// Vagas do anel de lotes (`YW_VK_SLOTS`, padrão 6): um quadro com interpolação faz 5–7 envios (cena, present, lote
/// dos intermediários, tela de baixo); com 3 a vaga dava a volta dentro do quadro e a thread esperava a GPU — a rede
/// de segurança da interpolação recuava em rajadas (medido 2026-11-11: 3 vagas × 6 vagas na rota a 3×).
fn slots_k() -> usize {
    static K: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *K.get_or_init(|| std::env::var("YW_VK_SLOTS").ok().and_then(|v| v.parse().ok()).unwrap_or(6).clamp(1, 8))
}

/// Envios sem espera (modo assíncrono): pedido (`render::async_on`) e com mais de uma vaga no anel.
fn async_eff() -> bool {
    slots_k() > 1 && super::async_on()
}

/// O dispositivo tem o que o shader de vértice na GPU precisa (gravado em `init`; lido por draw sem travar `VK`).
static VS_OK: AtomicBool = AtomicBool::new(false);

/// Memória para `mem_report`, lida sem travar `VK` (a linha de desempenho não espera a thread da GPU): bytes nos
/// buffers mapeados do host que o backend alocou (`host_buf` − `free_buf`), bytes do cache de texturas
/// (`Vk::tex_bytes`) e, com VK_EXT_memory_budget, a instância + o dispositivo físico para consultar o orçamento.
static HOST_BYTES: AtomicU64 = AtomicU64::new(0);
static TEX_BYTES: AtomicU64 = AtomicU64::new(0);
static BUDGET: std::sync::OnceLock<(ash::Instance, vk::PhysicalDevice)> = std::sync::OnceLock::new();

/// Versão mínima do dispositivo (a base do backend: sync2, dynamic rendering, timeline, push descriptors).
const API_MIN: u32 = vk::make_api_version(0, 1, 4, 0);

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
    let mut v = unsafe { Vk::new()? };
    // pipelines da lista salva começam a compilar já, em segundo plano (não espera nada aqui)
    // SAFETY: ver acima
    unsafe { v.prewarm_start() };
    let n = v.name.clone();
    *g = Some(v);
    Ok(n)
}

/// Grava um draw (vértices já preparados) no lote atual.
pub fn draw(ctx: &Ctx, verts: &[GpuVtx]) {
    let mut g = VK.lock().unwrap();
    let Some(v) = g.as_mut() else { return };
    // SAFETY: ver `init`
    if let Err(e) = unsafe { v.draw(ctx, Geo::Placed(verts)) } {
        v.warn(&format!("draw: {e}"));
    }
}

/// Grava um draw com o shader de vértice na GPU. Falso = não deu (buffers pequenos, viewport que o Vulkan não
/// aceita, erro): o chamador usa o caminho da CPU.
pub fn draw_vs(ctx: &Ctx, d: &VsDraw) -> bool {
    let mut g = VK.lock().unwrap();
    let Some(v) = g.as_mut() else { return false };
    // SAFETY: ver `init`
    match unsafe { v.draw(ctx, Geo::Shaded(d)) } {
        Ok(done) => done,
        Err(e) => {
            v.warn(&format!("draw (vértices na GPU): {e}"));
            false
        }
    }
}

/// O shader de vértice na GPU está disponível (dispositivo com `shaderClipDistance` e janela de storage buffer
/// suficiente)?
pub fn vs_supported() -> bool {
    VS_OK.load(Ordering::Relaxed)
}

/// Fim de uma lista de comandos com draws. O lote NÃO é executado aqui: continua aberto até alguém precisar da
/// memória (`before_read`/`before_write`, textura que lê uma superfície) ou os buffers encherem — normalmente uma vez
/// por quadro, no DisplayTransfer. `YW_VK_EAGER=1` executa e devolve tudo aqui (comportamento antigo).
pub fn end_list() {
    let mut g = VK.lock().unwrap();
    let Some(v) = g.as_mut() else { return };
    v.list += 1;
    // pipelines novos vão para o disco no máximo a cada 10 s; só os usos da lista de chaves, a cada 60 s (a escrita
    // do arquivo é numa thread à parte)
    let el = v.pcache_saved.elapsed().as_secs();
    if (v.pcache_dirty && el >= 10) || (v.keys_dirty && el >= 60) {
        v.save_pipeline_cache(false);
    }
    if eager() {
        // SAFETY: ver `init`
        if let Err(e) = unsafe { v.flush() } {
            v.warn(&format!("flush: {e}"));
        }
    }
}

/// Grava o cache de pipelines e a lista de chaves em disco agora, se mudaram (fim da rodada; durante o jogo
/// `end_list` grava sozinho).
pub fn save_pipeline_cache() {
    let mut g = VK.lock().unwrap();
    if let Some(v) = g.as_mut() {
        if v.pcache_dirty || v.keys_dirty {
            v.save_pipeline_cache(true);
        }
    }
}

/// Arquivo do cache de pipelines: `YW_VK_PIPELINE_CACHE` (caminho; `0`/`off` desliga), senão
/// `%APPDATA%/yw1/vk_pipelines.bin` (junto de `settings.toml`); nos testes, `work/pica_test/vk_pipelines.bin`.
fn pipeline_cache_path() -> Option<std::path::PathBuf> {
    match std::env::var("YW_VK_PIPELINE_CACHE") {
        Ok(v) if v == "0" || v.eq_ignore_ascii_case("off") => None,
        Ok(v) if !v.is_empty() => Some(v.into()),
        _ if cfg!(test) => Some(versioned_cache(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../work/pica_test"))),
        _ => std::env::var_os("APPDATA").map(|a| versioned_cache(std::path::PathBuf::from(a).join("yw1"))),
    }
}

/// `dir/vk_pipelines_<hash dos SPIR-V>.bin`. O cache do driver não apaga entradas de shaders antigos: a cada mudança de
/// shader o arquivo crescia (24 MB com 45 pipelines vivos, medido 2026-11-11) e era carregado inteiro na RAM. Com o hash
/// no nome, mudou um shader → arquivo novo; os de versões anteriores (e o `vk_pipelines.bin` antigo) são apagados uma vez.
fn versioned_cache(dir: std::path::PathBuf) -> std::path::PathBuf {
    static H: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    let h = *H.get_or_init(|| {
        let h = spirv_hash();
        remove_other_versions(&dir, "vk_pipelines", &format!("vk_pipelines_{h:016x}.bin"));
        h
    });
    dir.join(format!("vk_pipelines_{h:016x}.bin"))
}

/// Hash dos SPIR-V do backend (o mesmo do nome do cache do driver e da lista de pipelines).
fn spirv_hash() -> u64 {
    static H: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *H.get_or_init(|| [VS_SPV, FS_SPV, FS_FASTZ_SPV, VSG_SPV].iter().fold(0u64, |a, s| (a ^ hash_bytes(s)).rotate_left(13)))
}

/// Apaga de `dir` os arquivos `<prefixo>*.bin` que não são `mine` (versões anteriores dos shaders/da tabela AOT).
fn remove_other_versions(dir: &std::path::Path, prefix: &str, mine: &str) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.starts_with(prefix) && n.ends_with(".bin") && n != mine {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Lista de pipelines usados (pré-compilação no boot)
//
// O cache do driver (`vk_pipelines_<hash>.bin`) guarda só os blobs: no boot seguinte o 1º draw de cada estado ainda
// passa por vkCreateGraphicsPipelines na hora (rápido com o cache, ≈1 s com o driver frio). A lista abaixo guarda as
// CHAVES (`PipeKey`) de todo pipeline criado, com quantas listas de comandos usaram cada uma; no boot elas são
// pré-compiladas em segundo plano (`Vk::prewarm_start`), primeiro as que o jogo compilaria síncronas.
//
// Arquivo `%APPDATA%/yw1/vk_pipekeys_<hash>.bin` (<hash> = SPIR-V do backend ⊕ tabela AOT; `YW_VK_PIPEKEYS=<caminho>`
// sobrescreve, `0`/`off` desliga; nos testes, desligado sem a variável). É DERIVADO dos shaders do jogo: nunca vai
// para o repositório. Formato (little-endian, versão `PK_VERSION`):
//   cabeçalho (32 B): "YWPK" · versão u32 · hash dos SPIR-V u64 · hash da tabela AOT u64 · nº de AOT u32 · nº de chaves u32
//   chave (`PK_ENTRY` = 158 B): usos u64 · vs u8 · cull u8 · aot u32 · blend u8 · eq[2] u8 · fac[4] u8 · logic u8 ·
//     mask u8 · dtest u8 · dfunc u8 · dwrite u8 · stencil u8 · sfunc u8 · sops[3] u8 · fastz u8 · fs.spec u8 ·
//     fs.nst u8 · fs.flags u32 · fs.bufupd u32 · fs.atest u32 · fs.depth u32 · fs.tex[3] u32 · fs.st[6][4] u32
//   rodapé: FNV-1a 64 de tudo o que vem antes
// Campo a campo (nada de layout de memória). Na leitura: arquivo truncado, versão/magic/checksum errados ou SPIR-V de
// outra versão → lista vazia; tabela AOT diferente → descarta só as chaves com AOT; chave com valor fora do domínio
// (bool ≠ 0/1, enum fora da tabela, AOT além da tabela) → descarta a chave.
// ---------------------------------------------------------------------------------------------------------------

const PK_MAGIC: [u8; 4] = *b"YWPK";
const PK_VERSION: u32 = 1;
const PK_HEADER: usize = 32;
const PK_ENTRY: usize = 8 + 22 + 4 * 32;
/// Teto de chaves no arquivo (ficam as mais usadas)
const PK_MAX: usize = 8192;

/// Arquivo da lista de pipelines para a tabela AOT `aot_hash` (ver o bloco acima). None = desligado.
fn pipekeys_path(aot_hash: u64) -> Option<std::path::PathBuf> {
    match std::env::var("YW_VK_PIPEKEYS") {
        Ok(v) if v == "0" || v.eq_ignore_ascii_case("off") => None,
        Ok(v) if !v.is_empty() => Some(v.into()),
        _ if cfg!(test) => None,
        _ => std::env::var_os("APPDATA").map(|a| {
            let dir = std::path::PathBuf::from(a).join("yw1");
            let mine = format!("vk_pipekeys_{:016x}.bin", spirv_hash() ^ aot_hash.rotate_left(29));
            static CLEAN: std::sync::Once = std::sync::Once::new();
            CLEAN.call_once(|| remove_other_versions(&dir, "vk_pipekeys", &mine));
            dir.join(mine)
        }),
    }
}

/// Pré-compilação no boot ligada? (`YW_VK_PREWARM=0` desliga)
fn prewarm_on() -> bool {
    std::env::var("YW_VK_PREWARM").map_or(true, |v| v != "0" && !v.eq_ignore_ascii_case("off"))
}

/// Chave criável: cada campo dentro do domínio que `pipe_key`/`fs_key` produzem (é o que protege `build_pipeline`
/// — índices em `CMP`/`LOGIC`, enums do Vulkan — de um arquivo estragado). `naot` = programas na tabela AOT.
fn key_ok(k: &PipeKey, naot: u32) -> bool {
    let fs = &k.fs;
    let fs_ok = if fs.spec {
        fs.nst <= 6 && fs.atest <= 0xff && fs.st.iter().skip(fs.nst as usize).all(|s| *s == [0; 4])
    } else {
        *fs == FsKey::default()
    };
    k.cull <= 3
        && k.aot <= naot
        && (k.vs || (k.cull == 0 && k.aot == 0 && !k.fastz))
        && k.eq.iter().all(|&e| e <= 4)
        && k.fac.iter().all(|&f| f <= 14)
        && k.logic <= 15
        && k.mask <= 15
        && k.dfunc <= 7
        && k.sfunc <= 7
        && k.sops.iter().all(|&o| o <= 7)
        && fs_ok
}

/// Chave que o jogo compilaria SÍNCRONA (ubershader de fragmento, sem AOT, profundidade exata): vai primeiro na
/// pré-compilação. As outras (AOT, fragmento especializado, profundidade rápida) já têm reserva enquanto compilam.
fn key_sync(k: &PipeKey) -> bool {
    k.aot == 0 && !k.fs.spec && !k.fastz
}

/// Ordem da pré-compilação: as síncronas primeiro, cada grupo da mais usada para a menos (estável no empate).
fn prewarm_order(keys: &[(PipeKey, u64)]) -> Vec<PipeKey> {
    let mut v: Vec<&(PipeKey, u64)> = keys.iter().collect();
    v.sort_by_key(|(k, n)| (!key_sync(k), std::cmp::Reverse(*n)));
    v.into_iter().map(|(k, _)| *k).collect()
}

fn encode_keys(spv: u64, aot_hash: u64, naot: u32, keys: &[(PipeKey, u64)]) -> Vec<u8> {
    let keys: Vec<&(PipeKey, u64)> = keys.iter().filter(|(k, _)| key_ok(k, naot)).collect();
    let mut b = Vec::with_capacity(PK_HEADER + keys.len() * PK_ENTRY + 8);
    b.extend_from_slice(&PK_MAGIC);
    b.extend_from_slice(&PK_VERSION.to_le_bytes());
    b.extend_from_slice(&spv.to_le_bytes());
    b.extend_from_slice(&aot_hash.to_le_bytes());
    b.extend_from_slice(&naot.to_le_bytes());
    b.extend_from_slice(&(keys.len() as u32).to_le_bytes());
    for (k, n) in keys {
        let w32 = |b: &mut Vec<u8>, v: u32| b.extend_from_slice(&v.to_le_bytes());
        b.extend_from_slice(&n.to_le_bytes());
        // os campos u32 de domínio pequeno (conferido em `key_ok`) vão em 1 byte
        b.push(k.vs as u8);
        b.push(k.cull as u8);
        w32(&mut b, k.aot);
        b.push(k.blend as u8);
        b.extend(k.eq.iter().map(|&e| e as u8));
        b.extend(k.fac.iter().map(|&f| f as u8));
        b.extend([k.logic as u8, k.mask as u8, k.dtest as u8, k.dfunc as u8, k.dwrite as u8, k.stencil as u8, k.sfunc as u8]);
        b.extend(k.sops.iter().map(|&o| o as u8));
        b.push(k.fastz as u8);
        b.push(k.fs.spec as u8);
        b.push(k.fs.nst as u8);
        for v in [k.fs.flags, k.fs.bufupd, k.fs.atest, k.fs.depth] {
            w32(&mut b, v);
        }
        for v in k.fs.tex {
            w32(&mut b, v);
        }
        for v in k.fs.st.iter().flatten() {
            w32(&mut b, *v);
        }
    }
    let sum = super::aot::fnv1a64(&b);
    b.extend_from_slice(&sum.to_le_bytes());
    b
}

/// Lê a lista: (chaves válidas com os usos, quantas foram descartadas). Err = arquivo inteiro recusado.
fn decode_keys(data: &[u8], spv: u64, aot_hash: u64, naot: u32) -> Result<(Vec<(PipeKey, u64)>, usize), String> {
    if data.len() < PK_HEADER + 8 {
        return Err(format!("arquivo curto ({} bytes)", data.len()));
    }
    let u32_at = |o: usize| u32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]);
    let u64_at = |o: usize| u64::from_le_bytes(data[o..o + 8].try_into().unwrap());
    if data[..4] != PK_MAGIC {
        return Err("não é uma lista de pipelines (magic)".into());
    }
    if u32_at(4) != PK_VERSION {
        return Err(format!("versão {} (esperada {PK_VERSION})", u32_at(4)));
    }
    let count = u32_at(28) as u64;
    let body = data.len() - 8;
    if (PK_HEADER as u64).saturating_add(count.saturating_mul(PK_ENTRY as u64)) != body as u64 {
        return Err(format!("tamanho {} não bate com {count} chaves (truncado?)", data.len()));
    }
    if super::aot::fnv1a64(&data[..body]) != u64_at(body) {
        return Err("checksum não bate".into());
    }
    if u64_at(8) != spv {
        return Err("de outra versão dos shaders".into());
    }
    let same_aot = u64_at(16) == aot_hash;
    let mut out = Vec::with_capacity(count as usize);
    let mut bad = 0;
    for i in 0..count as usize {
        let e = &data[PK_HEADER + i * PK_ENTRY..PK_HEADER + (i + 1) * PK_ENTRY];
        let mut o = 0usize;
        let mut u8_ = || {
            o += 1;
            e[o - 1]
        };
        let uses = u64::from_le_bytes(std::array::from_fn(|_| u8_()));
        // bool: só 0/1 (outro valor = arquivo estragado → a chave sai)
        let mut ok = true;
        let mut bool_ = |v: u8| {
            ok &= v <= 1;
            v == 1
        };
        let vs = bool_(u8_());
        let cull = u8_() as u32;
        let aot = u32::from_le_bytes(std::array::from_fn(|_| u8_()));
        let blend = bool_(u8_());
        let eq = [u8_() as u32, u8_() as u32];
        let fac = [u8_() as u32, u8_() as u32, u8_() as u32, u8_() as u32];
        let logic = u8_() as u32;
        let mask = u8_() as u32;
        let dtest = bool_(u8_());
        let dfunc = u8_() as u32;
        let dwrite = bool_(u8_());
        let stencil = bool_(u8_());
        let sfunc = u8_() as u32;
        let sops = [u8_() as u32, u8_() as u32, u8_() as u32];
        let fastz = bool_(u8_());
        let spec = bool_(u8_());
        let nst = u8_() as u32;
        let mut u32_ = || u32::from_le_bytes(std::array::from_fn(|_| u8_()));
        let (flags, bufupd, atest, depth) = (u32_(), u32_(), u32_(), u32_());
        let tex = [u32_(), u32_(), u32_()];
        let st: [[u32; 4]; 6] = std::array::from_fn(|_| std::array::from_fn(|_| u32_()));
        let k = PipeKey {
            vs,
            cull,
            aot,
            blend,
            eq,
            fac,
            logic,
            mask,
            dtest,
            dfunc,
            dwrite,
            stencil,
            sfunc,
            sops,
            fs: FsKey { spec, nst, flags, bufupd, atest, depth, tex, st },
            fastz,
        };
        if ok && key_ok(&k, naot) && (same_aot || k.aot == 0) {
            out.push((k, uses));
        } else {
            bad += 1;
        }
    }
    Ok((out, bad))
}

/// A lista de pipelines desta execução: usos das execuções anteriores (o arquivo lido) presos a uma tabela AOT. Os
/// usos desta execução ficam em `PipeEntry::uses` e são somados na hora de gravar (`merged`).
struct KeyLog {
    path: Option<std::path::PathBuf>,
    aot_hash: u64,
    naot: u32,
    base: HashMap<PipeKey, u64>,
}

impl KeyLog {
    fn open(aot_hash: u64, naot: u32) -> KeyLog {
        let path = pipekeys_path(aot_hash);
        let mut base = HashMap::new();
        if let Some(data) = path.as_ref().and_then(|p| std::fs::read(p).ok()) {
            match decode_keys(&data, spirv_hash(), aot_hash, naot) {
                Ok((keys, bad)) => {
                    eprintln!(
                        "[vulkan] lista de pipelines: {} chaves{}",
                        keys.len(),
                        if bad > 0 { format!(" ({bad} descartadas: tabela AOT diferente ou valores inválidos)") } else { String::new() }
                    );
                    base.extend(keys);
                }
                Err(e) => eprintln!("[vulkan] lista de pipelines ignorada ({e}): começando vazia"),
            }
        }
        KeyLog { path, aot_hash, naot, base }
    }

    /// Lista a gravar: as lidas + os pipelines desta execução (usos somados), no máximo `PK_MAX` (as mais usadas).
    fn merged(&self, pipes: &HashMap<PipeKey, PipeEntry>) -> Vec<(PipeKey, u64)> {
        let mut m = self.base.clone();
        for (k, e) in pipes {
            *m.entry(*k).or_insert(0) += e.uses;
        }
        let mut v: Vec<(PipeKey, u64)> = m.into_iter().collect();
        if v.len() > PK_MAX {
            v.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
            v.truncate(PK_MAX);
        }
        v
    }
}

/// Grava `data` em `path` por um temporário + rename (escritas em segundo plano podem coincidir com a do fim).
fn write_atomic(path: &std::path::Path, data: &[u8], what: &str) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let tmp = path.with_extension(format!("{}.tmp", N.fetch_add(1, Ordering::Relaxed)));
    if let Err(e) = std::fs::write(&tmp, data).and_then(|_| std::fs::rename(&tmp, path)) {
        let _ = std::fs::remove_file(&tmp);
        eprintln!("[vulkan] não gravei {what} em {}: {e}", path.display());
    }
}

/// Cria o VkPipelineCache com o conteúdo salvo, se o cabeçalho (VkPipelineCacheHeaderVersionOne: tamanho,
/// versão 1, vendorID, deviceID, pipelineCacheUUID) é deste GPU/driver; senão começa vazio. Conferido aqui porque
/// alguns drivers já travaram com dados de outro dispositivo.
unsafe fn load_pipeline_cache(device: &ash::Device, props: &vk::PhysicalDeviceProperties) -> vk::PipelineCache {
    let data = pipeline_cache_path().and_then(|p| std::fs::read(p).ok()).unwrap_or_default();
    let u = |o: usize| data.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let ok = data.len() > 32
        && u(0).is_some_and(|n| n >= 32)
        && u(4) == Some(1)
        && u(8) == Some(props.vendor_id)
        && u(12) == Some(props.device_id)
        && data[16..32] == props.pipeline_cache_uuid;
    let init: &[u8] = if ok { &data } else { &[] };
    if !data.is_empty() && !ok {
        eprintln!("[vulkan] cache de pipelines em disco é de outro GPU/driver: começando vazio");
    }
    match device.create_pipeline_cache(&vk::PipelineCacheCreateInfo::default().initial_data(init), None) {
        Ok(c) => {
            if ok {
                eprintln!("[vulkan] cache de pipelines carregado ({} KB)", data.len() / 1024);
            }
            c
        }
        Err(e) => {
            eprintln!("[vulkan] vkCreatePipelineCache: {e:?} — sem cache de pipelines");
            vk::PipelineCache::null()
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

/// `YW_VK_SKIP_HIDDEN=0`: executa a sombra N× também das cenas que vão para o olho escondido (comportamento antigo).
fn skip_hidden() -> bool {
    static S: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *S.get_or_init(|| std::env::var("YW_VK_SKIP_HIDDEN").map_or(true, |v| v != "0"))
}

/// DisplayTransfer de `[lo, hi)` para o LCD. Com `hidden` (saída que a janela não mostra), se o lote aberto só tiver
/// desenhado na sombra de alvos dentro dessa faixa, o buffer da sombra não é enviado (`Vk::flush`).
pub fn before_read_lcd(lo: u32, hi: u32, hidden: bool) {
    with(|v| unsafe {
        v.discard_shadow = (hidden && skip_hidden()).then_some((lo, hi));
        let r = v.sync_range(lo, hi, false);
        v.discard_shadow = None;
        r
    });
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

/// DisplayTransfer de um alvo de tela para o framebuffer do LCD: com resolução interna (ou FXAA), constrói a imagem final
/// em N× a partir da sombra do alvo e a publica (`published`). Sem sombra válida não faz nada (a janela usa o nativo).
pub fn present_hires(req: PresentReq) {
    // os dois passos seguidos, sob o mesmo mutex (= o comportamento de antes da divisão)
    with(|v| unsafe {
        if let Some(c) = v.present_check(req)? {
            v.present_finish(c)?;
        }
        Ok(())
    });
}

/// Apresentação em dois passos (PPF adiantado): `present_hires_check` roda ANTES de o GSP sinalizar o fim do
/// DisplayTransfer (o jogo ainda não pode reescrever o alvo/textura): fecha o lote, confere se a sombra espelha o alvo
/// nativo, calcula o hash do framebuffer de saída e leva os desenhos capturados do quadro (interpolação).
/// `present_hires_finish` roda DEPOIS do PPF: imagem N×, intermediários e publicação — sem reler a memória do jogo.
/// `None` = nada a apresentar em alta resolução (desligada, olho escondido, sem sombra válida).
pub struct PresentChecked {
    pub req: PresentReq,
    /// alvo de cor com sombra e a geração de escrita da sombra no momento da conferência (`Shadow::wgen`)
    key: SurfKey,
    wgen: u64,
    /// hash do framebuffer nativo que a transferência escreveu (o que `HiRes::hash` publica)
    hash: u64,
    /// desenhos capturados do quadro (tirados de `Interp::cur` na conferência: desenhos do quadro seguinte gravados
    /// entre os dois passos não se misturam com este)
    cap: interp::FrameCap,
}

// o pedido conferido pode esperar o PPF em outra thread (fila do GSP)
const _: fn() = || {
    fn send<T: Send>() {}
    send::<PresentChecked>();
};

/// Memória do backend para a linha de desempenho: (VRAM usada pelo processo, orçamento de VRAM, bytes em buffers
/// mapeados do host, bytes no cache de texturas). VRAM = soma dos heaps DEVICE_LOCAL (com ReBAR desligado o AMD mostra
/// dois: o grande e a janela de 256 MB) pelo VK_EXT_memory_budget; sem a extensão (ou sem Vulkan) os dois primeiros
/// são 0. Não trava `VK`: pode ser chamada de qualquer thread a qualquer momento.
/// Picos de uso dos anéis de upload: (nome, tamanho, maior lote, maior ocupação), em bytes.
pub fn arena_peaks() -> Vec<(&'static str, u64, u64, u64)> {
    let mut g = VK.lock().unwrap();
    g.as_mut().map_or(Vec::new(), |v| v.arena_peaks())
}

pub fn mem_report() -> (u64, u64, u64, u64) {
    let (used, budget) = BUDGET.get().map_or((0, 0), |(inst, pd)| {
        let mut b = vk::PhysicalDeviceMemoryBudgetPropertiesEXT::default();
        let mut p2 = vk::PhysicalDeviceMemoryProperties2::default().push_next(&mut b);
        // SAFETY: instância e dispositivo físico vivem até o fim do processo (`VK` nunca é destruído antes);
        // vkGetPhysicalDeviceMemoryProperties2 não pede sincronização externa
        unsafe { inst.get_physical_device_memory_properties2(*pd, &mut p2) };
        let mp = p2.memory_properties;
        let mut r = (0u64, 0u64);
        for i in 0..(mp.memory_heap_count as usize).min(vk::MAX_MEMORY_HEAPS) {
            if mp.memory_heaps[i].flags.contains(vk::MemoryHeapFlags::DEVICE_LOCAL) {
                r.0 += b.heap_usage[i];
                r.1 += b.heap_budget[i];
            }
        }
        r
    });
    (used, budget, HOST_BYTES.load(Ordering::Relaxed), TEX_BYTES.load(Ordering::Relaxed))
}

pub fn present_hires_check(req: PresentReq) -> Option<PresentChecked> {
    let mut g = VK.lock().unwrap();
    let v = g.as_mut()?;
    // SAFETY: ver `init`
    match unsafe { v.present_check(req) } {
        Ok(c) => c,
        Err(e) => {
            v.warn(&format!("sincronismo: {e}"));
            None
        }
    }
}

pub fn present_hires_finish(c: PresentChecked) {
    with(|v| unsafe { v.present_finish(c) });
}

/// TESTE: descarta os desenhos capturados para a interpolação (começo de uma rodada).
#[cfg(test)]
pub fn test_interp_discard() {
    with(|v| {
        v.interp_discard();
        Ok(())
    });
}

/// TESTE: força uma re-renderização da interpolação (α = 0,5, o quadro capturado pareado consigo mesmo) dos desenhos
/// capturados no alvo `req.inp`, como `interp_frame_done` faria, e devolve as texturas (endereço, bytes) que esses
/// desenhos usam, na ordem dos desenhos. Os desenhos capturados são consumidos.
#[cfg(test)]
pub fn test_interp_replay(req: PresentReq) -> Result<Vec<(u32, u32)>, String> {
    let mut g = VK.lock().unwrap();
    let v = g.as_mut().ok_or("sem Vulkan")?;
    // SAFETY: ver `init`
    unsafe { v.interp_test_replay(req) }
}


/// Tudo o que só está na GPU volta para a memória (diagnóstico / antes de algo ler a memória inteira). Espera também
/// todos os envios em voo (os contadores de tempo da GPU, `stats`, ficam completos).
pub fn sync_all() {
    with(|v| unsafe {
        v.sync_range(0, u32::MAX, false)?;
        v.wait_all()
    });
}

fn with(f: impl FnOnce(&mut Vk) -> Result<(), String>) {
    let mut g = VK.lock().unwrap();
    let Some(v) = g.as_mut() else { return };
    if let Err(e) = f(v) {
        v.warn(&format!("sincronismo: {e}"));
    }
}

/// Contadores: draws, lotes, uploads de superfície, texturas criadas, pipelines, µs esperando o GPU, µs convertendo
/// superfícies (memória ↔ imagem), µs decodificando texturas, draws com shader AOT, draws com o ubershader de vértice,
/// µs compilando pipelines AOT, draws com AOT esperando o pipeline (usaram o ubershader), draws com AOT no modo
/// `dispatch` (só as variantes do teste).
/// Contadores do backend (índices: ver os usos de `stats[..]`; 15 = draws com fragmento especializado, 16 = draws
/// que queriam o especializado e usaram o ubershader porque o pipeline ainda compilava; 17 = lotes cuja sombra N× foi
/// descartada porque a cena ia para o olho escondido; 18 = draws da sombra/replay com profundidade rápida, 19 = draws
/// que podiam usar a profundidade rápida mas ficaram no exato porque o pipeline ainda compilava).
/// Pré-compilação (lista de pipelines): 20 = travadas do draw por pipeline (compilação síncrona ou espera da thread),
/// 21 = soma delas (µs), 22 = a maior (µs), 23 = pipelines pré-compilados no boot, 24 = soma das compilações deles
/// (µs), 25 = quantos deles o jogo usou, 26 = enfileirados no boot, 27 = tempo de parede da pré-compilação (µs).
pub const NSTATS: usize = 28;

pub fn stats() -> [u64; NSTATS] {
    let mut g = VK.lock().unwrap();
    g.as_mut().map_or([0; NSTATS], |v| {
        // os tempos de GPU dos envios que já terminaram entram agora (sem esperar os que ainda estão em voo)
        // SAFETY: ver `init`
        unsafe { v.poll() };
        v.stats
    })
}

macro_rules! vkr {
    ($e:expr, $what:expr) => {
        $e.map_err(|e| format!("{}: {:?}", $what, e))?
    };
}

#[path = "hires.rs"]
mod hires;
#[path = "interp.rs"]
mod interp;
pub use hires::{published_group, Group};
pub use interp::capturing as interp_capturing;
pub use hires::{publish_pending, published, HiRes, PresentReq};
use hires::{shadow_eligible, Hr, OutRef, Shadow, ShadowSrc};

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
    /// (hash, tamanho) da memória guest no último sincronismo (`None` = forçar upload). Só um hash: a cópia inteira
    /// servia apenas para comparar
    snapshot: Option<(u64, usize)>,
    synced: bool,
    /// desenhada no lote que está sendo gravado
    dirty: bool,
    /// a imagem da GPU é mais nova que a memória guest: o conteúdo está no `readback` (copiado no fim do lote) e só
    /// volta para a memória quando alguém precisa ler/escrever essa faixa (`sync_range`) — devolução preguiçosa
    pending: bool,
    /// valor de timeline do envio que gravou o `readback` (a leitura espera a GPU passar dele)
    pending_v: u64,
    /// a memória foi preenchida por inteiro (MemoryFill) com este valor já decodificado: no próximo uso, clear na GPU
    clear: Option<Clear>,
    last_used: u64,
    /// sombra N× do alvo (resolução interna; só alvos de tela, ver `hires`)
    shadow: Option<Shadow>,
}

#[derive(Clone, Copy, Debug)]
enum Clear {
    Color([f32; 4]),
    Depth(f32, u32),
}

#[derive(Hash, Eq, PartialEq, Clone, Copy, Debug)]
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
    /// shader de vértice na GPU (`pica_vs.vert`, recorte pelo hardware) em vez dos vértices da CPU
    vs: bool,
    /// só com `vs`: descarte por orientação do PICA (0 nenhum, 1 fica área < 0, 2/3 fica área > 0)
    cull: u32,
    /// só com `vs`: 0 = ubershader, k = shader AOT k − 1 da tabela atual (`aot_gen`)
    aot: u32,
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
    /// especialização do pica.frag (`FsKey::spec` = false: ubershader)
    fs: FsKey,
    /// fragmento `pica_frag_fastz` (sem gl_FragDepth: profundidade do rasterizador, faixa no viewport — `fastz_range`).
    /// Só na sombra N× e no replay da interpolação; a passada nativa (memória do jogo) nunca.
    fastz: bool,
}

/// Estado de CONTROLE do draw que vira constante de especialização do pica.frag (ver o bloco `constant_id` lá): o
/// mesmo SPIR-V, com o TEV, os modos das texturas e as flags fixos, para o driver dobrar as ramificações. Valores
/// (constantes do TEV, cores, tamanhos, referência do alpha test, zscale/zoffset) continuam no uniform, então draws
/// com o mesmo controle dividem o pipeline. Montada do `DrawUniforms` final do draw (`fs_key`).
#[derive(Hash, Eq, PartialEq, Clone, Copy, Debug, Default)]
struct FsKey {
    spec: bool,
    nst: u32,
    flags: u32,
    bufupd: u32,
    atest: u32,
    depth: u32,
    tex: [u32; 3],
    /// por estágio: fontes, operandos, combinadores, escala (zerados depois de `nst`)
    st: [[u32; 4]; 6],
}

impl FsKey {
    /// Dados de especialização na ordem dos `constant_id` 0..=8 e 10..=33.
    fn words(&self) -> ([u32; 33], [vk::SpecializationMapEntry; 33]) {
        let mut w = [0u32; 33];
        let mut e = [vk::SpecializationMapEntry::default(); 33];
        let head = [self.spec as u32, self.nst, self.flags, self.bufupd, self.atest, self.depth, self.tex[0], self.tex[1], self.tex[2]];
        let mut n = 0;
        for (id, v) in head.iter().enumerate() {
            w[n] = *v;
            e[n] = vk::SpecializationMapEntry { constant_id: id as u32, offset: 4 * n as u32, size: 4 };
            n += 1;
        }
        for i in 0..6 {
            for k in 0..4 {
                w[n] = self.st[i][k];
                e[n] = vk::SpecializationMapEntry { constant_id: 10 + 4 * i as u32 + k as u32, offset: 4 * n as u32, size: 4 };
                n += 1;
            }
        }
        (w, e)
    }
}

/// Pipeline criado e o uso dele nesta execução (vai para a lista salva, `KeyLog`).
struct PipeEntry {
    p: vk::Pipeline,
    /// última lista de comandos (`Vk::list`) que o usou e em quantas listas foi usado
    last: u64,
    uses: u64,
    /// veio da pré-compilação do boot (conta em "usadas" no 1º uso)
    warm: bool,
}

/// Quem compilou um pipeline: o draw, na hora (trava o jogo); a thread em segundo plano a pedido do jogo; ou a
/// pré-compilação do boot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Origin {
    Sync,
    Game,
    Warm,
}

/// Chave especializada do draw a partir dos uniforms finais (depois de `DEPTH_ZNDC` e das texturas ausentes).
fn fs_key(u: &DrawUniforms) -> FsKey {
    let nst = u.tex[0][3].min(6);
    let mut st = [[0u32; 4]; 6];
    for (i, s) in st.iter_mut().enumerate().take(nst as usize) {
        *s = [u.tev[i][0], u.tev[i][1], u.tev[i][2], u.scale[i / 4][i % 4]];
    }
    let flags = u.misc[3];
    let tex = [0, 1, 2].map(|i| if flags & (super::F_TEX0 << i) != 0 { u.tex[i][1] } else { 0 });
    let atest = if u.misc[2] & 1 != 0 { u.misc[2] & 0xff } else { 0 };
    FsKey { spec: true, nst, flags, bufupd: u.misc[1], atest, depth: u.depth[3], tex, st }
}

static FS_SPEC: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(2);

/// Fragmento especializado ligado? (`YW_VK_FSPEC=0` desliga; `set_fs_spec` nos testes)
pub fn fs_spec() -> bool {
    match FS_SPEC.load(Ordering::Relaxed) {
        2 => {
            let on = std::env::var("YW_VK_FSPEC").map_or(true, |v| v != "0");
            FS_SPEC.store(on as u8, Ordering::Relaxed);
            on
        }
        v => v != 0,
    }
}

pub fn set_fs_spec(on: bool) {
    FS_SPEC.store(on as u8, Ordering::Relaxed);
}

static FASTZ: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(2);

/// Profundidade rápida (F4) na sombra N× e no replay da interpolação? (`YW_VK_FASTZ=0` desliga; `set_fastz` nos
/// testes)
pub fn fastz() -> bool {
    match FASTZ.load(Ordering::Relaxed) {
        2 => {
            let on = std::env::var("YW_VK_FASTZ").map_or(true, |v| v != "0");
            FASTZ.store(on as u8, Ordering::Relaxed);
            on
        }
        v => v != 0,
    }
}

pub fn set_fastz(on: bool) {
    FASTZ.store(on as u8, Ordering::Relaxed);
}

/// Faixa do viewport (min_depth, max_depth) de um draw com o fragmento `pica_frag_fastz`, ou None se ele tem de
/// ficar com o gl_FragDepth exato.
///
/// Vértices na GPU: gl_FragCoord.z (faixa 0..1) = −z/w do PICA e o pica.frag faz zi = ⌊clamp(zoffset −
/// zscale·gl_FragCoord.z)·M⌋, guardado como zi·2⁻²⁴ (M = máximo do formato, `u.depth[0]`). Com min = zoffset·M·2⁻²⁴ e
/// max = (zoffset − zscale)·M·2⁻²⁴, o rasterizador entrega x·2⁻²⁴ com x = (zoffset − zscale·z_ndc)·M — a mesma escala,
/// só sem o ⌊⌋. Contra valores guardados quantizados (k inteiro) LESS e GEQUAL são exatos (⌊x⌋ < k ⇔ x < k; ⌊x⌋ ≥ k ⇔
/// x ≥ k), ALWAYS/NEVER não olham o valor; o que muda é só o desempate (< 1 unidade) entre valores escritos por draws
/// rápidos. LEQUAL/GREATER (diferença de até 1 unidade) e EQUAL/NOTEQUAL (o valor não quantizado quase nunca é igual)
/// ficam no caminho exato, assim como o w-buffer (precisa de gl_FragCoord.w), os vértices da CPU (`DEPTH_FRAGCOORD`,
/// depth clamp) e faixas fora de [0, 1] (o Vulkan exige min/max dentro dela; min > max é permitido).
/// Sem teste nem escrita de profundidade o valor não importa: faixa 0..1.
fn fastz_range(ctx: &Ctx, u: &DrawUniforms, k: &PipeKey) -> Option<(f32, f32)> {
    if !k.vs || ctx.wbuffer || u.depth[3] & 3 != super::DEPTH_ZNDC {
        return None;
    }
    if !k.dtest {
        return Some((0.0, 1.0));
    }
    // PICA: 0 nunca, 1 sempre, 4 menor, 7 maior ou igual
    if !matches!(k.dfunc, 0 | 1 | 4 | 7) {
        return None;
    }
    let m = u.depth[0] as f64 * (1.0 / 16_777_216.0);
    let (zs, zo) = (f32::from_bits(u.depth[1]) as f64, f32::from_bits(u.depth[2]) as f64);
    let (a, b) = ((zo * m) as f32, ((zo - zs) * m) as f32);
    let ok = |v: f32| v.is_finite() && (0.0..=1.0).contains(&v);
    (ok(a) && ok(b)).then_some((a, b))
}

struct Vk {
    _entry: ash::Entry,
    instance: ash::Instance,
    device: ash::Device,
    queue: vk::Queue,
    mem_props: vk::PhysicalDeviceMemoryProperties,
    name: String,
    logic_ok: bool,
    /// VK_KHR_push_descriptor (o set 0 dos draws e os sets do FXAA/present são empurrados no buffer de comandos)
    pd: ash::khr::push_descriptor::Device,
    /// set 0 (UBO + 3 samplers do fragmento), layout de push descriptor
    dsl: vk::DescriptorSetLayout,
    playout: vk::PipelineLayout,
    vs: vk::ShaderModule,
    fs: vk::ShaderModule,
    /// `pica_frag_fastz` (`PipeKey::fastz`)
    fs_fastz: vk::ShaderModule,
    pipelines: HashMap<PipeKey, PipeEntry>,
    /// cache de pipelines próprio, persistido em disco (`pipeline_cache_path`): a 1ª compilação de cada pipeline leva
    /// 0,1–1 s sem cache; com ele o próximo boot só desserializa. `pcache_dirty` = pipeline novo desde o último save.
    pcache: vk::PipelineCache,
    pcache_dirty: bool,
    pcache_saved: std::time::Instant,
    /// lista das chaves usadas (pré-compilação no próximo boot) e se mudou desde o último save
    keylog: KeyLog,
    keys_dirty: bool,
    /// pré-compilação do boot: início, trabalhos ainda na fila/em compilação, avisos de travada já impressos
    prewarm_t0: Option<std::time::Instant>,
    warm_left: usize,
    stall_logged: u32,
    /// recursos do shader de vértice na GPU, criados no 1º draw que os usa (None = ainda não, ou sem suporte)
    vsg: Option<VsGpu>,
    /// o dispositivo serve para o shader de vértice na GPU (falso também se a criação dos recursos falhou)
    vs_dev: bool,
    /// minStorageBufferOffsetAlignment (≥ 16)
    salign: u64,
    /// anel de envios (lotes em voo) e a vaga em uso / usada por último
    slots: Vec<Slot>,
    slot: usize,
    /// buffers de comando do lote em gravação (os da vaga `slot`, copiados em `begin`)
    cmd_pre: vk::CommandBuffer,
    cmd_main: vk::CommandBuffer,
    /// grava os mesmos draws dos alvos de tela em N× (resolução interna; ver `hires`)
    cmd_shadow: vk::CommandBuffer,
    /// 1ª query da vaga do lote em gravação em `ts_pool`
    ts_base: u32,
    /// fronteiras de intermediários da interpolação com timestamp neste lote (`TS_MID..`)
    mid_b: u32,
    /// envios ainda não recolhidos (em ordem de valor), o maior valor que a GPU já passou e o modo dos envios atuais
    subs: std::collections::VecDeque<Sub>,
    done_v: u64,
    async_mode: bool,
    /// tempo de GPU (ns, pelos timestamps) do trabalho que não é intermediário da interpolação (cena nativa, sombra,
    /// present) e dos intermediários (soma e quantos): o orçamento da interpolação (`interp`)
    gpu_work_ns: u64,
    gpu_mid_ns: u64,
    gpu_mid_n: u64,
    pass_s: Option<(SurfKey, SurfKey)>,
    /// profundidade da passada aberta em `cmd_shadow` quando ela está em DEPTH_STENCIL_ATTACHMENT_OPTIMAL (`dopt`;
    /// null = GENERAL): `end_pass_s` a devolve a GENERAL
    pass_s_dimg: vk::Image,
    /// clear inicial da profundidade do alvo de re-renderização ainda não feito (com `dopt`: vai no loadOp da 1ª
    /// passada do replay; `interp_record_mid`)
    replay_dclear: Option<(f32, u32)>,
    /// sombra do lote aberto: alvos (cor, profundidade) que receberam draws na sombra, e se o buffer da sombra tem
    /// outra coisa (re-renderização da interpolação) — `flush` só descarta a sombra de um lote "puro"
    shadow_drawn: Vec<(SurfKey, SurfKey)>,
    shadow_other: bool,
    /// faixa do DisplayTransfer para o olho escondido em andamento (`before_read_lcd`)
    discard_shadow: Option<(u32, u32)>,
    hr: Hr,
    /// interpolação de quadros: captura dos desenhos da tela de cima e o alvo de re-renderização (ver `interp`)
    ip: interp::Interp,
    replay: Option<hires::ReplayTarget>,
    /// texturas do desenho re-renderizado agora (as chaves capturadas com ele; `interp` põe antes de cada desenho do
    /// replay): o replay não relê a memória do jogo nem toca no `tex_hash`
    replay_tex: Option<[Option<TexKey>; 3]>,
    /// texturas dos quadros capturados (anterior ∪ em curso): `evict` não as remove, o replay precisa delas
    tex_pinned: std::collections::HashSet<TexKey>,
    /// recursos a destruir depois que a GPU passar do valor de timeline (o do envio que ainda pode usá-los)
    deferred: std::collections::VecDeque<(u64, Res)>,
    /// semáforo de timeline: cada envio sinaliza `tl_value + 1` (`submit`); `tl_value` = último valor enviado
    timeline: vk::Semaphore,
    tl_value: u64,
    sampler: vk::Sampler,
    vbuf: HostBuf,
    va: Arena,
    ubuf: HostBuf,
    ua: Arena,
    ustride: u64,
    ring: HostBuf,
    ra: Arena,
    surfaces: HashMap<SurfKey, Surface>,
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
    /// avisos já impressos (`warn`, só erros/condições anormais: cota de 20) e avisos informativos de compilação
    /// lenta de pipeline (cota própria, para não esconder os erros que vierem depois)
    warned: u32,
    slow_logged: u32,
    stats: [u64; NSTATS],
    /// timestamps da GPU por lote (início, fim da cena nativa, fim da sombra N×) e o período do contador (ns)
    ts_pool: vk::QueryPool,
    ts_period: f32,
    /// perfil por draw (`YW_VK_DRAWPROF`, diagnóstico)
    prof: Option<super::drawprof::Prof>,
    /// shaders AOT: geração da tabela vista, módulos criados sob demanda, (hash do programa, entrada) → AOT, e os
    /// programas sem AOT já registrados no log
    aot_gen: u64,
    aot_table: Option<std::sync::Arc<aot::Table>>,
    aot_mods: Vec<Option<vk::ShaderModule>>,
    aot_memo: HashMap<(u64, u32), Option<usize>>,
    aot_logged: std::collections::HashSet<(u32, Option<(usize, usize)>)>,
    /// compilação dos pipelines AOT em segundo plano e os que falharam (ficam com o ubershader)
    compiler: Option<Compiler>,
    aot_failed: std::collections::HashSet<PipeKey>,
}

/// Geometria de um draw: vértices prontos da CPU ou o pedido para o shader de vértice na GPU.
enum Geo<'a> {
    Placed(&'a [GpuVtx]),
    Shaded(&'a VsDraw<'a>),
}

/// Recursos do shader de vértice na GPU. O set 1 (parâmetros, programa, vértices) é um só, gravado uma vez: por
/// draw só mudam os deslocamentos dinâmicos.
struct VsGpu {
    playout: vk::PipelineLayout,
    module: vk::ShaderModule,
    set: vk::DescriptorSet,
    salign: u64,
    sh_stride: u64,
    prog_stride: u64,
    shbuf: HostBuf,
    sha: Arena,
    vdbuf: HostBuf,
    vda: Arena,
    ibuf: HostBuf,
    ia: Arena,
    pbuf: HostBuf,
    progs: ProgCache,
}

/// Uma vaga do anel de envios: os buffers de comando de um envio e o valor de timeline do último envio que a usou
/// (reusar = esperar esse valor).
struct Slot {
    pre: vk::CommandBuffer,
    main: vk::CommandBuffer,
    shadow: vk::CommandBuffer,
    /// present da imagem final (`hires::present_image`)
    hr: vk::CommandBuffer,
    value: u64,
}

/// O que um envio mediu (timestamps da vaga, lidos quando a GPU termina: `Vk::retire`).
#[derive(Clone, Copy)]
enum SubKind {
    /// lote: timestamps 0..`nts` (2 = sombra não enviada) e `mb` fronteiras de intermediários (`TS_MID..`)
    Batch { nts: u32, mb: u32 },
    /// present da imagem final (`TS_PRESENT`, `TS_PRESENT + 1`)
    Present,
}

#[derive(Clone, Copy)]
struct Sub {
    value: u64,
    slot: usize,
    kind: SubKind,
}

/// Recurso com destruição adiada (`Vk::defer`).
enum Res {
    Img(Img),
    Buf(HostBuf),
}

/// Buffer de upload como anel compartilhado pelos lotes em voo. As reservas do lote em gravação avançam `head`; ao
/// enviar, a parte dele (`cur`, com a sobra do fim do buffer quando dá a volta) fica ocupada até a GPU passar do valor
/// de timeline do envio. O ocupado é sempre um arco contíguo terminando em `head` (os envios terminam em ordem).
/// Sem nada em voo e com o lote vazio, volta ao começo: no modo síncrono os deslocamentos são os de antes (cada lote
/// a partir de 0) e as condições de "cheio" (`fits`) são exatamente as antigas.
#[derive(Default)]
struct Arena {
    size: u64,
    head: u64,
    /// bytes do lote em gravação
    cur: u64,
    /// lotes enviados e não terminados: (valor de timeline, bytes), e a soma
    inflight: std::collections::VecDeque<(u64, u64)>,
    busy: u64,
    /// picos medidos (para dimensionar os buffers): maior lote, maior ocupação (lote + lotes em voo)
    pk_lote: u64,
    pk_total: u64,
}

impl Arena {
    fn new(size: u64) -> Arena {
        Arena { size, ..Arena::default() }
    }

    /// (início, bytes consumidos) de uma reserva de `n` bytes a partir de `head`, com alinhamento `al` e uma janela de
    /// `win` ≥ `n` bytes que tem de caber no buffer (o descritor de vértices enxerga `VDATA_RANGE` a partir do início).
    fn place(&self, n: u64, win: u64, al: u64) -> (u64, u64) {
        let s = align(self.head, al.max(1));
        if s + win <= self.size {
            (s, s - self.head + n)
        } else {
            (0, self.size - self.head + n)
        }
    }

    /// A reserva cabe NESTE lote quando os anteriores terminarem? (lote vazio: sempre — o anel volta ao começo.) Falso
    /// = o lote tem de ser executado antes (no modo síncrono: a mesma conta de antes, deslocamento + tamanho > buffer).
    fn fits(&self, n: u64, win: u64, al: u64) -> bool {
        self.cur == 0 || self.cur + self.place(n, win, al).1 <= self.size
    }

    /// Reserva agora, se não pisa em dados de lotes em voo (None = esperar o mais antigo, `oldest`).
    fn try_alloc(&mut self, n: u64, win: u64, al: u64) -> Option<u64> {
        if self.cur == 0 && self.busy == 0 {
            self.head = 0;
        }
        let (s, c) = self.place(n, win, al);
        if self.busy + self.cur + c > self.size {
            return None;
        }
        self.head = s + n;
        self.cur += c;
        self.pk_lote = self.pk_lote.max(self.cur);
        self.pk_total = self.pk_total.max(self.busy + self.cur);
        Some(s)
    }

    fn oldest(&self) -> Option<u64> {
        self.inflight.front().map(|x| x.0)
    }

    /// O lote em gravação foi enviado com o valor `v`.
    fn submit(&mut self, v: u64) {
        if self.cur > 0 {
            self.inflight.push_back((v, self.cur));
            self.busy += self.cur;
            self.cur = 0;
        }
    }

    /// A GPU passou de `done`: libera as partes dos lotes até ele.
    fn retire(&mut self, done: u64) {
        while let Some(&(v, b)) = self.inflight.front() {
            if v > done {
                break;
            }
            self.busy -= b;
            self.inflight.pop_front();
        }
        if self.busy == 0 && self.cur == 0 {
            self.head = 0;
        }
    }
}

/// Qual anel de upload (`Vk::arena`).
#[derive(Clone, Copy, Debug)]
enum Ar {
    U,
    R,
    V,
    Sh,
    Vd,
    I,
}

/// Programas do PICA em vagas de `pbuf`, por hash do conteúdo (código + descritores). A geração de `pica::State`
/// evita recalcular o hash quando nada foi reescrito desde o último draw.
struct ProgCache {
    by_hash: HashMap<u64, usize>,
    /// por vaga: (hash, valor de timeline do último envio que a usou — `Vk::use_value`)
    slots: Vec<Option<(u64, u64)>>,
    /// (geração, hash) do último programa visto
    last: Option<(u64, u64)>,
}

// SAFETY: o objeto só é usado com o mutex `VK`; os ponteiros mapeados apontam para memória do dispositivo que vive
// junto com ele.
unsafe impl Send for Vk {}

/// `YW_VK_EAGER=1`: devolve todos os render targets no fim de cada lote (comportamento antigo, para diagnóstico).
fn eager() -> bool {
    static E: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *E.get_or_init(|| std::env::var("YW_VK_EAGER").is_ok_and(|v| v != "0"))
}

/// Uso de um buffer mapeado (`Vk::host_buf`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum HostMem {
    /// a CPU só escreve e a GPU lê (ubuf, vbuf, ring, shbuf, vdbuf, ibuf, pbuf)
    Upload,
    /// a CPU lê o que a GPU escreveu (readback das superfícies, saída do present)
    Readback,
}

/// ReBAR: buffers de upload em DEVICE_LOCAL|HOST_VISIBLE (`YW_VK_REBAR=0` desliga: memória do host, como antes).
fn rebar() -> bool {
    static R: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *R.get_or_init(|| std::env::var("YW_VK_REBAR").map_or(true, |v| v != "0"))
}

fn align(v: u64, a: u64) -> u64 {
    v.div_ceil(a) * a
}

/// "Retrato" de uma faixa de memória guest para comparar depois (hash + tamanho; ver `Surface::snapshot`).
fn snap_of(mem: &[u8]) -> Option<(u64, usize)> {
    Some((hash_bytes(mem), mem.len()))
}

/// A memória ainda é a do retrato?
fn snap_eq(snap: Option<(u64, usize)>, mem: &[u8]) -> bool {
    snap.is_some_and(|(h, n)| n == mem.len() && h == hash_bytes(mem))
}

/// Tira do cache de hash por lista (`Vk::tex_hash`) as texturas cuja faixa cruza `[lo, hi)`.
fn forget_tex_hash(m: &mut HashMap<(u32, u32, u32, u32), (u64, u64)>, lo: u32, hi: u32) {
    m.retain(|&(a, w, h, f), _| {
        let n = w * h * texture::bits_per_texel(f) / 8;
        !(a < hi && lo < a.wrapping_add(n))
    });
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
            .api_version(API_MIN);
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
        // os recusados por versão/extensão (vão na mensagem de erro: o motor cai no renderizador soft)
        let mut old = Vec::new();
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
            let exts = instance.enumerate_device_extension_properties(pd).unwrap_or_default();
            let has = |n: &std::ffi::CStr| exts.iter().any(|e| e.extension_name_as_c_str() == Ok(n));
            // Vulkan 1.4 (sync2, dynamic rendering e timeline são obrigatórios a partir da 1.3) + push descriptors
            // pelo nome da extensão (o ash 0.38 só tem a forma KHR); memory_budget é opcional (só `mem_report`)
            if props.api_version < API_MIN || !has(ash::khr::push_descriptor::NAME) {
                let v = props.api_version;
                let n = props.device_name_as_c_str().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                old.push(format!("{n} {}.{}.{}", vk::api_version_major(v), vk::api_version_minor(v), vk::api_version_patch(v)));
                continue;
            }
            let budget = has(ash::ext::memory_budget::NAME);
            if let (Some(qf), true, true) = (qf, ok_depth, feats.depth_clamp == vk::TRUE) {
                cands.push((i, pd, qf as u32, props, feats, budget));
            }
        }
        let pick = match want {
            Some(n) => cands.iter().find(|c| c.0 == n),
            None => cands
                .iter()
                .find(|c| c.3.device_type == vk::PhysicalDeviceType::DISCRETE_GPU)
                .or(cands.first()),
        };
        let Some(&(_, pdev, qf, props, feats, budget)) = pick else {
            return Err(format!(
                "nenhum GPU Vulkan 1.4 com VK_KHR_push_descriptor, fila gráfica, depthClamp e D32_SFLOAT_S8_UINT{}",
                if old.is_empty() { String::new() } else { format!(" (recusados: {})", old.join(", ")) }
            ));
        };
        let name = props
            .device_name_as_c_str()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "?".into());
        let logic_ok = feats.logic_op == vk::TRUE;
        // shader de vértice na GPU: plano de recorte do usuário em gl_ClipDistance, janela de vértices por draw
        let vs_ok = feats.shader_clip_distance == vk::TRUE && props.limits.max_storage_buffer_range as u64 >= VDATA_RANGE;
        let enabled = vk::PhysicalDeviceFeatures {
            depth_clamp: vk::TRUE,
            logic_op: feats.logic_op,
            shader_clip_distance: feats.shader_clip_distance,
            pipeline_statistics_query: if super::drawprof::wanted().is_some() { feats.pipeline_statistics_query } else { vk::FALSE },
            ..Default::default()
        };
        let prio = [1.0f32];
        let qci = [vk::DeviceQueueCreateInfo::default().queue_family_index(qf).queue_priorities(&prio)];
        let mut f12 = vk::PhysicalDeviceVulkan12Features::default().timeline_semaphore(true);
        let mut f13 = vk::PhysicalDeviceVulkan13Features::default().synchronization2(true).dynamic_rendering(true);
        let mut f2 = vk::PhysicalDeviceFeatures2::default().features(enabled).push_next(&mut f12).push_next(&mut f13);
        let mut dexts = vec![ash::khr::push_descriptor::NAME.as_ptr()];
        if budget {
            dexts.push(ash::ext::memory_budget::NAME.as_ptr());
        }
        let dci = vk::DeviceCreateInfo::default().queue_create_infos(&qci).enabled_extension_names(&dexts).push_next(&mut f2);
        let device = vkr!(instance.create_device(pdev, &dci, None), "vkCreateDevice");
        let pd = ash::khr::push_descriptor::Device::new(&instance, &device);
        // timestamps por vaga do anel (`QS`): só para medir (stats[13] = µs de GPU da cena nativa, stats[14] = da sombra
        // N×) e para o orçamento da interpolação (tempo de GPU da cena, do present e de cada intermediário)
        let nslots = slots_k();
        let ts_pool = vkr!(
            device.create_query_pool(
                &vk::QueryPoolCreateInfo::default().query_type(vk::QueryType::TIMESTAMP).query_count(QS * nslots as u32),
                None
            ),
            "vkCreateQueryPool"
        );
        let ts_period = props.limits.timestamp_period;
        let prof = match super::drawprof::wanted() {
            Some(path) if feats.pipeline_statistics_query == vk::TRUE => match super::drawprof::Prof::new(&device, &path, nslots) {
                Ok(p) => Some(p),
                Err(e) => {
                    eprintln!("[vulkan] {e}");
                    None
                }
            },
            Some(_) => {
                eprintln!("[vulkan] YW_VK_DRAWPROF: dispositivo sem pipelineStatisticsQuery");
                None
            }
            None => None,
        };
        let queue = device.get_device_queue(qf, 0);
        let mem_props = instance.get_physical_device_memory_properties(pdev);

        // passadas: dynamic rendering (`begin_pass`), cor RGBA8 + profundidade/stencil em GENERAL, LOAD/STORE
        // set 0 em push descriptor: o UBO é UNIFORM_BUFFER comum (push não aceita dinâmico), com o deslocamento do
        // draw no próprio DescriptorBufferInfo
        let fsf = vk::ShaderStageFlags::FRAGMENT;
        let binds = [
            vk::DescriptorSetLayoutBinding::default()
                .binding(0)
                .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
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
            device.create_descriptor_set_layout(
                &vk::DescriptorSetLayoutCreateInfo::default()
                    .flags(vk::DescriptorSetLayoutCreateFlags::PUSH_DESCRIPTOR_KHR)
                    .bindings(&binds),
                None
            ),
            "vkCreateDescriptorSetLayout"
        );
        let dsls = [dsl];
        let pcr = [vk::PushConstantRange { stage_flags: vk::ShaderStageFlags::FRAGMENT, offset: 0, size: 4 }];
        let playout = vkr!(
            device.create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(&dsls).push_constant_ranges(&pcr), None),
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
        let fs_fastz = module(FS_FASTZ_SPV)?;
        let pcache = load_pipeline_cache(&device, &props);

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
                    .command_buffer_count(4 * nslots as u32)
            ),
            "vkAllocateCommandBuffers"
        );
        let slots: Vec<Slot> =
            cbs.chunks_exact(4).map(|c| Slot { pre: c[0], main: c[1], shadow: c[2], hr: c[3], value: 0 }).collect();
        if nslots > 1 {
            eprintln!("[vulkan] {nslots} lotes em voo (YW_VK_SLOTS; modo assíncrono: YW_VK_ASYNC / menu \"Renderização\")");
        }
        let mut sti = vk::SemaphoreTypeCreateInfo::default().semaphore_type(vk::SemaphoreType::TIMELINE).initial_value(0);
        let timeline = vkr!(
            device.create_semaphore(&vk::SemaphoreCreateInfo::default().push_next(&mut sti), None),
            "vkCreateSemaphore(timeline)"
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
        let salign = props.limits.min_storage_buffer_offset_alignment.max(16);

        let mut v = Vk {
            _entry: entry,
            instance,
            device,
            queue,
            mem_props,
            name,
            logic_ok,
            pd,
            dsl,
            playout,
            vs,
            fs,
            fs_fastz,
            pipelines: HashMap::new(),
            pcache,
            pcache_dirty: false,
            pcache_saved: std::time::Instant::now(),
            keylog: {
                // a tabela AOT já está registrada (o executável registra antes de iniciar o jogo); desligada por
                // YW_VK_NO_AOT, as chaves com AOT continuam na lista (só não são pré-compiladas)
                let t = aot::table();
                KeyLog::open(t.as_ref().map_or(0, |t| t.hash), t.as_ref().map_or(0, |t| t.progs.len() as u32))
            },
            keys_dirty: false,
            prewarm_t0: None,
            warm_left: 0,
            stall_logged: 0,
            vsg: None,
            vs_dev: false,
            salign,
            cmd_pre: slots[0].pre,
            cmd_main: slots[0].main,
            cmd_shadow: slots[0].shadow,
            // o 1º `take_slot` avança para a vaga 0
            slot: nslots - 1,
            slots,
            ts_base: 0,
            mid_b: 0,
            subs: std::collections::VecDeque::new(),
            done_v: 0,
            async_mode: async_eff(),
            gpu_work_ns: 0,
            gpu_mid_ns: 0,
            gpu_mid_n: 0,
            pass_s: None,
            pass_s_dimg: vk::Image::null(),
            replay_dclear: None,
            shadow_drawn: Vec::new(),
            shadow_other: false,
            discard_shadow: None,
            hr: Hr::default(),
            ip: interp::Interp::default(),
            replay: None,
            replay_tex: None,
            tex_pinned: std::collections::HashSet::new(),
            deferred: std::collections::VecDeque::new(),
            timeline,
            tl_value: 0,
            sampler,
            vbuf: HostBuf { buf: vk::Buffer::null(), mem: vk::DeviceMemory::null(), ptr: std::ptr::null_mut(), size: 0 },
            va: Arena::new(0),
            ubuf: HostBuf { buf: vk::Buffer::null(), mem: vk::DeviceMemory::null(), ptr: std::ptr::null_mut(), size: 0 },
            ua: Arena::new(UBUF_SIZE),
            ustride,
            ring: HostBuf { buf: vk::Buffer::null(), mem: vk::DeviceMemory::null(), ptr: std::ptr::null_mut(), size: 0 },
            ra: Arena::new(RING_SIZE),
            surfaces: HashMap::new(),
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
            slow_logged: 0,
            stats: [0; NSTATS],
            ts_pool,
            ts_period,
            prof,
            aot_gen: 0,
            aot_table: None,
            aot_mods: Vec::new(),
            aot_memo: HashMap::new(),
            aot_logged: std::collections::HashSet::new(),
            compiler: None,
            aot_failed: std::collections::HashSet::new(),
        };
        // `vbuf` (32 MB, só para vértices da CPU) é criado no 1º draw que o usa (`draw`, `Geo::Placed`)
        v.ubuf = v.host_buf(UBUF_SIZE, vk::BufferUsageFlags::UNIFORM_BUFFER, HostMem::Upload)?;
        v.ring = v.host_buf(RING_SIZE, vk::BufferUsageFlags::TRANSFER_SRC, HostMem::Upload)?;
        if rebar() {
            // maior heap com memória DEVICE_LOCAL|HOST_VISIBLE: ~256 MB = janela do BAR sem ReBAR; a VRAM toda = ReBAR
            let want = vk::MemoryPropertyFlags::DEVICE_LOCAL | vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
            let mp = &v.mem_props;
            let heap = (0..mp.memory_type_count as usize)
                .filter(|&i| mp.memory_types[i].property_flags.contains(want))
                .map(|i| mp.memory_heaps[mp.memory_types[i].heap_index as usize].size)
                .max();
            match heap {
                Some(s) => eprintln!("[vulkan] buffers de upload em VRAM visível pela CPU (heap de {} MB; YW_VK_REBAR=0 desliga)", s >> 20),
                None => eprintln!("[vulkan] sem memória DEVICE_LOCAL|HOST_VISIBLE: buffers de upload na memória do host"),
            }
        }
        v.dummy =v.image(1, 1, COLOR_FMT, vk::ImageUsageFlags::SAMPLED, vk::ImageAspectFlags::COLOR)?;
        if !logic_ok {
            eprintln!("[vulkan] {}: sem logicOp — só a op Copy será usada", v.name);
        }
        // os buffers do shader de vértice na GPU (≈ 73 MB mapeados) só são criados no 1º draw que o usa (`vsg_ready`):
        // com YW_VK_CPU_VS=1 nunca são
        if !vs_ok {
            eprintln!("[vulkan] {}: sem shaderClipDistance — vértices pela CPU", v.name);
        }
        v.vs_dev = vs_ok;
        v.salign = salign;
        VS_OK.store(vs_ok, Ordering::Relaxed);
        // só agora (nenhum `?` depois): a instância guardada é a do `Vk` que vai para o static e nunca é destruído
        // (antes ficava guardada uma instância que um erro posterior destruiria — achado da revisão da F2)
        if budget {
            let _ = BUDGET.set((v.instance.clone(), pdev));
        }
        Ok(v)
    }

    /// Recursos do shader de vértice na GPU prontos? Cria no 1º uso; se falhar, avisa e desliga o caminho (o pica
    /// passa a usar os vértices da CPU).
    unsafe fn vsg_ready(&mut self) -> bool {
        if self.vsg.is_some() {
            return true;
        }
        if !self.vs_dev {
            return false;
        }
        match self.vs_gpu(self.salign) {
            Ok(g) => {
                self.vsg = Some(g);
                true
            }
            Err(e) => {
                eprintln!("[vulkan] shader de vértice na GPU indisponível ({e}); vértices pela CPU");
                self.vs_dev = false;
                VS_OK.store(false, Ordering::Relaxed);
                false
            }
        }
    }

    /// Set 1 do shader de vértice na GPU (parâmetros, programa, vértices: storage buffers dinâmicos), layout com os
    /// dois sets e os buffers.
    unsafe fn vs_gpu(&self, salign: u64) -> Result<VsGpu, String> {
        let d = &self.device;
        let sb = |b: u32| {
            vk::DescriptorSetLayoutBinding::default()
                .binding(b)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER_DYNAMIC)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::VERTEX)
        };
        let binds = [sb(0), sb(1), sb(2)];
        let dsl = vkr!(
            d.create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&binds), None),
            "vkCreateDescriptorSetLayout(vs)"
        );
        let dsls = [self.dsl, dsl];
        let pcr = [vk::PushConstantRange { stage_flags: vk::ShaderStageFlags::FRAGMENT, offset: 0, size: 4 }];
        let playout = vkr!(
            d.create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(&dsls).push_constant_ranges(&pcr), None),
            "vkCreatePipelineLayout(vs)"
        );
        let words = ash::util::read_spv(&mut std::io::Cursor::new(VSG_SPV)).map_err(|e| e.to_string())?;
        let module = vkr!(
            d.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&words), None),
            "vkCreateShaderModule(vs)"
        );
        let sizes = [vk::DescriptorPoolSize { ty: vk::DescriptorType::STORAGE_BUFFER_DYNAMIC, descriptor_count: 3 }];
        let pool = vkr!(
            d.create_descriptor_pool(&vk::DescriptorPoolCreateInfo::default().max_sets(1).pool_sizes(&sizes), None),
            "vkCreateDescriptorPool(vs)"
        );
        let l1 = [dsl];
        let set = vkr!(
            d.allocate_descriptor_sets(&vk::DescriptorSetAllocateInfo::default().descriptor_pool(pool).set_layouts(&l1)),
            "vkAllocateDescriptorSets(vs)"
        )[0];
        let st = vk::BufferUsageFlags::STORAGE_BUFFER;
        let prog_stride = align(PROG_BYTES, salign);
        let shbuf = self.host_buf(SHBUF_SIZE, st, HostMem::Upload)?;
        let vdbuf = self.host_buf(VDBUF_SIZE, st, HostMem::Upload)?;
        let ibuf = self.host_buf(IBUF_SIZE, vk::BufferUsageFlags::INDEX_BUFFER, HostMem::Upload)?;
        let pbuf = self.host_buf(prog_stride * PROG_SLOTS as u64, st, HostMem::Upload)?;
        let infos = [
            [vk::DescriptorBufferInfo { buffer: shbuf.buf, offset: 0, range: SH_SIZE }],
            [vk::DescriptorBufferInfo { buffer: pbuf.buf, offset: 0, range: PROG_BYTES }],
            [vk::DescriptorBufferInfo { buffer: vdbuf.buf, offset: 0, range: VDATA_RANGE }],
        ];
        let writes: Vec<vk::WriteDescriptorSet> = infos
            .iter()
            .enumerate()
            .map(|(b, i)| {
                vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(b as u32)
                    .descriptor_type(vk::DescriptorType::STORAGE_BUFFER_DYNAMIC)
                    .buffer_info(i)
            })
            .collect();
        d.update_descriptor_sets(&writes, &[]);
        Ok(VsGpu {
            playout,
            module,
            set,
            salign,
            sh_stride: align(SH_SIZE, salign),
            prog_stride,
            shbuf,
            sha: Arena::new(SHBUF_SIZE),
            vdbuf,
            vda: Arena::new(VDBUF_SIZE),
            ibuf,
            ia: Arena::new(IBUF_SIZE),
            pbuf,
            progs: ProgCache { by_hash: HashMap::new(), slots: vec![None; PROG_SLOTS], last: None },
        })
    }

    /// Vaga do programa do draw em `pbuf` (envia se ainda não está lá). Pode executar o lote antes, se todas as
    /// vagas estão em uso nele.
    /// Hash do conteúdo do programa carregado (código + descritores), recalculado só quando a geração muda.
    fn prog_hash(&mut self, d: &VsDraw) -> u64 {
        let g = self.vsg.as_mut().unwrap();
        match g.progs.last {
            Some((gen, h)) if gen == d.prog_gen => h,
            _ => {
                // SAFETY: fatias de u32 vistas como bytes
                let (cb, db) = unsafe {
                    (
                        std::slice::from_raw_parts(d.code.as_ptr() as *const u8, d.code.len() * 4),
                        std::slice::from_raw_parts(d.desc.as_ptr() as *const u8, d.desc.len() * 4),
                    )
                };
                let h = hash_bytes(cb) ^ hash_bytes(db).rotate_left(17).wrapping_mul(0x9E37_79B9_7F4A_7C15);
                g.progs.last = Some((d.prog_gen, h));
                h
            }
        }
    }

    /// Shader AOT para o programa do draw (None = ubershader). Na 1ª vez de cada (programa, entrada) procura na
    /// tabela registrada; programa sem AOT vai para o log uma vez (confere se o jogo carrega o mesmo que o `fix.xr`).
    unsafe fn aot_for(&mut self, d: &VsDraw) -> Result<Option<usize>, String> {
        if !aot::enabled() {
            return Ok(None);
        }
        let t = aot::table();
        let gen = t.as_ref().map_or(0, |t| t.gen);
        if gen != self.aot_gen {
            // outra tabela (só nos testes, que registram mais de uma): a lista de pipelines é de uma tabela só —
            // grava a atual antes de descartar os AOT e abre a da nova
            let h = t.as_ref().map_or(0, |t| t.hash);
            if h != self.keylog.aot_hash {
                self.save_pipeline_cache(false);
            }
            self.drop_aot()?;
            if h != self.keylog.aot_hash {
                self.keylog = KeyLog::open(h, t.as_ref().map_or(0, |t| t.progs.len() as u32));
            }
            self.aot_gen = gen;
            self.aot_mods = t.as_ref().map_or(Vec::new(), |t| vec![None; t.progs.len()]);
            self.aot_table = t;
        }
        let Some(t) = self.aot_table.clone() else { return Ok(None) };
        let entry = d.shading.misc[1];
        let hash = self.prog_hash(d);
        if let Some(&r) = self.aot_memo.get(&(hash, entry)) {
            return Ok(r);
        }
        let r = match aot::find(&t, d.code, d.desc, entry) {
            aot::Found::Aot(i) => Some(i),
            aot::Found::Miss(best) => {
                if self.aot_logged.len() < 64 && self.aot_logged.insert((entry, best)) {
                    let near = match best {
                        Some((i, n)) => format!("o mais próximo, {}, difere em {n} palavras", t.progs[i].name),
                        None => "nenhum AOT tem esta entrada".into(),
                    };
                    eprintln!("[vulkan] programa de vértice sem AOT: entrada {entry:#x}, hash {hash:016x} ({near}) — ubershader");
                }
                None
            }
        };
        self.aot_memo.insert((hash, entry), r);
        Ok(r)
    }

    /// Descarta módulos e pipelines da tabela AOT anterior (o lote em gravação é executado antes).
    unsafe fn drop_aot(&mut self) -> Result<(), String> {
        if self.aot_mods.iter().all(|m| m.is_none()) && self.aot_memo.is_empty() {
            return Ok(());
        }
        self.flush()?;
        // a pré-compilação que ainda não começou sai da fila (pode usar módulos que vão ser destruídos)
        self.cancel_warm();
        self.wait_compiles();
        self.wait_all()?;
        let _ = self.device.device_wait_idle();
        self.aot_failed.clear();
        let old: Vec<PipeKey> = self.pipelines.keys().filter(|k| k.aot != 0).copied().collect();
        for k in old {
            if let Some(e) = self.pipelines.remove(&k) {
                self.device.destroy_pipeline(e.p, None);
            }
        }
        for m in self.aot_mods.drain(..).flatten() {
            self.device.destroy_shader_module(m, None);
        }
        self.aot_memo.clear();
        Ok(())
    }

    /// Módulo do shader AOT `i` (criado no 1º uso).
    unsafe fn aot_module(&mut self, i: usize) -> Result<vk::ShaderModule, String> {
        if let Some(Some(m)) = self.aot_mods.get(i) {
            return Ok(*m);
        }
        let t = self.aot_table.clone().ok_or("tabela AOT ausente")?;
        let p = t.progs.get(i).ok_or("índice AOT fora da tabela")?;
        let m = vkr!(
            self.device.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&p.spirv), None),
            format!("vkCreateShaderModule(AOT {})", p.name)
        );
        self.aot_mods[i] = Some(m);
        Ok(m)
    }

    unsafe fn prog_slot(&mut self, d: &VsDraw) -> Result<usize, String> {
        let hash = self.prog_hash(d);
        let use_v = self.use_value();
        let g = self.vsg.as_mut().unwrap();
        if let Some(&s) = g.progs.by_hash.get(&hash) {
            g.progs.slots[s] = Some((hash, use_v));
            return Ok(s);
        }
        // vaga livre, senão a usada há mais tempo; se todas estão no lote em gravação, executa-o antes. A vaga escolhida
        // pode ainda estar em uso por um lote em voo: espera o valor dele antes de reescrevê-la
        let pick = |p: &ProgCache, recording: bool, use_v: u64| -> Option<(usize, u64)> {
            if let Some(i) = p.slots.iter().position(|s| s.is_none()) {
                return Some((i, 0));
            }
            let (i, (_, v)) = p.slots.iter().enumerate().map(|(i, s)| (i, s.unwrap())).min_by_key(|(_, (_, v))| *v)?;
            (!recording || v < use_v).then_some((i, v))
        };
        let (slot, last) = match pick(&g.progs, self.recording, use_v) {
            Some(x) => x,
            None => {
                self.flush()?;
                let g = self.vsg.as_mut().unwrap();
                pick(&g.progs, false, 0).unwrap()
            }
        };
        if last > self.done_v {
            self.wait_value(last)?;
        }
        let use_v = self.use_value();
        let g = self.vsg.as_mut().unwrap();
        if let Some((old, _)) = g.progs.slots[slot] {
            g.progs.by_hash.remove(&old);
        }
        g.progs.slots[slot] = Some((hash, use_v));
        g.progs.by_hash.insert(hash, slot);
        let dst = g.pbuf.ptr.add(slot * g.prog_stride as usize);
        std::ptr::copy_nonoverlapping(d.code.as_ptr() as *const u8, dst, d.code.len() * 4);
        std::ptr::copy_nonoverlapping(d.desc.as_ptr() as *const u8, dst.add(d.code.len() * 4), d.desc.len() * 4);
        Ok(slot)
    }

    fn mem_type(&self, bits: u32, want: vk::MemoryPropertyFlags) -> Option<u32> {
        (0..self.mem_props.memory_type_count).find(|&i| {
            bits >> i & 1 != 0 && self.mem_props.memory_types[i as usize].property_flags.contains(want)
        })
    }

    /// Buffer mapeado. `HostMem::Upload` (a CPU só escreve, a GPU lê): com ReBAR prefere DEVICE_LOCAL|HOST_VISIBLE —
    /// sem cache na CPU, então nunca LER desses ponteiros; `HostMem::Readback` (a CPU lê o que a GPU escreveu):
    /// HOST_CACHED. Se a alocação no tipo preferido falhar (janela de 256 MB sem ReBAR cheia), usa o host-visible comum.
    unsafe fn host_buf(&self, size: u64, usage: vk::BufferUsageFlags, kind: HostMem) -> Result<HostBuf, String> {
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
        let base = self.mem_type(req.memory_type_bits, hv).ok_or("sem memória host-visible")?;
        let pref = match kind {
            HostMem::Readback => self.mem_type(req.memory_type_bits, hv | vk::MemoryPropertyFlags::HOST_CACHED),
            // ReBAR de verdade (o heap do tipo DEVICE_LOCAL|HOST_VISIBLE tem ≥ 1 GB): todos os buffers de upload; só a
            // janela de 256 MB (sem ReBAR): só os pequenos (≤ 16 MB), para não esgotá-la
            HostMem::Upload if rebar() => self.mem_type(req.memory_type_bits, hv | vk::MemoryPropertyFlags::DEVICE_LOCAL).filter(|&t| {
                let heap = self.mem_props.memory_types[t as usize].heap_index as usize;
                self.mem_props.memory_heaps[heap].size >= 1 << 30 || size <= 16 << 20
            }),
            HostMem::Upload => None,
        };
        let alloc = |ty: u32| d.allocate_memory(&vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(ty), None);
        let mem = match pref.filter(|&t| t != base).map(alloc) {
            Some(Ok(m)) => m,
            _ => vkr!(alloc(base), "vkAllocateMemory(buffer)"),
        };
        vkr!(d.bind_buffer_memory(buf, mem, 0), "vkBindBufferMemory");
        let ptr = vkr!(d.map_memory(mem, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty()), "vkMapMemory") as *mut u8;
        HOST_BYTES.fetch_add(size, Ordering::Relaxed);
        Ok(HostBuf { buf, mem, ptr, size })
    }

    unsafe fn free_buf(&self, b: &HostBuf) {
        self.device.unmap_memory(b.mem);
        self.device.destroy_buffer(b.buf, None);
        self.device.free_memory(b.mem, None);
        HOST_BYTES.fetch_sub(b.size, Ordering::Relaxed);
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
        // superfície de profundidade: a vista cobre DEPTH|STENCIL (é o que o anexo de profundidade/estêncil pede)
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
        self.init_layout_on(self.cmd_pre, image, aspect);
    }

    /// UNDEFINED → GENERAL num buffer de comandos qualquer. Escopos de antes (TOP_OF_PIPE sem acesso → ALL_COMMANDS
    /// leitura/escrita); no sync2 o TOP_OF_PIPE da 1ª metade é o NONE.
    unsafe fn init_layout_on(&self, cb: vk::CommandBuffer, image: vk::Image, aspect: vk::ImageAspectFlags) {
        let b = [vk::ImageMemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::NONE)
            .src_access_mask(vk::AccessFlags2::NONE)
            .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .dst_access_mask(vk::AccessFlags2::MEMORY_READ | vk::AccessFlags2::MEMORY_WRITE)
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::GENERAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(image)
            .subresource_range(range(aspect))];
        self.device.cmd_pipeline_barrier2(cb, &vk::DependencyInfo::default().image_memory_barriers(&b));
    }

    /// Fecha a passada aberta em `cmd_shadow` (sombra N× / replay), se houver, e devolve a profundidade dela a GENERAL
    /// quando estava em ATTACHMENT_OPTIMAL. `true` = havia passada.
    pub(super) unsafe fn end_pass_s(&mut self) -> bool {
        if self.pass_s.take().is_none() {
            return false;
        }
        let cs = self.cmd_shadow;
        self.device.cmd_end_rendering(cs);
        let img = std::mem::replace(&mut self.pass_s_dimg, vk::Image::null());
        if img != vk::Image::null() {
            self.ds_layout(cs, img, vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL, vk::ImageLayout::GENERAL);
        }
        true
    }

    /// Troca de layout de uma imagem de profundidade/estêncil, com dependência total (tudo antes → tudo depois).
    unsafe fn ds_layout(&self, cb: vk::CommandBuffer, image: vk::Image, from: vk::ImageLayout, to: vk::ImageLayout) {
        let b = [vk::ImageMemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .dst_access_mask(vk::AccessFlags2::MEMORY_READ | vk::AccessFlags2::MEMORY_WRITE)
            .old_layout(from)
            .new_layout(to)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(image)
            .subresource_range(range(vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL))];
        self.device.cmd_pipeline_barrier2(cb, &vk::DependencyInfo::default().image_memory_barriers(&b));
    }

    /// Barreira de memória global (sync2) com os escopos dados.
    unsafe fn mem_barrier(
        &self,
        cb: vk::CommandBuffer,
        src: (vk::PipelineStageFlags2, vk::AccessFlags2),
        dst: (vk::PipelineStageFlags2, vk::AccessFlags2),
    ) {
        let m = [vk::MemoryBarrier2::default().src_stage_mask(src.0).src_access_mask(src.1).dst_stage_mask(dst.0).dst_access_mask(dst.1)];
        self.device.cmd_pipeline_barrier2(cb, &vk::DependencyInfo::default().memory_barriers(&m));
    }

    unsafe fn full_barrier(&self, cb: vk::CommandBuffer) {
        self.mem_barrier(
            cb,
            (vk::PipelineStageFlags2::ALL_COMMANDS, vk::AccessFlags2::MEMORY_WRITE),
            (vk::PipelineStageFlags2::ALL_COMMANDS, vk::AccessFlags2::MEMORY_READ | vk::AccessFlags2::MEMORY_WRITE),
        );
    }

    /// Abre uma passada (dynamic rendering) em `cb`: cor `cv` + profundidade/estêncil `dv` (o D32S8 vai nos dois
    /// anexos), LOAD/STORE — o mesmo que o VkRenderPass único de antes fazia. Cor em GENERAL; profundidade no layout
    /// `dl` (GENERAL, ou ATTACHMENT_OPTIMAL na sombra/replay com `dopt`) e, com `dclear`, limpa pelo loadOp CLEAR
    /// (profundidade, estêncil) em vez de carregar.
    unsafe fn begin_pass(
        &self,
        cb: vk::CommandBuffer,
        cv: vk::ImageView,
        dv: vk::ImageView,
        w: u32,
        h: u32,
        dl: vk::ImageLayout,
        dclear: Option<(f32, u32)>,
    ) {
        let c = [vk::RenderingAttachmentInfo::default()
            .image_view(cv)
            .image_layout(vk::ImageLayout::GENERAL)
            .load_op(vk::AttachmentLoadOp::LOAD)
            .store_op(vk::AttachmentStoreOp::STORE)];
        let (load, (depth, stencil)) = match dclear {
            Some(c) => (vk::AttachmentLoadOp::CLEAR, c),
            None => (vk::AttachmentLoadOp::LOAD, (0.0, 0)),
        };
        let z = vk::RenderingAttachmentInfo::default()
            .image_view(dv)
            .image_layout(dl)
            .load_op(load)
            .store_op(vk::AttachmentStoreOp::STORE)
            .clear_value(vk::ClearValue { depth_stencil: vk::ClearDepthStencilValue { depth, stencil } });
        let ri = vk::RenderingInfo::default()
            .render_area(vk::Rect2D { offset: vk::Offset2D { x: 0, y: 0 }, extent: vk::Extent2D { width: w, height: h } })
            .layer_count(1)
            .color_attachments(&c)
            .depth_attachment(&z)
            .stencil_attachment(&z);
        self.device.cmd_begin_rendering(cb, &ri);
    }

    /// Envia `cbs` (nessa ordem, num só envio) pela vaga `slot`, sinalizando o próximo valor do semáforo de timeline.
    /// Não espera (quem precisa do resultado chama `wait_value`). Só avança `tl_value` se o envio deu certo. As
    /// reservas dos anéis de upload do lote ficam presas a esse valor.
    unsafe fn submit(&mut self, cbs: &[vk::CommandBuffer], slot: usize, kind: SubKind, what: &str) -> Result<u64, String> {
        let v = self.tl_value + 1;
        let cbi: Vec<vk::CommandBufferSubmitInfo> = cbs.iter().map(|&c| vk::CommandBufferSubmitInfo::default().command_buffer(c)).collect();
        let sig = [vk::SemaphoreSubmitInfo::default().semaphore(self.timeline).value(v).stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)];
        let si = [vk::SubmitInfo2::default().command_buffer_infos(&cbi).signal_semaphore_infos(&sig)];
        vkr!(self.device.queue_submit2(self.queue, &si, vk::Fence::null()), format!("vkQueueSubmit2({what})"));
        self.tl_value = v;
        self.slots[slot].value = v;
        self.subs.push_back(Sub { value: v, slot, kind });
        for a in [Ar::U, Ar::R, Ar::V, Ar::Sh, Ar::Vd, Ar::I] {
            if let Some(a) = self.arena(a) {
                a.submit(v);
            }
        }
        Ok(v)
    }

    /// Valor de timeline do próximo envio: o do lote em gravação (nada mais é enviado enquanto ele grava) ou, sem lote
    /// aberto, um valor que vem depois de tudo o que já foi enviado. Marca o uso de recursos (vagas de programa,
    /// destruição adiada).
    fn use_value(&self) -> u64 {
        self.tl_value + 1
    }

    /// Espera a GPU passar do valor `v` (no máximo o último enviado) e recolhe o que terminou.
    pub(super) unsafe fn wait_value(&mut self, v: u64) -> Result<(), String> {
        let v = v.min(self.tl_value);
        if v > self.done_v {
            let t0 = std::time::Instant::now();
            let sems = [self.timeline];
            let vals = [v];
            vkr!(
                self.device.wait_semaphores(&vk::SemaphoreWaitInfo::default().semaphores(&sems).values(&vals), u64::MAX),
                "vkWaitSemaphores"
            );
            self.stats[5] += t0.elapsed().as_micros() as u64;
            self.done_v = v;
        }
        self.retire();
        Ok(())
    }

    /// Anel de upload pequeno para `need` bytes num draw: executa o lote aberto, espera a GPU (nada mais lê o anel) e
    /// troca por um maior (potência de 2). Raro: só um draw com texturas enormes ou a sombra N× carregada da memória.
    unsafe fn grow_ring(&mut self, need: u64) -> Result<(), String> {
        if self.recording {
            self.flush()?;
        }
        self.wait_all()?;
        let novo = need.next_power_of_two().max(RING_SIZE);
        let buf = self.host_buf(novo, vk::BufferUsageFlags::TRANSFER_SRC, HostMem::Upload)?;
        let old = std::mem::replace(&mut self.ring, buf);
        eprintln!("[vulkan] anel de upload: {} → {} MB", old.size >> 20, novo >> 20);
        self.free_buf(&old);
        let (pl, pt) = (self.ra.pk_lote, self.ra.pk_total);
        self.ra = Arena::new(novo);
        (self.ra.pk_lote, self.ra.pk_total) = (pl, pt);
        Ok(())
    }

    /// Espera todos os envios.
    pub(super) unsafe fn wait_all(&mut self) -> Result<(), String> {
        self.wait_value(self.tl_value)
    }

    /// Lê o contador do semáforo (sem esperar) e recolhe o que terminou.
    pub(super) unsafe fn poll(&mut self) {
        if self.tl_value > self.done_v {
            if let Ok(c) = self.device.get_semaphore_counter_value(self.timeline) {
                self.done_v = self.done_v.max(c.min(self.tl_value));
            }
        }
        self.retire();
    }

    /// Recolhe os envios até `done_v`: timestamps (estatísticas, perfil por draw, tempo de GPU da interpolação),
    /// destruições adiadas e as partes dos anéis de upload.
    unsafe fn retire(&mut self) {
        let done = self.done_v;
        while let Some(s) = self.subs.front().copied() {
            if s.value > done {
                break;
            }
            self.subs.pop_front();
            self.collect_ts(s);
        }
        while self.deferred.front().is_some_and(|(v, _)| *v <= done) {
            match self.deferred.pop_front().unwrap().1 {
                Res::Img(i) => self.free_img(&i),
                Res::Buf(b) => self.free_buf(&b),
            }
        }
        for a in [Ar::U, Ar::R, Ar::V, Ar::Sh, Ar::Vd, Ar::I] {
            if let Some(a) = self.arena(a) {
                a.retire(done);
            }
        }
    }

    /// Timestamps de um envio que terminou.
    unsafe fn collect_ts(&mut self, s: Sub) {
        let d = self.device.clone();
        let base = s.slot as u32 * QS;
        let period = self.ts_period as f64;
        let tick = |a: u64, b: u64| -> u64 { (b.saturating_sub(a) as f64 * period) as u64 };
        match s.kind {
            SubKind::Batch { nts, mb } => {
                let mut ts = [0u64; 3];
                let n = nts as usize;
                if d.get_query_pool_results(self.ts_pool, base, &mut ts[..n], vk::QueryResultFlags::TYPE_64).is_ok()
                    && ts[1] >= ts[0]
                    && (n < 3 || ts[2] >= ts[1])
                {
                    let (native, shadow) = (tick(ts[0], ts[1]), if n == 3 { tick(ts[1], ts[2]) } else { 0 });
                    self.stats[13] += native / 1000;
                    self.stats[14] += shadow / 1000;
                    // num lote de intermediários a sombra é deles (abaixo); o resto é a cena
                    self.gpu_work_ns += if mb > 1 { native } else { native + shadow };
                }
                if mb > 1 {
                    let mut mt = [0u64; (TS_MID_END - TS_MID) as usize];
                    let m = (mb as usize).min(mt.len());
                    if d.get_query_pool_results(self.ts_pool, base + TS_MID, &mut mt[..m], vk::QueryResultFlags::TYPE_64).is_ok() {
                        self.gpu_mid_ns += tick(mt[0], mt[m - 1]);
                        self.gpu_mid_n += m as u64 - 1;
                    }
                }
                if let Some(p) = self.prof.as_mut() {
                    p.collect(&d, s.slot, ts[0], self.ts_period);
                }
            }
            SubKind::Present => {
                let mut ts = [0u64; 2];
                if d.get_query_pool_results(self.ts_pool, base + TS_PRESENT, &mut ts, vk::QueryResultFlags::TYPE_64).is_ok() {
                    self.gpu_work_ns += tick(ts[0], ts[1]);
                }
            }
        }
    }

    /// Anel de upload `a` (None = o do shader de vértice na GPU, ainda não criado).
    /// (anel, tamanho, maior lote, maior ocupação) de cada anel de upload — `arena_peaks`.
    fn arena_peaks(&mut self) -> Vec<(&'static str, u64, u64, u64)> {
        [(Ar::U, "ubuf"), (Ar::R, "ring"), (Ar::V, "vbuf"), (Ar::Sh, "shbuf"), (Ar::Vd, "vdbuf"), (Ar::I, "ibuf")]
            .into_iter()
            .filter_map(|(a, n)| self.arena(a).map(|x| (n, x.size, x.pk_lote, x.pk_total)))
            .collect()
    }

    fn arena(&mut self, a: Ar) -> Option<&mut Arena> {
        match a {
            Ar::U => Some(&mut self.ua),
            Ar::R => Some(&mut self.ra),
            Ar::V => Some(&mut self.va),
            Ar::Sh => self.vsg.as_mut().map(|g| &mut g.sha),
            Ar::Vd => self.vsg.as_mut().map(|g| &mut g.vda),
            Ar::I => self.vsg.as_mut().map(|g| &mut g.ia),
        }
    }

    /// Reserva no anel `a` (ver `Arena::try_alloc`), esperando os lotes em voo que ocupam o espaço. None = não cabe nem
    /// com tudo livre (o lote em gravação já ocupa demais).
    unsafe fn alloc(&mut self, a: Ar, n: u64, win: u64, al: u64) -> Result<Option<u64>, String> {
        loop {
            let ar = self.arena(a).ok_or("anel de upload ausente")?;
            if let Some(o) = ar.try_alloc(n, win, al) {
                return Ok(Some(o));
            }
            match ar.oldest() {
                Some(v) => self.wait_value(v)?,
                None => return Ok(None),
            }
        }
    }

    /// Destrói `r` quando a GPU terminar tudo o que pode estar usando-o (o lote em gravação ou já enviado).
    pub(super) fn defer(&mut self, r: Res) {
        let v = self.use_value();
        self.deferred.push_back((v, r));
    }

    /// Modo dos próximos envios (assíncrono ou síncrono, `async_eff`). Se mudou: espera tudo o que já foi enviado e a
    /// publicação em andamento antes de trocar.
    pub(super) unsafe fn mode_check(&mut self) -> Result<bool, String> {
        let a = async_eff();
        if a != self.async_mode {
            self.wait_all()?;
            self.pub_idle();
            self.async_mode = a;
        }
        Ok(a)
    }

    /// Próxima vaga do anel de envios (espera o envio anterior dela, se ainda estiver na GPU).
    pub(super) unsafe fn take_slot(&mut self) -> Result<usize, String> {
        self.poll();
        let s = (self.slot + 1) % self.slots.len();
        let v = self.slots[s].value;
        if v > self.done_v {
            self.wait_value(v)?;
        }
        self.slot = s;
        Ok(s)
    }

    unsafe fn begin(&mut self) -> Result<(), String> {
        if self.recording {
            return Ok(());
        }
        self.mode_check()?;
        let s = self.take_slot()?;
        let (pre, main, shadow) = (self.slots[s].pre, self.slots[s].main, self.slots[s].shadow);
        (self.cmd_pre, self.cmd_main, self.cmd_shadow) = (pre, main, shadow);
        self.ts_base = s as u32 * QS;
        self.mid_b = 0;
        let d = &self.device;
        let bi = vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        for cb in [self.cmd_pre, self.cmd_main, self.cmd_shadow] {
            vkr!(d.reset_command_buffer(cb, vk::CommandBufferResetFlags::empty()), "vkResetCommandBuffer");
            vkr!(d.begin_command_buffer(cb, &bi), "vkBeginCommandBuffer");
        }
        // barreira de entrada: os envios anteriores (mesma fila, possivelmente ainda na GPU) terminam antes dos
        // clears/cópias/resets deste (achado da revisão da F2)
        self.full_barrier(self.cmd_pre);
        d.cmd_reset_query_pool(self.cmd_pre, self.ts_pool, self.ts_base, QS);
        d.cmd_write_timestamp(self.cmd_pre, vk::PipelineStageFlags::TOP_OF_PIPE, self.ts_pool, self.ts_base);
        if let Some(p) = self.prof.as_mut() {
            p.reset(d, self.cmd_pre, s);
        }
        // tudo o que os uploads (buffer `pre`, enviado antes) escreverem fica visível para os draws
        self.full_barrier(self.cmd_main);
        self.full_barrier(self.cmd_shadow);
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

    /// Reserva `n` bytes no anel de upload (alinhado a 16; pode esperar lotes em voo que ocupam o espaço).
    unsafe fn ring_alloc(&mut self, n: u64) -> Option<u64> {
        match self.alloc(Ar::R, n, n, 16) {
            Ok(o) => o,
            Err(e) => {
                self.warn(&format!("anel de upload: {e}"));
                None
            }
        }
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

    /// Grava um draw. `Ok(false)` só para `Geo::Shaded` que não cabe no caminho da GPU (nada foi gravado: o chamador
    /// usa os vértices da CPU).
    unsafe fn draw(&mut self, ctx: &Ctx, geo: Geo) -> Result<bool, String> {
        // resolução interna / FXAA ligados, desligados ou com outro fator: refaz as sombras (executa o lote antes)
        let replay = self.replay;
        if replay.is_none() {
            self.hires_sync_config()?;
        }
        let (w, h) = (ctx.width, ctx.height_m1 + 1);
        // bytes por draw: vértices prontos (vbuf) · índices (ibuf) · arranjos de vértices crus (vdbuf)
        let (vbytes, ibytes, vdbytes) = match geo {
            Geo::Placed(verts) => {
                if verts.is_empty() {
                    return Ok(true);
                }
                let vb = verts.len() as u64 * VTX_STRIDE as u64;
                if vb > VBUF_SIZE {
                    self.warn(&format!("draw com {} vértices ignorado (buffer pequeno)", verts.len()));
                    return Ok(true);
                }
                // 1º draw com vértices da CPU: cria o buffer (com o shader de vértice na GPU ele nunca é usado)
                if self.vbuf.size == 0 {
                    self.vbuf = self.host_buf(VBUF_SIZE, vk::BufferUsageFlags::VERTEX_BUFFER, HostMem::Upload)?;
                    self.va = Arena::new(VBUF_SIZE);
                }
                (vb, 0, 0)
            }
            Geo::Shaded(d) => {
                if !self.vsg_ready() {
                    return Ok(false);
                }
                let ib = d.indices.len() as u64 * 4;
                let vd: u64 = d.spans.iter().map(|s| s.1 as u64).sum();
                // viewport do PICA = viewport do Vulkan: tem de ser positivo e dentro dos limites do dispositivo
                let vp = ctx.vp;
                let vp_ok = vp.iter().all(|x| x.is_finite())
                    && vp[0] > 0.0
                    && vp[1] > 0.0
                    && vp[0] <= 2048.0
                    && vp[1] <= 2048.0
                    && vp[2].abs() <= 4096.0
                    && vp[3].abs() <= 4096.0;
                if ib > IBUF_SIZE || vd > VDATA_RANGE || !vp_ok {
                    return Ok(false);
                }
                if d.indices.is_empty() {
                    return Ok(true);
                }
                // o trecho reservado tem pelo menos 4 bytes (draw sem arranjos)
                (0, ib, vd.max(4))
            }
        };
        let shaded = matches!(geo, Geo::Shaded(_));
        if w == 0 || h == 0 || w > 1024 || h > 1024 {
            return Ok(true);
        }
        let (ck, dk) = match replay {
            Some(r) => (r.ck, r.dk),
            None => (
                SurfKey { addr: ctx.color_addr, w, h, fmt: ctx.color_fmt, depth: false },
                SurfKey { addr: ctx.depth_addr, w, h, fmt: ctx.depth_fmt, depth: true },
            ),
        };
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
            + if replay.is_none() && self.hr.on && shadow_eligible(ck) { w as u64 * h as u64 * (4 + 5) * (self.hr.scale as u64).pow(2) } else { 0 }
            + 32;
        if ring_need > self.ring.size {
            self.grow_ring(ring_need)?;
        }
        // descarrega antes se: falta espaço, ou o draw lê/usa memória que outra superfície do lote sujou. "Espaço" = o
        // que o lote em gravação ainda tem nos anéis de upload (`Arena::fits`; no modo síncrono, a conta de antes)
        if self.recording {
            let vs_full = shaded
                && self.vsg.as_ref().is_some_and(|g| {
                    !g.sha.fits(g.sh_stride, g.sh_stride, g.salign)
                        || !g.vda.fits(vdbytes, VDATA_RANGE, g.salign)
                        || !g.ia.fits(ibytes, ibytes, 16)
                });
            let full = (vbytes > 0 && !self.va.fits(vbytes, vbytes, 16))
                || vs_full
                || !self.ua.fits(self.ustride, self.ustride, self.ustride)
                || !self.ra.fits(ring_need, ring_need, 16);
            // replay da interpolação: as texturas vêm do cache (versões capturadas), não da memória
            let tex_hazard = replay.is_none() && texs.iter().any(|(_, t)| {
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
        // texturas e alvos leem a memória: o que só está na GPU e toca essas faixas volta antes (no replay da
        // interpolação as texturas não são lidas da memória: usam as versões capturadas com o desenho)
        if replay.is_none() {
            for (_, t) in &texs {
                let n = t.w * t.h * texture::bits_per_texel(t.format) / 8;
                self.sync_range(t.addr, t.addr + n, false)?;
            }
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
        // shader AOT do programa (se houver e o pipeline já estiver pronto: aí não precisa enviar o programa). Antes
        // da vaga: trocar de tabela AOT pode executar o lote.
        let mut key = self.pipe_key(ctx, shaded);
        let aot = match geo {
            Geo::Shaded(d) => self.aot_for(d)?,
            Geo::Placed(_) => None,
        };
        let aot = match aot {
            Some(i) => {
                let ka = PipeKey { aot: i as u32 + 1, ..key };
                if self.aot_ready(ka)? {
                    Some(i)
                } else {
                    self.stats[11] += 1;
                    None
                }
            }
            None => None,
        };
        // programa por último: a vaga é marcada com o lote atual, e daqui até gravar o draw nada mais executa o lote
        let slot = match geo {
            Geo::Shaded(d) if aot.is_none() => Some(self.prog_slot(d)?),
            _ => None,
        };
        self.begin()?;

        let mut views = [self.dummy.view; 3];
        // unidades cuja memória não está mapeada: `texture` devolve o dummy 1×1, então o shader não pode usar o
        // tamanho real (texelFetch fora da imagem é indefinido sem robustImageAccess)
        let mut missing = [false; 3];
        // chaves (versões) das texturas efetivamente usadas: vão para a captura da interpolação
        let mut tex_keys: [Option<TexKey>; 3] = [None; 3];
        for (i, t) in &texs {
            let (view, key) = if replay.is_some() {
                // replay: exatamente a versão capturada, do cache (`tex_pinned` a protege do `evict`); nada de
                // `texture()` — ele relê a memória do jogo (assíncrona a esta thread) e grava o `tex_hash`
                match self.replay_tex.and_then(|r| r[*i]) {
                    Some(k) => match self.textures.get_mut(&k) {
                        Some(e) => {
                            e.last_used = self.batch;
                            (e.img.view, Some(k))
                        }
                        None => return Err(format!("interpolação: textura {:#x} {}x{} fora do cache", t.addr, t.w, t.h)),
                    },
                    None => (self.dummy.view, None),
                }
            } else {
                self.texture(t)?
            };
            views[*i] = view;
            tex_keys[*i] = key;
            missing[*i] = view == self.dummy.view;
        }
        let (cview, dview) = match replay {
            Some(r) => (r.cview, r.dview),
            None => (self.surface(ck)?, self.surface(dk)?),
        };
        // sombra N× dos alvos de tela (resolução interna): recebe os mesmos draws; na re-renderização da interpolação o
        // alvo é a imagem de re-renderização (e só ela)
        let shadow_views = match replay {
            Some(r) => Some((r.cview, r.dview)),
            None if self.hr.on && shadow_eligible(ck) => Some((self.shadow(ck)?, self.shadow(dk)?)),
            None => None,
        };
        key.aot = aot.map_or(0, |i| i as u32 + 1);
        let mut u = uniforms(ctx);
        if shaded {
            u.depth[3] = super::DEPTH_ZNDC | if ctx.wbuffer { super::DEPTH_WBUF } else { 0 };
        }
        for (i, m) in missing.iter().enumerate() {
            if *m {
                // largura/altura 0 → o shader devolve (0, 0, 0, 255), como `texture::sample` sem textura
                u.tex[i][0] = 0;
            }
        }
        // fragmento especializado: compila em segundo plano; até ficar pronto, o ubershader (mesmo resultado)
        if fs_spec() {
            let ks = PipeKey { fs: fs_key(&u), ..key };
            if self.aot_ready(ks)? {
                key = ks;
                self.stats[15] += 1;
            } else {
                self.stats[16] += 1;
            }
        }
        let pipe = self.pipeline(key)?;
        // com sombra N×, quem vai para a tela é a sombra: a passada nativa (memória do jogo) fica sem o filtro
        // anisotrópico, que não existe no PICA — sai igual ao hardware e mais barata. Só com o fragmento especializado
        // (no ubershader o filtro vem do uniform, comum às duas passadas).
        let aniso_bits = 15 << super::F_ANISO_SHIFT;
        let pipe_native = if replay.is_none() && shadow_views.is_some() && key.fs.spec && key.fs.flags & aniso_bits != 0 {
            let kn = PipeKey { fs: FsKey { flags: key.fs.flags & !aniso_bits, ..key.fs }, ..key };
            if self.aot_ready(kn)? {
                self.pipeline(kn)?
            } else {
                pipe
            }
        } else {
            pipe
        };
        // profundidade rápida (F4) nas passadas que não voltam para a memória do jogo — sombra N× e replay da
        // interpolação: fragmento sem gl_FragDepth (teste antecipado de profundidade) e a escala do PICA na faixa do
        // viewport. Compila em segundo plano; até ficar pronto, o pipeline exato. Com o fragmento especializado ligado
        // e ainda compilando, também o exato (não compila um "ubershader rápido" de passagem).
        let mut fz = if shadow_views.is_some() && fastz() && (key.fs.spec || !fs_spec()) { fastz_range(ctx, &u, &key) } else { None };
        let pipe_shadow = match fz {
            Some(_) => {
                let kf = PipeKey { fastz: true, ..key };
                if self.aot_ready(kf)? {
                    self.stats[18] += 1;
                    self.pipeline(kf)?
                } else {
                    self.stats[19] += 1;
                    fz = None;
                    pipe
                }
            }
            None => pipe,
        };
        if replay.is_none() && interp::capturing() && shadow_eligible(ck) {
            if let Geo::Shaded(dr) = geo {
                // as versões (chaves) das texturas que este draw usou: o replay usa exatamente essas, e o conteúdo
                // delas entra na identidade do desenho (vídeo, textura animada ou renderizada muda de um quadro para o
                // outro e tira o desenho do pareamento)
                if self.ip.record(ctx, key, aot, dr, tex_keys) {
                    self.tex_pinned.extend(tex_keys.iter().flatten().copied());
                }
            }
        }

        // passada: troca de alvo fecha a anterior (com barreira global entre elas)
        let d = self.device.clone();
        let cb = self.cmd_main;
        let pass_break = replay.is_none() && self.pass != Some((ck, dk));
        if replay.is_none() && self.pass != Some((ck, dk)) {
            if self.pass.is_some() {
                d.cmd_end_rendering(cb);
                self.full_barrier(cb);
            }
            self.begin_pass(cb, cview, dview, w, h, vk::ImageLayout::GENERAL, None);
            self.pass = Some((ck, dk));
        }
        if shadow_views.is_some() {
            if replay.is_some() {
                self.shadow_other = true;
            } else {
                if !self.shadow_drawn.contains(&(ck, dk)) {
                    self.shadow_drawn.push((ck, dk));
                }
                if let Some(sh) = self.surfaces.get_mut(&ck).and_then(|s| s.shadow.as_mut()) {
                    sh.wgen += 1;
                }
            }
        }
        if let Some((scv, sdv)) = shadow_views {
            // FXAA só na cena: o primeiro draw sem teste de profundidade depois de draws de cena fecha a cena
            self.aa_boundary(ck, key.dtest && key.mask != 0, key.mask != 0)?;
            let cs = self.cmd_shadow;
            let n = self.hr.scale;
            if self.pass_s != Some((ck, dk)) {
                if self.end_pass_s() {
                    self.full_barrier(cs);
                }
                let mut dl = vk::ImageLayout::GENERAL;
                let mut dclear = None;
                if dopt() {
                    let dimg = match replay {
                        Some(r) => Some(r.dimage),
                        None => self.surfaces.get(&dk).and_then(|s| s.shadow.as_ref()).map(|s| s.img.image),
                    };
                    if let Some(img) = dimg {
                        dl = vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL;
                        self.ds_layout(cs, img, vk::ImageLayout::GENERAL, dl);
                        self.pass_s_dimg = img;
                        // 1ª passada do replay: o clear inicial da profundidade vai no loadOp (limpa no layout ótimo,
                        // com a compressão/HiZ inicializados), não por vkCmdClearDepthStencilImage em GENERAL
                        if replay.is_some() {
                            dclear = self.replay_dclear.take();
                        }
                    }
                }
                self.begin_pass(cs, scv, sdv, w * n, h * n, dl, dclear);
                self.pass_s = Some((ck, dk));
            }
        }

        // scissor de inclusão (modo 3) = retângulo do Vulkan; exclusão (modo 1) fica no shader
        let (smode, sc) = ctx.scissor;
        let (mut x0, mut y0, mut x1, mut y1) = (0u32, 0u32, w, h);
        if smode == 3 {
            x0 = sc[0].min(w);
            y0 = sc[1].min(h);
            x1 = (sc[2] + 1).min(w);
            y1 = (sc[3] + 1).min(h);
        }
        // vértices na GPU: o viewport do Vulkan é o do PICA (o recorte em x/y do PICA é o do hardware); o scissor
        // também fica dentro dele, para implementações com "guard band" não desenharem fora
        let viewport = if shaded {
            let vp = ctx.vp;
            let clampu = |v: f32, hi: u32| v.max(0.0).min(hi as f32) as u32;
            x0 = x0.max(clampu(vp[2].floor(), w));
            y0 = y0.max(clampu(vp[3].floor(), h));
            x1 = x1.min(clampu((vp[2] + 2.0 * vp[0]).ceil(), w));
            y1 = y1.min(clampu((vp[3] + 2.0 * vp[1]).ceil(), h));
            vk::Viewport { x: vp[2], y: vp[3], width: 2.0 * vp[0], height: 2.0 * vp[1], min_depth: 0.0, max_depth: 1.0 }
        } else {
            vk::Viewport { x: 0.0, y: 0.0, width: w as f32, height: h as f32, min_depth: 0.0, max_depth: 1.0 }
        };
        if x1 <= x0 || y1 <= y0 {
            return Ok(true);
        }

        // geometria
        let mut voff = 0u64;
        let mut vs_offs = [0u32; 3];
        let mut ioff = 0u64;
        // reservas nos anéis de upload (podem esperar lotes em voo; None não acontece depois da conferência de "cheio")
        let full_err = |what: &str| format!("anel de upload {what} cheio depois da conferência");
        match geo {
            Geo::Placed(verts) => {
                voff = self.alloc(Ar::V, vbytes, vbytes, 16)?.ok_or_else(|| full_err("de vértices"))?;
                std::ptr::copy_nonoverlapping(verts.as_ptr() as *const u8, self.vbuf.ptr.add(voff as usize), vbytes as usize);
            }
            Geo::Shaded(dr) => {
                let (sh_stride, salign) = {
                    let g = self.vsg.as_ref().unwrap();
                    (g.sh_stride, g.salign)
                };
                let shoff = self.alloc(Ar::Sh, sh_stride, sh_stride, salign)?.ok_or_else(|| full_err("de parâmetros"))?;
                let vdoff = self.alloc(Ar::Vd, vdbytes, VDATA_RANGE, salign)?.ok_or_else(|| full_err("de vértices crus"))?;
                ioff = self.alloc(Ar::I, ibytes, ibytes, 16)?.ok_or_else(|| full_err("de índices"))?;
                let g = self.vsg.as_mut().unwrap();
                // parâmetros
                std::ptr::copy_nonoverlapping(
                    dr.shading as *const VsShading as *const u8,
                    g.shbuf.ptr.add(shoff as usize),
                    SH_SIZE as usize,
                );
                // arranjos de vértices: trechos da memória guest um atrás do outro (cada um múltiplo de 4 bytes)
                let mut pos = vdoff;
                for (k, &(a, n)) in dr.spans.iter().enumerate() {
                    let src = match dr.data {
                        Some(d) => d.get(k).copied(),
                        None => guest_slice(a, n),
                    };
                    if let Some(src) = src {
                        std::ptr::copy_nonoverlapping(src.as_ptr(), g.vdbuf.ptr.add(pos as usize), (n as usize).min(src.len()));
                    }
                    pos += n as u64;
                }
                // índices (u32, lista de triângulos)
                std::ptr::copy_nonoverlapping(
                    dr.indices.as_ptr() as *const u8,
                    g.ibuf.ptr.add(ioff as usize),
                    ibytes as usize,
                );
                // AOT: o programa não é lido (binding 1 fica na vaga 0, qualquer uma serve)
                vs_offs = [shoff as u32, (slot.unwrap_or(0) as u64 * g.prog_stride) as u32, vdoff as u32];
            }
        }
        let ust = self.ustride;
        let uoff = self.alloc(Ar::U, ust, ust, ust)?.ok_or_else(|| full_err("de uniforms"))? as u32;
        std::ptr::copy_nonoverlapping(&u as *const DrawUniforms as *const u8, self.ubuf.ptr.add(uoff as usize), UBO_SIZE as usize);

        // set 0 em push descriptor (gravado em cada buffer de comandos do draw): UBO no deslocamento deste draw + as
        // 3 texturas
        let binfo = [vk::DescriptorBufferInfo { buffer: self.ubuf.buf, offset: uoff as u64, range: UBO_SIZE }];
        let iinfo: [[vk::DescriptorImageInfo; 1]; 3] =
            views.map(|v| [vk::DescriptorImageInfo { sampler: self.sampler, image_view: v, image_layout: vk::ImageLayout::GENERAL }]);
        let img_write = |i: usize| {
            vk::WriteDescriptorSet::default()
                .dst_binding(1 + i as u32)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(&iinfo[i])
        };
        let writes = [
            vk::WriteDescriptorSet::default().dst_binding(0).descriptor_type(vk::DescriptorType::UNIFORM_BUFFER).buffer_info(&binfo),
            img_write(0),
            img_write(1),
            img_write(2),
        ];
        let pd = self.pd.clone();

        // o mesmo draw no buffer nativo (n = 1) e, com resolução interna, na sombra (n = N): viewport e scissor × n.
        // O 3º campo marca a passada nativa (memória do jogo): pipeline `pipe_native`, nunca o rápido
        let mut targets: Vec<(vk::CommandBuffer, u32, bool)> = if replay.is_some() { Vec::new() } else { vec![(cb, 1, true)] };
        if shadow_views.is_some() {
            targets.push((self.cmd_shadow, self.hr.scale, false));
        }
        let prof_meta = |ns: u32| super::drawprof::Meta {
            ns,
            w,
            h,
            prims: match geo {
                Geo::Placed(v) => v.len() as u32 / 3,
                Geo::Shaded(dr) => dr.indices.len() as u32 / 3,
            },
            aot: aot.is_some(),
            shaded,
            dtest: key.dtest,
            dwrite: key.dwrite,
            dfunc: key.dfunc,
            blend: key.blend,
            mask: key.mask,
            stencil: key.stencil,
            aniso: (u.misc[3] >> super::F_ANISO_SHIFT) & 15,
            linear: (0..3).filter(|&i| u.tex[i][0] != 0 && (u.tex[i][1] & 0x100) != 0).count() as u32,
            ntex: (0..3).filter(|&i| u.tex[i][0] != 0).count() as u32,
            wbuf: ctx.wbuffer,
            alpha_test: (u.misc[2] & 1) != 0,
            color: ck.addr,
            pass_break,
        };
        for &(cb, ns, native) in &targets {
            let f = ns as f32;
            d.cmd_bind_pipeline(cb, vk::PipelineBindPoint::GRAPHICS, if native { pipe_native } else { pipe_shadow });
            // profundidade rápida: a escala do PICA vai na faixa do viewport (`fastz_range`); a nativa fica com 0..1
            let (min_depth, max_depth) = match fz {
                Some(r) if !native => r,
                _ => (viewport.min_depth, viewport.max_depth),
            };
            d.cmd_set_viewport(
                cb,
                0,
                &[vk::Viewport { x: viewport.x * f, y: viewport.y * f, width: viewport.width * f, height: viewport.height * f, min_depth, max_depth }],
            );
            d.cmd_set_scissor(
                cb,
                0,
                &[vk::Rect2D {
                    offset: vk::Offset2D { x: (x0 * ns) as i32, y: (y0 * ns) as i32 },
                    extent: vk::Extent2D { width: (x1 - x0) * ns, height: (y1 - y0) * ns },
                }],
            );
            d.cmd_set_blend_constants(cb, &ctx.blend_color.map(|c| c as f32 / 255.0));
            let (_, _, st_wmask, st_ref, st_imask, _) = ctx.stencil;
            let ff = vk::StencilFaceFlags::FRONT_AND_BACK;
            d.cmd_set_stencil_reference(cb, ff, st_ref as u32);
            d.cmd_set_stencil_compare_mask(cb, ff, st_imask as u32);
            d.cmd_set_stencil_write_mask(cb, ff, st_wmask as u32);
            let which = if native { 0 } else { 1 };
            let q = match self.prof.as_mut() {
                Some(p) if replay.is_none() => p.before(&d, cb, which),
                _ => None,
            };
            match geo {
                Geo::Placed(verts) => {
                    pd.cmd_push_descriptor_set(cb, vk::PipelineBindPoint::GRAPHICS, self.playout, 0, &writes);
                    d.cmd_push_constants(cb, self.playout, vk::ShaderStageFlags::FRAGMENT, 0, &ns.to_le_bytes());
                    d.cmd_bind_vertex_buffers(cb, 0, &[self.vbuf.buf], &[voff]);
                    d.cmd_draw(cb, verts.len() as u32, 1, 0, 0);
                }
                Geo::Shaded(dr) => {
                    let g = self.vsg.as_ref().unwrap();
                    // set 1 (único, dinâmico) com os deslocamentos deste draw; set 0 empurrado
                    d.cmd_bind_descriptor_sets(cb, vk::PipelineBindPoint::GRAPHICS, g.playout, 1, &[g.set], &vs_offs);
                    pd.cmd_push_descriptor_set(cb, vk::PipelineBindPoint::GRAPHICS, g.playout, 0, &writes);
                    d.cmd_push_constants(cb, g.playout, vk::ShaderStageFlags::FRAGMENT, 0, &ns.to_le_bytes());
                    d.cmd_bind_index_buffer(cb, g.ibuf.buf, ioff, vk::IndexType::UINT32);
                    d.cmd_draw_indexed(cb, dr.indices.len() as u32, 1, 0, 0, 0);
                }
            }
            if let (Some(q), Some(p)) = (q, self.prof.as_mut()) {
                p.after(&d, cb, which, q, prof_meta(ns));
            }
        }
        self.stats[0] += 1;
        if shaded {
            self.stats[if aot.is_some() { 8 } else { 9 }] += 1;
            if let (Some(i), Some(t)) = (aot, self.aot_table.as_ref()) {
                if t.progs.get(i).is_some_and(|p| p.mode == "dispatch") {
                    self.stats[12] += 1;
                }
            }
        }

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
        Ok(true)
    }

    fn pipe_key(&mut self, c: &Ctx, vs: bool) -> PipeKey {
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
            vs,
            cull: if vs { c.cull & 3 } else { 0 },
            aot: 0,
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
            fs: FsKey::default(),
            fastz: false,
        }
    }

    /// Módulo de vértice e layout de um pipeline.
    unsafe fn stage_for(&mut self, k: PipeKey) -> Result<(vk::ShaderModule, vk::PipelineLayout), String> {
        Ok(match (&self.vsg, k.vs) {
            (Some(g), true) if k.aot == 0 => (g.module, g.playout),
            (Some(g), true) => {
                let l = g.playout;
                (self.aot_module(k.aot as usize - 1)?, l)
            }
            _ => (self.vs, self.playout),
        })
    }

    /// Pipeline pronto `k`, contando o uso (1 por lista de comandos: a ordem da pré-compilação do próximo boot).
    fn use_pipe(&mut self, k: &PipeKey) -> Option<vk::Pipeline> {
        let e = self.pipelines.get_mut(k)?;
        if e.last != self.list {
            e.last = self.list;
            e.uses += 1;
            self.keys_dirty = true;
            if e.warm && e.uses == 1 {
                self.stats[25] += 1;
            }
        }
        Some(e.p)
    }

    /// Pipeline `k` para o draw AGORA. Se falta, o jogo trava até ele existir: se já está na fila da thread de
    /// compilação (pré-compilação ou pedido anterior) e ainda não começou, sai da fila e compila aqui; se está
    /// compilando lá, espera ESSE (nada em dobro); senão compila aqui, como sempre. O tempo parado entra nos
    /// contadores de travada (`stats[20..23]`).
    unsafe fn pipeline(&mut self, k: PipeKey) -> Result<vk::Pipeline, String> {
        if let Some(p) = self.use_pipe(&k) {
            return Ok(p);
        }
        let t0 = std::time::Instant::now();
        if let Some(c) = self.compiler.as_mut().filter(|c| c.pending.contains_key(&k)) {
            match c.take_or_wait(&k) {
                Some(warm) => {
                    c.pending.remove(&k);
                    if warm {
                        self.warm_done(1, None);
                    }
                }
                None => {
                    self.drain_compiles();
                    if let Some(p) = self.use_pipe(&k) {
                        self.stalled(k, t0.elapsed().as_secs_f64() * 1000.0, "esperando a thread");
                        return Ok(p);
                    }
                    // a compilação de lá falhou: tenta aqui (o erro sai pelo caminho normal)
                }
            }
        }
        let (vmod, layout) = self.stage_for(k)?;
        let t1 = std::time::Instant::now();
        let p = build_pipeline(&self.device, self.pcache, [self.fs, self.fs_fastz], k, vmod, layout)?;
        // a 1ª compilação de um pipeline num driver leva centenas de ms (depois o cache do driver resolve)
        self.pipeline_built(k, p, t1.elapsed().as_secs_f64() * 1000.0, Origin::Sync);
        self.stalled(k, t0.elapsed().as_secs_f64() * 1000.0, "compilando");
        Ok(self.use_pipe(&k).unwrap_or(p))
    }

    /// O draw ficou `ms` parado por causa do pipeline `k` (compilação síncrona ou espera da thread).
    fn stalled(&mut self, k: PipeKey, ms: f64, how: &str) {
        let us = (ms * 1000.0) as u64;
        self.stats[20] += 1;
        self.stats[21] += us;
        self.stats[22] = self.stats[22].max(us);
        // informativo, cota própria (não esconde os erros de `warn`)
        if ms > 16.0 && self.stall_logged < 16 {
            self.stall_logged += 1;
            let what = if k.fastz {
                "profundidade rápida"
            } else if k.fs.spec {
                "fragmento especializado"
            } else if k.aot != 0 {
                "AOT"
            } else if k.vs {
                "ubershader"
            } else {
                "vértices da CPU"
            };
            eprintln!(
                "[vulkan] travada de {ms:.0} ms {how} um pipeline ({what}) durante o jogo{}",
                if self.stall_logged == 16 { " (últimos avisos de travada)" } else { "" }
            );
        }
    }

    /// Grava o cache de pipelines do driver (se mudou) e a lista de chaves (se mudou): temporário + rename, numa
    /// thread à parte (`vk-pcache`) ou, com `sync`, aqui (fim da rodada).
    fn save_pipeline_cache(&mut self, sync: bool) {
        self.pcache_saved = std::time::Instant::now();
        let mut blob = None;
        if std::mem::take(&mut self.pcache_dirty) && self.pcache != vk::PipelineCache::null() {
            if let Some(path) = pipeline_cache_path() {
                // SAFETY: cache válido do dispositivo (sincronizado internamente)
                match unsafe { self.device.get_pipeline_cache_data(self.pcache) } {
                    Ok(d) => blob = Some((path, d)),
                    Err(e) => self.warn(&format!("vkGetPipelineCacheData: {e:?}")),
                }
            }
        }
        let mut keys = None;
        if std::mem::take(&mut self.keys_dirty) {
            if let Some(path) = self.keylog.path.clone() {
                let l = &self.keylog;
                keys = Some((path, l.aot_hash, l.naot, l.merged(&self.pipelines)));
            }
        }
        if blob.is_none() && keys.is_none() {
            return;
        }
        let write = move || {
            if let Some((path, data)) = blob {
                write_atomic(&path, &data, "o cache de pipelines");
            }
            if let Some((path, h, n, list)) = keys {
                write_atomic(&path, &encode_keys(spirv_hash(), h, n, &list), "a lista de pipelines");
            }
        };
        if sync {
            write();
        } else {
            let _ = std::thread::Builder::new().name("vk-pcache".into()).spawn(write);
        }
    }

    fn pipeline_built(&mut self, k: PipeKey, p: vk::Pipeline, ms: f64, origin: Origin) {
        // informativo, fora da cota de `warn` (o total dos AOT sai no resumo final, `stats[10]`); as síncronas saem
        // como travada (`stalled`) e as do boot não são avisadas uma a uma
        if origin == Origin::Game && ms > 100.0 && self.slow_logged < 16 {
            self.slow_logged += 1;
            eprintln!(
                "[vulkan] pipeline (em segundo plano) levou {ms:.0} ms para compilar{}",
                if self.slow_logged == 16 { " (últimos avisos de compilação lenta)" } else { "" }
            );
        }
        let us = (ms * 1000.0) as u64;
        if origin == Origin::Warm {
            self.stats[23] += 1;
            self.stats[24] += us;
        } else if k.aot != 0 {
            self.stats[10] += us;
        }
        if self.pipelines.contains_key(&k) {
            // não acontece (pedido nunca é compilado em dobro); se acontecer, fica o antigo (pode estar em voo) e o
            // novo, que ninguém usou, é destruído
            // SAFETY: pipeline recém-criado, nunca gravado em buffer de comandos
            unsafe { self.device.destroy_pipeline(p, None) };
            return;
        }
        self.pipelines.insert(k, PipeEntry { p, last: u64::MAX, uses: 0, warm: origin == Origin::Warm });
        self.stats[4] += 1;
        self.pcache_dirty = true;
        self.keys_dirty = true;
    }

    /// Pipelines compilados em segundo plano que ficaram prontos.
    fn drain_compiles(&mut self) {
        let Some(c) = self.compiler.as_mut() else { return };
        if c.ndone.load(Ordering::Acquire) == 0 {
            return;
        }
        let done = c.take_done();
        for d in done {
            let warm = matches!(self.compiler.as_mut().unwrap().pending.remove(&d.k), Some(Pend::Warm | Pend::WarmTried));
            match d.r {
                Ok(p) => self.pipeline_built(d.k, p, d.ms, if warm { Origin::Warm } else { Origin::Game }),
                // pré-compilação: a chave sai da lista; as que têm reserva ficam com ela, como no pedido do jogo
                Err(e) if warm => {
                    self.warn(&format!("pré-compilação de um pipeline falhou ({e}); a chave sai da lista"));
                    self.keylog.base.remove(&d.k);
                    self.keys_dirty = true;
                    if !key_sync(&d.k) {
                        self.aot_failed.insert(d.k);
                    }
                }
                Err(e) => {
                    self.warn(&format!("pipeline em segundo plano não compilou ({e}); esse estado fica com a reserva"));
                    self.aot_failed.insert(d.k);
                }
            }
            if warm {
                self.warm_done(1, Some(d.at));
            }
        }
    }

    /// `n` trabalhos da pré-compilação saíram da fila (prontos, promovidos a pedido do jogo, compilados no draw ou
    /// cancelados). No último, registra o tempo de parede (`stats[27]`, até `at` = fim do último) e avisa.
    fn warm_done(&mut self, n: usize, at: Option<std::time::Instant>) {
        if self.warm_left == 0 {
            return;
        }
        self.warm_left = self.warm_left.saturating_sub(n);
        if self.warm_left == 0 {
            if let Some(t0) = self.prewarm_t0 {
                let end = at.unwrap_or_else(std::time::Instant::now);
                self.stats[27] = end.saturating_duration_since(t0).as_micros() as u64;
                eprintln!(
                    "[vulkan] pré-compilação do boot terminou: {} pipelines em {:.1} s (soma das compilações {:.0} ms)",
                    self.stats[23],
                    self.stats[27] as f64 / 1e6,
                    self.stats[24] as f64 / 1000.0
                );
            }
        }
    }

    /// Tira da fila a pré-compilação que ainda não começou.
    fn cancel_warm(&mut self) {
        let Some(c) = self.compiler.as_mut() else { return };
        let gone = c.cancel_warm();
        for k in &gone {
            c.pending.remove(k);
        }
        self.warm_done(gone.len(), None);
    }

    fn ensure_compiler(&mut self) -> &mut Compiler {
        if self.compiler.is_none() {
            self.compiler = Some(Compiler::start(self.device.clone(), self.pcache, [self.fs, self.fs_fastz]));
        }
        self.compiler.as_mut().unwrap()
    }

    /// O pipeline `k` (AOT, fragmento especializado, profundidade rápida — todos com reserva) está pronto? Se não, pede
    /// a compilação em segundo plano (o draw usa a reserva enquanto isso, como o Dolphin faz com os "ubershaders") —
    /// a 1ª compilação de um pipeline leva 0,1–1 s num driver sem cache e travaria o jogo. Se já estava na fila da
    /// pré-compilação, passa para a frente. `YW_VK_AOT_SYNC=1` compila na hora.
    unsafe fn aot_ready(&mut self, k: PipeKey) -> Result<bool, String> {
        self.drain_compiles();
        if self.use_pipe(&k).is_some() {
            return Ok(true);
        }
        if self.aot_failed.contains(&k) {
            return Ok(false);
        }
        if aot_sync() {
            self.pipeline(k)?;
            return Ok(true);
        }
        if let Some(c) = self.compiler.as_mut() {
            match c.pending.get(&k) {
                Some(Pend::Game | Pend::WarmTried) => return Ok(false),
                Some(Pend::Warm) => {
                    // na fila da pré-compilação: o jogo quer agora → fila do jogo (sai da conta da pré-compilação).
                    // Se já começou (ou terminou), continua sendo dela; não tenta de novo a cada draw.
                    if c.promote(&k) {
                        c.pending.insert(k, Pend::Game);
                        self.warm_done(1, None);
                    } else {
                        c.pending.insert(k, Pend::WarmTried);
                    }
                    return Ok(false);
                }
                None => {}
            }
        }
        let (vmod, layout) = self.stage_for(k)?;
        let c = self.ensure_compiler();
        c.pending.insert(k, Pend::Game);
        c.submit(Job { k, vmod, layout, warm: false });
        Ok(false)
    }

    /// Pré-compilação no boot (`YW_VK_PREWARM=0` desliga): enfileira as chaves da lista salva que valem nesta execução,
    /// na ordem de `prewarm_order`. Chamada logo depois de criar o `Vk`; só cria os recursos de que as chaves
    /// precisam (os do shader de vértice na GPU e os módulos AOT, que o 1º draw criaria de qualquer jeito).
    unsafe fn prewarm_start(&mut self) {
        if !prewarm_on() || self.keylog.base.is_empty() {
            return;
        }
        let t0 = std::time::Instant::now();
        let list: Vec<(PipeKey, u64)> = self.keylog.base.iter().map(|(k, n)| (*k, *n)).collect();
        // vértices na GPU: só se esta execução os usa (sem `render::backend()`: ainda estamos dentro do `init`)
        let vs_ok = list.iter().any(|(k, _)| k.vs) && !super::cpu_vs_forced() && self.vsg_ready();
        // tabela AOT da lista = a registrada (a mesma de `KeyLog::open`); liga-a já, como o 1º `aot_for` faria
        let mut aot_ok = false;
        if aot::enabled() {
            if let Some(t) = aot::table().filter(|t| t.hash == self.keylog.aot_hash) {
                if t.gen != self.aot_gen {
                    self.aot_gen = t.gen;
                    self.aot_mods = vec![None; t.progs.len()];
                    self.aot_table = Some(t);
                }
                aot_ok = true;
            }
        }
        let naot = self.aot_mods.len();
        let logic_ok = self.logic_ok;
        let usable = |k: &PipeKey| {
            (!k.vs || vs_ok)
                && (k.aot == 0 || (aot_ok && k.aot as usize <= naot))
                && (!k.fs.spec || fs_spec())
                && (!k.fastz || fastz())
                && (k.blend || k.logic == 3 || logic_ok)
        };
        let keys: Vec<PipeKey> = prewarm_order(&list).into_iter().filter(|k| usable(k)).collect();
        let mut n = 0usize;
        let mut nsync = 0usize;
        for k in keys {
            let (vmod, layout) = match self.stage_for(k) {
                Ok(s) => s,
                Err(e) => {
                    self.warn(&format!("pré-compilação: {e}"));
                    continue;
                }
            };
            let c = self.ensure_compiler();
            c.pending.insert(k, Pend::Warm);
            c.submit(Job { k, vmod, layout, warm: true });
            n += 1;
            nsync += key_sync(&k) as usize;
        }
        if n == 0 {
            return;
        }
        self.prewarm_t0 = Some(t0);
        self.warm_left = n;
        self.stats[26] = n as u64;
        eprintln!(
            "[vulkan] pré-compilando {n} pipelines da lista em segundo plano ({nsync} que travariam o jogo primeiro; enfileirados em {:.1} ms; YW_VK_PREWARM=0 desliga)",
            t0.elapsed().as_secs_f64() * 1000.0
        );
    }

    /// Espera as compilações em segundo plano terminarem (testes; e antes de destruir módulos AOT).
    fn wait_compiles(&mut self) {
        loop {
            self.drain_compiles();
            if self.compiler.as_ref().is_none_or(|c| c.pending.is_empty()) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
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
            let readback = self.host_buf(rb, vk::BufferUsageFlags::TRANSFER_DST, HostMem::Readback)?;
            if k.addr != 0 && !(0x1f00_0000..0x1f60_0000).contains(&k.addr) {
                eprintln!("[vulkan] alvo de render FORA da VRAM: {:#x} {}x{} fmt {} (profundidade {})", k.addr, k.w, k.h, k.fmt, k.depth);
            }
            self.surfaces.insert(
                k,
                Surface {
                    img,
                    readback,
                    snapshot: None,
                    synced: false,
                    dirty: false,
                    pending: false,
                    pending_v: 0,
                    clear: None,
                    last_used: 0,
                    shadow: None,
                },
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
            s.snapshot = snap_of(mem);
            if let Some(sh) = s.shadow.as_mut() {
                sh.src = Some(ShadowSrc::Clear(c));
            }
            self.ip.note_clear(k, c);
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
        if snap_eq(s.snapshot, mem) {
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
        s.snapshot = snap_of(mem);
        if let Some(sh) = s.shadow.as_mut() {
            sh.src = Some(ShadowSrc::Memory);
        }
        let dst = std::slice::from_raw_parts_mut(self.ring.ptr.add(off as usize), bytes as usize);
        decode_surface(k, mem, dst);
        let regions = copy_regions(k, off);
        self.device.cmd_copy_buffer_to_image(self.cmd_pre, self.ring.buf, image, vk::ImageLayout::GENERAL, &regions);
        self.stats[2] += 1;
        self.stats[6] += t0.elapsed().as_micros() as u64;
        Ok(view)
    }

    /// Textura em cache (decodifica e envia se o conteúdo mudou) e a chave da versão usada (`None` = memória não
    /// mapeada: a vista é o dummy 1×1). Só para a passada nativa: o replay da interpolação usa `replay_tex`.
    unsafe fn texture(&mut self, t: &Tex) -> Result<(vk::ImageView, Option<TexKey>), String> {
        let n = t.w * t.h * texture::bits_per_texel(t.format) / 8;
        let gk = (t.addr, t.w, t.h, t.format);
        let hash = match self.tex_hash.get(&gk) {
            Some(&(l, hv)) if l == self.list => hv,
            _ => {
                let Some(mem) = guest_slice(t.addr, n) else { return Ok((self.dummy.view, None)) };
                let hv = hash_bytes(mem);
                self.tex_hash.insert(gk, (self.list, hv));
                hv
            }
        };
        let key = TexKey { addr: t.addr, w: t.w, h: t.h, fmt: t.format, hash };
        if let Some(e) = self.textures.get_mut(&key) {
            e.last_used = self.batch;
            return Ok((e.img.view, Some(key)));
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
        TEX_BYTES.store(self.tex_bytes, Ordering::Relaxed);
        self.stats[3] += 1;
        Ok((view, Some(key)))
    }

    /// Envia o lote (no modo síncrono, espera) e marca as superfícies desenhadas como pendentes de devolução para a
    /// memória guest (`pending_v` = o valor do envio).
    unsafe fn flush(&mut self) -> Result<(), String> {
        if !self.recording {
            return Ok(());
        }
        let d = self.device.clone();
        let cb = self.cmd_main;
        if self.pass.take().is_some() {
            d.cmd_end_rendering(cb);
        }
        self.end_pass_s();
        self.full_barrier(cb);
        let dirty: Vec<SurfKey> = self.surfaces.iter().filter(|(_, s)| s.dirty).map(|(k, _)| *k).collect();
        for k in &dirty {
            let s = &self.surfaces[k];
            d.cmd_copy_image_to_buffer(cb, s.img.image, vk::ImageLayout::GENERAL, s.readback.buf, &copy_regions(*k, 0));
        }
        // cópias de devolução → leitura pelo host (TRANSFER do sync1 = ALL_TRANSFER do sync2)
        self.mem_barrier(
            cb,
            (vk::PipelineStageFlags2::ALL_TRANSFER, vk::AccessFlags2::TRANSFER_WRITE),
            (vk::PipelineStageFlags2::HOST, vk::AccessFlags2::HOST_READ),
        );
        d.cmd_write_timestamp(cb, vk::PipelineStageFlags::BOTTOM_OF_PIPE, self.ts_pool, self.ts_base + 1);
        d.cmd_write_timestamp(self.cmd_shadow, vk::PipelineStageFlags::BOTTOM_OF_PIPE, self.ts_pool, self.ts_base + 2);
        vkr!(d.end_command_buffer(self.cmd_pre), "vkEndCommandBuffer");
        vkr!(d.end_command_buffer(cb), "vkEndCommandBuffer");
        vkr!(d.end_command_buffer(self.cmd_shadow), "vkEndCommandBuffer");
        // cena para o olho escondido (`before_read_lcd`): a sombra dela nunca vai para a tela. Só quando o buffer da
        // sombra deste lote tem apenas draws em alvos lidos por essa transferência; depois, essas sombras recarregam
        // da memória no próximo uso (se a próxima cena não começar com um clear)
        let skip_shadow = !self.shadow_drawn.is_empty()
            && !self.shadow_other
            && self.discard_shadow.is_some_and(|(lo, hi)| {
                self.shadow_drawn.iter().all(|(c, _)| {
                    let (a, b) = c.range();
                    lo <= a && b <= hi
                })
            });
        let cbs_all = [self.cmd_pre, cb, self.cmd_shadow];
        let cbs = if skip_shadow { &cbs_all[..2] } else { &cbs_all[..] };
        self.recording = false;
        let kind = SubKind::Batch { nts: if skip_shadow { 2 } else { 3 }, mb: self.mid_b };
        let v = self.submit(cbs, self.slot, kind, "lote")?;
        // modo síncrono: espera aqui (o comportamento de antes); assíncrono: quem precisar do resultado espera
        if !self.async_mode {
            self.wait_value(v)?;
        }
        if skip_shadow {
            self.stats[17] += 1;
            for (c, z) in std::mem::take(&mut self.shadow_drawn) {
                for k in [c, z] {
                    if let Some(sh) = self.surfaces.get_mut(&k).and_then(|s| s.shadow.as_mut()) {
                        sh.src = Some(ShadowSrc::Memory);
                    }
                }
            }
        }
        self.shadow_drawn.clear();
        self.shadow_other = false;
        let t1 = std::time::Instant::now();

        // imagem → memória guest: preguiçoso. O conteúdo fica no `readback` e só é convertido quando alguém precisar
        // da faixa (`sync_range`). `YW_VK_EAGER=1` volta ao comportamento antigo (devolve tudo no fim de cada lote).
        for k in &dirty {
            let s = self.surfaces.get_mut(k).unwrap();
            s.dirty = false;
            s.pending = true;
            s.pending_v = v;
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
        self.batch += 1;
        self.stats[1] += 1;
        self.evict();
        Ok(())
    }

    /// Converte o `readback` de uma superfície pendente para a memória guest (mesmo formato que o soft escreveria).
    unsafe fn materialize(&mut self, k: SurfKey) {
        let Some(s) = self.surfaces.get(&k) else { return };
        if !s.pending {
            return;
        }
        // o envio que gravou o `readback` pode ainda estar na GPU
        let pv = s.pending_v;
        if pv > self.done_v {
            if let Err(e) = self.wait_value(pv) {
                self.warn(&format!("devolução de {:#x}: {e}", k.addr));
            }
        }
        let Some(s) = self.surfaces.get_mut(&k) else { return };
        s.pending = false;
        // a memória dessa faixa vai mudar: o hash de textura guardado nesta lista (por faixa de endereços) deixa de valer
        // — texturas renderizadas pela GPU e amostradas depois, na mesma lista, ficariam com a imagem antiga
        let (lo, hi) = k.range();
        forget_tex_hash(&mut self.tex_hash, lo, hi);
        let t0 = std::time::Instant::now();
        {
            let Some(cur) = guest_slice(k.addr, k.bytes()) else { return };
            // DIAGNÓSTICO: a memória deste alvo mudou DEPOIS do último sincronismo (a CPU escreveu nela) e a GPU também
            // tem conteúdo novo — gravar o alvo agora sobrescreve o que a CPU escreveu (o jogo pode ter reaproveitado a
            // memória para outra coisa, ex.: buffers de áudio na memória linear).
            if s.snapshot.is_some() && !snap_eq(s.snapshot, cur) {
                static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                if N.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 12 {
                    eprintln!(
                        "[vulkan] CONFLITO ao devolver o alvo {:#x} {}x{} fmt {} (profundidade {}): a CPU mudou a memória depois do último sincronismo e a GPU tem conteúdo pendente; a devolução vai sobrescrevê-los",
                        k.addr, k.w, k.h, k.fmt, k.depth
                    );
                }
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
                s.snapshot = if lossy { None } else { snap_of(memw) };
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
            s.snapshot = if lossy { None } else { guest_slice(k.addr, k.bytes()).and_then(snap_of) };
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
        // escrita do GSP (MemoryFill, DisplayTransfer, TextureCopy) na faixa: o hash de textura guardado nesta lista
        // para ela deixa de valer (uma textura amostrada depois, na mesma lista, tem de ser relida)
        if write {
            self.tex_hash_forget(lo, hi);
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
                s.snapshot = None;
                s.synced = false;
            } else {
                self.materialize(k);
            }
        }
        Ok(())
    }

    /// Esquece o hash (por lista) das texturas cuja faixa cruza `[lo, hi)`: a memória delas vai mudar.
    fn tex_hash_forget(&mut self, lo: u32, hi: u32) {
        forget_tex_hash(&mut self.tex_hash, lo, hi);
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

    /// Libera texturas e superfícies sem uso há muito tempo (entre lotes; a destruição é adiada até a GPU terminar os
    /// envios que ainda podem usá-las, `defer`).
    unsafe fn evict(&mut self) {
        let b = self.batch;
        // (1) versões velhas: quando a CPU muda o conteúdo de uma textura (vídeo, texto, atlas), o hash novo cria outra
        // entrada para o mesmo (endereço, tamanho, formato). Num lote só uma versão é usada (o hash é calculado uma
        // vez por lote), então toda versão usada antes da mais recente já não serve e sai agora (destruída quando a GPU
        // terminar os lotes em voo).
        let mut newest: HashMap<(u32, u32, u32, u32), u64> = HashMap::new();
        for (k, e) in &self.textures {
            let n = newest.entry((k.addr, k.w, k.h, k.fmt)).or_insert(0);
            *n = (*n).max(e.last_used);
        }
        // Versões pinadas (`tex_pinned`: usadas pelos quadros capturados para a interpolação) nunca saem aqui: o replay
        // desenha com elas mesmo depois de o jogo ter trocado o conteúdo da textura.
        let pinned = &self.tex_pinned;
        let mut old: Vec<TexKey> = self
            .textures
            .iter()
            .filter(|(k, e)| e.last_used < newest[&(k.addr, k.w, k.h, k.fmt)] && !pinned.contains(k))
            .map(|(k, _)| *k)
            .collect();
        // (2) muitas entradas: as sem uso há 300 lotes
        if self.textures.len() > 1500 {
            old.extend(self.textures.iter().filter(|(k, e)| e.last_used + 300 < b && !pinned.contains(k)).map(|(k, _)| *k));
        }
        old.sort_unstable_by_key(|k| (k.addr, k.w, k.h, k.fmt, k.hash));
        old.dedup();
        for k in old {
            let e = self.textures.remove(&k).unwrap();
            self.tex_bytes -= e.bytes;
            self.defer(Res::Img(e.img));
        }
        // (3) limite de memória: descarta as menos usadas recentemente até caber
        if self.tex_bytes > TEX_BUDGET {
            let mut lru: Vec<(u64, TexKey)> =
                self.textures.iter().filter(|(k, _)| !self.tex_pinned.contains(k)).map(|(k, e)| (e.last_used, *k)).collect();
            lru.sort_unstable_by_key(|(u, _)| *u);
            for (_, k) in lru {
                if self.tex_bytes <= TEX_BUDGET {
                    break;
                }
                let e = self.textures.remove(&k).unwrap();
                self.tex_bytes -= e.bytes;
                self.defer(Res::Img(e.img));
            }
        }
        TEX_BYTES.store(self.tex_bytes, Ordering::Relaxed);
        if self.tex_hash.len() > 4096 {
            let l = self.list;
            self.tex_hash.retain(|_, (ll, _)| *ll + 2 >= l);
        }
        if self.surfaces.len() > 32 {
            let old: Vec<SurfKey> = self.surfaces.iter().filter(|(_, s)| s.last_used + 3000 < b).map(|(k, _)| *k).collect();
            for k in old {
                self.hr.aa.remove(&k);
                let s = self.surfaces.remove(&k).unwrap();
                if let Some(sh) = s.shadow {
                    self.defer(Res::Img(sh.img));
                }
                self.defer(Res::Img(s.img));
                self.defer(Res::Buf(s.readback));
            }
        }
    }
}

/// Cria um pipeline gráfico para o estado `k` (função solta: também roda na thread de compilação em segundo plano).
/// `fs` = [pica.frag, pica_frag_fastz]: o módulo de fragmento sai de `k.fastz`.
unsafe fn build_pipeline(
    device: &ash::Device,
    cache: vk::PipelineCache,
    fs: [vk::ShaderModule; 2],
    k: PipeKey,
    vmod: vk::ShaderModule,
    layout: vk::PipelineLayout,
) -> Result<vk::Pipeline, String> {
    let (sw, se) = k.fs.words();
    let sdata: &[u8] = std::slice::from_raw_parts(sw.as_ptr() as *const u8, sw.len() * 4);
    let sinfo = vk::SpecializationInfo::default().map_entries(&se).data(sdata);
    let fs = fs[k.fastz as usize];
    let mut fstage = vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::FRAGMENT).module(fs).name(c"main");
    if k.fs.spec {
        fstage = fstage.specialization_info(&sinfo);
    }
    let stages = [vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::VERTEX).module(vmod).name(c"main"), fstage];
    let binds = [vk::VertexInputBindingDescription { binding: 0, stride: VTX_STRIDE, input_rate: vk::VertexInputRate::VERTEX }];
    let attr = |location: u32, format: vk::Format, offset: u32| vk::VertexInputAttributeDescription { location, binding: 0, format, offset };
    let attrs = [
        attr(0, vk::Format::R32G32B32A32_SFLOAT, 0),
        attr(1, vk::Format::R32G32B32A32_SFLOAT, 16),
        attr(2, vk::Format::R32G32_SFLOAT, 32),
        attr(3, vk::Format::R32G32_SFLOAT, 40),
        attr(4, vk::Format::R32G32_SFLOAT, 48),
    ];
    // vértices na GPU: sem atributos (o shader lê a memória guest do storage buffer)
    let vi = if k.vs {
        vk::PipelineVertexInputStateCreateInfo::default()
    } else {
        vk::PipelineVertexInputStateCreateInfo::default()
            .vertex_binding_descriptions(&binds)
            .vertex_attribute_descriptions(&attrs)
    };
    let ia = vk::PipelineInputAssemblyStateCreateInfo::default().topology(vk::PrimitiveTopology::TRIANGLE_LIST);
    let vp = vk::PipelineViewportStateCreateInfo::default().viewport_count(1).scissor_count(1);
    // Vértices da CPU: recorte em z já feito (planos do PICA) e profundidade saturada como no soft → depth clamp,
    // sem descarte (feito em `render::prepare`).
    // Vértices da GPU: o hardware recorta em 0 ≤ z ≤ w (sem depth clamp) e descarta pela orientação. A área do
    // Vulkan é a = −½ Σ (xᵢ yᵢ₊₁ − xᵢ₊₁ yᵢ) em coordenadas de framebuffer (linha = y do PICA), ou seja −½ da área
    // do soft (`raster::area`): com frente = anti-horário (a > 0), frente ⇔ área do soft < 0. Modo 1 mantém área
    // < 0 → descarta a traseira; modos 2/3 mantêm área > 0 → descarta a frente (mesmo resultado do Zakuro, que
    // usa frente = horário com os papéis trocados).
    let cull = match (k.vs, k.cull) {
        (true, 1) => vk::CullModeFlags::BACK,
        (true, 2 | 3) => vk::CullModeFlags::FRONT,
        _ => vk::CullModeFlags::NONE,
    };
    let rs = vk::PipelineRasterizationStateCreateInfo::default()
        .depth_clamp_enable(!k.vs)
        .polygon_mode(vk::PolygonMode::FILL)
        .cull_mode(cull)
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
    // dynamic rendering: os formatos dos anexos no lugar do render pass (cor RGBA8; D32S8 em profundidade e estêncil)
    let cfmt = [COLOR_FMT];
    let mut rinfo = vk::PipelineRenderingCreateInfo::default()
        .color_attachment_formats(&cfmt)
        .depth_attachment_format(DEPTH_FMT)
        .stencil_attachment_format(DEPTH_FMT);
    let ci = vk::GraphicsPipelineCreateInfo::default()
        .push_next(&mut rinfo)
        .stages(&stages)
        .vertex_input_state(&vi)
        .input_assembly_state(&ia)
        .viewport_state(&vp)
        .rasterization_state(&rs)
        .multisample_state(&ms)
        .depth_stencil_state(&ds)
        .color_blend_state(&cbs)
        .dynamic_state(&dy)
        .layout(layout);
    let p = device
        .create_graphics_pipelines(cache, &[ci], None)
        .map_err(|(_, e)| format!("vkCreateGraphicsPipelines: {e:?}"))?[0];
    Ok(p)
}
/// Um pipeline a compilar em segundo plano: a chave, o módulo de vértice e o layout dela, e se é da pré-compilação.
struct Job {
    k: PipeKey,
    vmod: vk::ShaderModule,
    layout: vk::PipelineLayout,
    warm: bool,
}

/// Resultado de um `Job`, com o tempo de compilação (ms) e quando terminou.
struct Done {
    k: PipeKey,
    r: Result<vk::Pipeline, String>,
    ms: f64,
    at: std::time::Instant,
}

/// Fila de prioridade das threads de compilação: `urgent` = pedidos do jogo (reserva em uso enquanto compila),
/// `warm` = pré-compilação do boot. Pedido do jogo sempre passa na frente.
#[derive(Default)]
struct JobQueue {
    urgent: std::collections::VecDeque<Job>,
    warm: std::collections::VecDeque<Job>,
    /// chaves sendo compiladas agora por alguma thread
    running: std::collections::HashSet<PipeKey>,
    done: Vec<Done>,
    closed: bool,
}

impl JobQueue {
    /// Próximo trabalho de uma thread: os do jogo antes; `warm_ok` = esta thread também pega os da pré-compilação.
    fn pop(&mut self, warm_ok: bool) -> Option<Job> {
        let j = match self.urgent.pop_front() {
            Some(j) => j,
            None if warm_ok => self.warm.pop_front()?,
            None => return None,
        };
        self.running.insert(j.k);
        Some(j)
    }

    /// O jogo quer `k`, que está na fila da pré-compilação e ainda não começou: passa para o fim da fila do jogo.
    fn promote(&mut self, k: &PipeKey) -> bool {
        let Some(i) = self.warm.iter().position(|j| j.k == *k) else { return false };
        let mut j = self.warm.remove(i).unwrap();
        j.warm = false;
        self.urgent.push_back(j);
        true
    }

    /// Tira `k` da fila se ainda não começou (o draw vai compilar na hora). Some(veio da pré-compilação).
    fn take(&mut self, k: &PipeKey) -> Option<bool> {
        for q in [&mut self.urgent, &mut self.warm] {
            if let Some(i) = q.iter().position(|j| j.k == *k) {
                return q.remove(i).map(|j| j.warm);
            }
        }
        None
    }

    /// Esvazia a fila da pré-compilação (o que já começou termina).
    fn cancel_warm(&mut self) -> Vec<PipeKey> {
        self.warm.drain(..).map(|j| j.k).collect()
    }
}

/// Threads que compilam pipelines em segundo plano: `vk-pipelines` só pega pedidos do jogo (AOT, fragmento
/// especializado, profundidade rápida — `Vk::aot_ready`), então um pedido nunca espera atrás da pré-compilação;
/// `vk-prewarm` pega os do jogo primeiro e, sem eles, a pré-compilação do boot. O `ash::Device` é só tabela de funções
/// + handle; criar pipelines em paralelo com a gravação de comandos é permitido pelo Vulkan, e o VkPipelineCache é
/// sincronizado internamente (criado sem EXTERNALLY_SYNCHRONIZED): as duas threads e a do jogo o usam ao mesmo tempo.
struct Compiler {
    /// fila (+ em compilação + prontos), "há trabalho" e "um terminou"
    sh: std::sync::Arc<(Mutex<JobQueue>, std::sync::Condvar, std::sync::Condvar)>,
    /// prontos ainda não recolhidos (`drain_compiles` olha sem travar a fila a cada draw)
    ndone: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// pedidos ainda não recolhidos por `drain_compiles` e de quem são
    pending: HashMap<PipeKey, Pend>,
}

/// Dono de um pedido em `Compiler::pending`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pend {
    /// pedido do jogo (ou da pré-compilação promovido a pedido do jogo)
    Game,
    /// pré-compilação
    Warm,
    /// pré-compilação que o jogo quis quando ela já estava compilando (continua contando como pré-compilação)
    WarmTried,
}

impl Compiler {
    fn start(device: ash::Device, cache: vk::PipelineCache, fs: [vk::ShaderModule; 2]) -> Compiler {
        let sh = std::sync::Arc::new((Mutex::new(JobQueue::default()), std::sync::Condvar::new(), std::sync::Condvar::new()));
        let ndone = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for (name, warm_ok) in [("vk-pipelines", false), ("vk-prewarm", true)] {
            let (sh, ndone, device) = (sh.clone(), ndone.clone(), device.clone());
            std::thread::Builder::new()
                .name(name.into())
                .spawn(move || {
                    let (m, work, fin) = &*sh;
                    loop {
                        let job = {
                            let mut q = m.lock().unwrap();
                            loop {
                                if q.closed {
                                    return;
                                }
                                if let Some(j) = q.pop(warm_ok) {
                                    break j;
                                }
                                q = work.wait(q).unwrap();
                            }
                        };
                        let t0 = std::time::Instant::now();
                        // SAFETY: módulos e layout vivem até `drop_aot`, que tira a pré-compilação da fila e espera o
                        // resto terminar
                        let r = unsafe { build_pipeline(&device, cache, fs, job.k, job.vmod, job.layout) };
                        let ms = t0.elapsed().as_secs_f64() * 1000.0;
                        let mut q = m.lock().unwrap();
                        q.running.remove(&job.k);
                        q.done.push(Done { k: job.k, r, ms, at: std::time::Instant::now() });
                        ndone.fetch_add(1, Ordering::Release);
                        fin.notify_all();
                    }
                })
                .expect("thread de compilação de pipelines");
        }
        Compiler { sh, ndone, pending: HashMap::new() }
    }

    fn submit(&mut self, j: Job) {
        let (m, work, _) = &*self.sh;
        let mut q = m.lock().unwrap();
        if j.warm {
            q.warm.push_back(j);
        } else {
            q.urgent.push_back(j);
        }
        work.notify_all();
    }

    fn promote(&mut self, k: &PipeKey) -> bool {
        let (m, work, _) = &*self.sh;
        let ok = m.lock().unwrap().promote(k);
        work.notify_all();
        ok
    }

    fn cancel_warm(&mut self) -> Vec<PipeKey> {
        self.sh.0.lock().unwrap().cancel_warm()
    }

    /// Para o draw que precisa de `k` agora: se ainda está na fila, tira (Some(era da pré-compilação)) e quem chamou
    /// compila; se está compilando, espera terminar (None: o resultado está em `done`).
    fn take_or_wait(&mut self, k: &PipeKey) -> Option<bool> {
        let (m, _, fin) = &*self.sh;
        let mut q = m.lock().unwrap();
        if let Some(w) = q.take(k) {
            return Some(w);
        }
        while q.running.contains(k) {
            q = fin.wait(q).unwrap();
        }
        None
    }

    fn take_done(&mut self) -> Vec<Done> {
        let mut q = self.sh.0.lock().unwrap();
        self.ndone.store(0, Ordering::Release);
        std::mem::take(&mut q.done)
    }

    /// Fecha as threads: a fila é descartada e quem está compilando termina antes de o dispositivo sumir.
    fn close(&mut self) {
        let (m, work, fin) = &*self.sh;
        let mut q = m.lock().unwrap();
        q.closed = true;
        q.urgent.clear();
        q.warm.clear();
        work.notify_all();
        while !q.running.is_empty() {
            q = fin.wait(q).unwrap();
        }
    }
}

/// Profundidade da sombra/replay em DEPTH_STENCIL_ATTACHMENT_OPTIMAL durante a passada (GENERAL fora dela). Só junto
/// com a profundidade rápida (`fastz`): medido na RX 5500 XT (driver AMD), banco da sombra 3× aniso 16 8,98 → 6,81 ms
/// com as duas, 8,73 só com a rápida e 9,25 só com o layout (as transições custam e, com gl_FragDepth, o HiZ não
/// ajuda). NO JOGO (rota a 3× com interpolação, mesma sessão) foi o contrário: sombra 328 ms/s só com a rápida contra
/// 350 com o layout — o jogo fecha/abre muito mais passadas da sombra por quadro que o quadro do banco, e cada uma
/// paga duas transições. Por isso fica DESLIGADO por padrão; `YW_VK_DOPT=1` liga (só junto com `fastz`).
fn dopt() -> bool {
    static S: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    fastz() && *S.get_or_init(|| std::env::var("YW_VK_DOPT").is_ok_and(|v| v != "0"))
}

/// `YW_VK_AOT_SYNC=1`: compila os pipelines AOT na hora (sem a thread em segundo plano).
fn aot_sync() -> bool {
    static S: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *S.get_or_init(|| std::env::var("YW_VK_AOT_SYNC").is_ok_and(|v| v != "0"))
}

/// Espera os pipelines AOT pedidos ficarem prontos (testes: as passadas seguintes usam os AOT de forma determinística).
pub fn wait_pipelines() {
    let mut g = VK.lock().unwrap();
    if let Some(v) = g.as_mut() {
        v.wait_compiles();
    }
}

impl Drop for Vk {
    fn drop(&mut self) {
        // as threads de compilação usam o dispositivo: param antes (a fila é descartada)
        if let Some(c) = self.compiler.as_mut() {
            c.close();
        }
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

/// Memória guest (blocos 8×8, linhas de baixo para cima) → imagem linear: cor RGBA8, ou profundidade f32 seguida do
/// estêncil u8. `dst` tem `w·h·4` (cor) ou `w·h·5` (profundidade) bytes.
fn decode_surface(k: SurfKey, mem: &[u8], dst: &mut [u8]) {
    let (w, h) = (k.w, k.h);
    let n = (w * h) as u64;
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

#[cfg(test)]
mod testes_arena {
    use super::{align, Arena};

    /// Gerador simples (determinístico) para os tamanhos das reservas.
    fn rnd(s: &mut u64) -> u64 {
        *s ^= *s << 13;
        *s ^= *s >> 7;
        *s ^= *s << 17;
        *s
    }

    /// Modo síncrono (cada lote enviado e esperado): os deslocamentos e as condições de "cheio" são os do contador
    /// linear de antes (`used`, zerado a cada lote).
    #[test]
    fn sincrono_igual_ao_contador_linear() {
        let size = 1 << 16;
        let mut a = Arena::new(size);
        let (mut used, mut v, mut s) = (0u64, 0u64, 0x9E37_79B9u64);
        for _ in 0..20_000 {
            let n = 1 + rnd(&mut s) % 9000;
            let al = [1u64, 16, 64][(rnd(&mut s) % 3) as usize];
            let cheio = align(used, al) + n > size;
            assert_eq!(!a.fits(n, n, al) && a.cur > 0, cheio && used > 0, "condição de cheio");
            if cheio {
                v += 1;
                a.submit(v);
                a.retire(v);
                used = 0;
            }
            let o = a.try_alloc(n, n, al).expect("lote vazio sempre cabe");
            assert_eq!(o, align(used, al), "deslocamento");
            used = align(used, al) + n;
        }
    }

    /// Modo assíncrono: nenhuma reserva cai em bytes de um lote ainda em voo, e a janela cabe no buffer.
    #[test]
    fn assincrono_nao_pisa_em_lote_em_voo() {
        let size = 1 << 16;
        let mut a = Arena::new(size);
        // faixas físicas por lote: (valor, [início, fim))
        let mut voo: Vec<(u64, u64, u64)> = Vec::new();
        let mut cur: Vec<(u64, u64)> = Vec::new();
        let (mut v, mut done, mut s) = (0u64, 0u64, 0x1234_5678u64);
        for _ in 0..50_000 {
            let n = 4 + rnd(&mut s) % 5000;
            let win = n + rnd(&mut s) % 3000;
            if rnd(&mut s) % 7 == 0 || !a.fits(n, win, 16) {
                v += 1;
                a.submit(v);
                voo.extend(cur.drain(..).map(|(i, f)| (v, i, f)));
                // a GPU anda um pouco (às vezes nada)
                done = (done + rnd(&mut s) % 3).min(v);
                a.retire(done);
                voo.retain(|x| x.0 > done);
            }
            let o = loop {
                match a.try_alloc(n, win, 16) {
                    Some(o) => break o,
                    None => {
                        done = a.oldest().expect("sem lote em voo e não coube");
                        a.retire(done);
                        voo.retain(|x| x.0 > done);
                    }
                }
            };
            assert!(o + win <= size && o % 16 == 0);
            for &(_, i, f) in &voo {
                assert!(o + n <= i || f <= o, "reserva [{o}, {}) pisa no lote em voo [{i}, {f})", o + n);
            }
            for &(i, f) in &cur {
                assert!(o + n <= i || f <= o, "reserva pisa no próprio lote");
            }
            cur.push((o, o + n));
        }
    }
}

#[cfg(test)]
mod testes_mem {
    /// `mem_report` com o dispositivo de verdade: VRAM usada > 0 e dentro do orçamento, buffers mapeados contados.
    #[test]
    #[ignore]
    fn vk_mem_report() {
        if let Err(e) = super::init() {
            println!("sem Vulkan ({e}): nada a conferir");
            return;
        }
        let (used, budget, host, tex) = super::mem_report();
        println!("VRAM {} MB de {} MB · buffers mapeados {} MB · texturas {} MB", used >> 20, budget >> 20, host >> 20, tex >> 20);
        assert!(used > 0 && used <= budget, "VRAM usada {used} / orçamento {budget}");
        assert!(host >= super::UBUF_SIZE + super::RING_SIZE, "buffers mapeados: {host}");
    }
}

#[cfg(test)]
mod testes_pipekeys {
    use super::*;

    const SPV: u64 = 0x1111_2222_3333_4444;
    const AOTH: u64 = 0x5555_6666_7777_8888;

    fn base() -> PipeKey {
        PipeKey {
            vs: true,
            cull: 1,
            aot: 0,
            blend: true,
            eq: [0, 4],
            fac: [6, 7, 1, 0],
            logic: 3,
            mask: 15,
            dtest: true,
            dfunc: 4,
            dwrite: true,
            stencil: false,
            sfunc: 1,
            sops: [0; 3],
            fs: FsKey::default(),
            fastz: false,
        }
    }

    fn amostra() -> Vec<(PipeKey, u64)> {
        let mut st = [[0u32; 4]; 6];
        st[0] = [0x0003_0201, 0x000f_0f0f, 0x0001_0008, 0x1234_5678];
        st[1] = [7, 8, 9, 10];
        let spec = FsKey { spec: true, nst: 2, flags: 0xdead_beef, bufupd: 0x0f00_0001, atest: 0x87, depth: 5, tex: [1, 0x100, 3], st };
        vec![
            (base(), 10),
            (PipeKey { aot: 3, ..base() }, 7),
            (PipeKey { aot: 3, fs: spec, ..base() }, u64::MAX),
            (PipeKey { fs: spec, fastz: true, stencil: true, sfunc: 7, sops: [7, 2, 5], ..base() }, 0),
            (PipeKey { vs: false, cull: 0, blend: false, logic: 12, ..base() }, 1),
        ]
    }

    /// Recalcula o rodapé (para testar uma recusa específica, não a do checksum).
    fn refaz_soma(b: &mut [u8]) {
        let n = b.len() - 8;
        let s = super::super::aot::fnv1a64(&b[..n]);
        b[n..].copy_from_slice(&s.to_le_bytes());
    }

    #[test]
    fn ida_e_volta() {
        let keys = amostra();
        let b = encode_keys(SPV, AOTH, 4, &keys);
        assert_eq!(b.len(), PK_HEADER + keys.len() * PK_ENTRY + 8);
        let (back, bad) = decode_keys(&b, SPV, AOTH, 4).unwrap();
        assert_eq!(bad, 0);
        assert_eq!(back, keys);
        // vazia também
        let (v, bad) = decode_keys(&encode_keys(SPV, AOTH, 0, &[]), SPV, AOTH, 0).unwrap();
        assert!(v.is_empty() && bad == 0);
    }

    #[test]
    fn arquivo_estragado_descarta_sem_panico() {
        let b = encode_keys(SPV, AOTH, 4, &amostra());
        // todo corte do arquivo é recusado inteiro, sem pânico
        for n in 0..b.len() {
            assert!(decode_keys(&b[..n], SPV, AOTH, 4).is_err(), "corte em {n} aceito");
        }
        // bytes a mais também
        let mut longo = b.clone();
        longo.push(0);
        assert!(decode_keys(&longo, SPV, AOTH, 4).is_err());
        // versão errada (com o checksum refeito: a recusa é pela versão)
        let mut v2 = b.clone();
        v2[4..8].copy_from_slice(&(PK_VERSION + 1).to_le_bytes());
        refaz_soma(&mut v2);
        assert!(decode_keys(&v2, SPV, AOTH, 4).unwrap_err().contains("versão"));
        // magic errado
        let mut m = b.clone();
        m[0] ^= 0x20;
        assert!(decode_keys(&m, SPV, AOTH, 4).is_err());
        // um bit trocado no meio: checksum
        let mut c = b.clone();
        c[PK_HEADER + 20] ^= 1;
        assert!(decode_keys(&c, SPV, AOTH, 4).unwrap_err().contains("checksum"));
        // contagem gigante no cabeçalho: sem estouro
        let mut g = b.clone();
        g[28..32].copy_from_slice(&u32::MAX.to_le_bytes());
        refaz_soma(&mut g);
        assert!(decode_keys(&g, SPV, AOTH, 4).is_err());
        // SPIR-V de outra versão: tudo fora
        assert!(decode_keys(&b, SPV ^ 1, AOTH, 4).is_err());
        // lixo puro
        assert!(decode_keys(&[0xffu8; 300], SPV, AOTH, 4).is_err());
    }

    #[test]
    fn chaves_invalidas_saem_uma_a_uma() {
        let keys = amostra();
        let b = encode_keys(SPV, AOTH, 4, &keys);
        // tabela AOT menor agora (mesmo hash, para isolar o teste do índice): as 2 chaves com aot = 3 saem
        let (v, bad) = decode_keys(&b, SPV, AOTH, 2).unwrap();
        assert_eq!((v.len(), bad), (3, 2));
        assert!(v.iter().all(|(k, _)| k.aot == 0));
        // tabela AOT diferente: só as com AOT saem
        let (v, bad) = decode_keys(&b, SPV, AOTH ^ 1, 4).unwrap();
        assert_eq!((v.len(), bad), (3, 2));
        // bool fora de 0/1 na 1ª chave (byte `vs`, logo depois dos usos)
        let mut x = b.clone();
        x[PK_HEADER + 8] = 2;
        refaz_soma(&mut x);
        let (v, bad) = decode_keys(&x, SPV, AOTH, 4).unwrap();
        assert_eq!((v.len(), bad), (4, 1));
        assert!(!v.iter().any(|(k, _)| *k == keys[0].0));
        // enum fora do domínio: função de profundidade 9 (índice de `CMP`) na 2ª chave
        let mut y = b.clone();
        let dfunc = PK_HEADER + PK_ENTRY + 8 + 1 + 1 + 4 + 1 + 2 + 4 + 3;
        assert_eq!(y[dfunc], 4, "deslocamento de dfunc");
        y[dfunc] = 9;
        refaz_soma(&mut y);
        let (v, bad) = decode_keys(&y, SPV, AOTH, 4).unwrap();
        assert_eq!((v.len(), bad), (4, 1));
        // a gravação também não escreve chave inválida
        let ruim = PipeKey { logic: 16, ..base() };
        let z = encode_keys(SPV, AOTH, 4, &[(ruim, 1), (base(), 2)]);
        assert_eq!(decode_keys(&z, SPV, AOTH, 4).unwrap().0, vec![(base(), 2)]);
    }

    #[test]
    fn ordem_da_pre_compilacao() {
        let spec = FsKey { spec: true, ..FsKey::default() };
        let a = (PipeKey { aot: 1, ..base() }, 100); // AOT: tem reserva
        let b = (base(), 5); // síncrona
        let c = (PipeKey { fs: spec, ..base() }, 50); // especializada: tem reserva
        let d = (PipeKey { vs: false, cull: 0, ..base() }, 9); // síncrona (vértices da CPU)
        let e = (PipeKey { fastz: true, ..base() }, 50); // rápida: tem reserva, empata com c
        let o = prewarm_order(&[a, b, c, d, e]);
        assert_eq!(o, vec![d.0, b.0, a.0, c.0, e.0]);
    }

    fn job(k: PipeKey, warm: bool) -> Job {
        Job { k, vmod: vk::ShaderModule::null(), layout: vk::PipelineLayout::null(), warm }
    }

    #[test]
    fn fila_pedido_do_jogo_passa_na_frente() {
        let k = |m: u32| PipeKey { mask: m, ..base() };
        let mut q = JobQueue::default();
        q.warm.push_back(job(k(1), true));
        q.warm.push_back(job(k(2), true));
        q.warm.push_back(job(k(3), true));
        q.urgent.push_back(job(k(9), false));
        // a thread dos pedidos do jogo nunca pega a pré-compilação
        assert_eq!(q.pop(false).map(|j| j.k), Some(k(9)));
        assert!(q.pop(false).is_none());
        // a da pré-compilação pega na ordem
        assert_eq!(q.pop(true).map(|j| (j.k, j.warm)), Some((k(1), true)));
        assert!(q.running.contains(&k(1)) && q.running.contains(&k(9)));
        // o jogo pediu a 3: vai para a fila do jogo, na frente da 2, e deixa de contar como pré-compilação
        assert!(q.promote(&k(3)));
        assert!(!q.promote(&k(3)));
        assert_eq!(q.pop(true).map(|j| (j.k, j.warm)), Some((k(3), false)));
        // o draw precisa da 2 agora: sai da fila (era da pré-compilação); a 1 está compilando, não sai
        assert_eq!(q.take(&k(2)), Some(true));
        assert_eq!(q.take(&k(1)), None);
        assert!(q.pop(true).is_none());
        // cancelamento esvazia só a pré-compilação
        q.warm.push_back(job(k(4), true));
        q.urgent.push_back(job(k(5), false));
        assert_eq!(q.cancel_warm(), vec![k(4)]);
        assert_eq!(q.pop(true).map(|j| j.k), Some(k(5)));
    }

    /// Espera de verdade entre threads: o draw que precisa de uma chave em compilação espera ESSA terminar.
    #[test]
    fn espera_a_chave_em_compilacao() {
        let sh = std::sync::Arc::new((Mutex::new(JobQueue::default()), std::sync::Condvar::new(), std::sync::Condvar::new()));
        let ndone = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut c = Compiler { sh: sh.clone(), ndone: ndone.clone(), pending: HashMap::new() };
        let k = PipeKey { mask: 7, ..base() };
        sh.0.lock().unwrap().running.insert(k);
        let t = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(30));
            let (m, _, fin) = &*sh;
            let mut q = m.lock().unwrap();
            q.running.remove(&k);
            q.done.push(Done { k, r: Ok(vk::Pipeline::null()), ms: 30.0, at: std::time::Instant::now() });
            ndone.fetch_add(1, Ordering::Release);
            fin.notify_all();
        });
        let t0 = std::time::Instant::now();
        assert_eq!(c.take_or_wait(&k), None);
        assert!(t0.elapsed() >= std::time::Duration::from_millis(25));
        let d = c.take_done();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].k, k);
        t.join().unwrap();
    }

    #[test]
    fn lista_soma_usos_e_respeita_o_teto() {
        let mut log = KeyLog { path: None, aot_hash: AOTH, naot: 4, base: HashMap::new() };
        log.base.insert(base(), 10);
        let mut pipes = HashMap::new();
        pipes.insert(base(), PipeEntry { p: vk::Pipeline::null(), last: 0, uses: 3, warm: true });
        let k2 = PipeKey { mask: 1, ..base() };
        pipes.insert(k2, PipeEntry { p: vk::Pipeline::null(), last: 0, uses: 2, warm: false });
        let mut m = log.merged(&pipes);
        m.sort_by_key(|(_, n)| *n);
        assert_eq!(m, vec![(k2, 2), (base(), 13)]);
        // teto: ficam as mais usadas
        for i in 0..PK_MAX as u32 + 10 {
            log.base.insert(PipeKey { fs: FsKey { spec: true, flags: i, ..FsKey::default() }, ..base() }, 100 + i as u64);
        }
        let m = log.merged(&pipes);
        assert_eq!(m.len(), PK_MAX);
        assert!(m.iter().all(|(_, n)| *n >= 100));
    }
}
