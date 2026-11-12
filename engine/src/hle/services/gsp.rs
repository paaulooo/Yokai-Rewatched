//! `gsp::Gpu` — fila de comandos do GPU, interrupções e framebuffers.
//!
//! Memória compartilhada (0x1000 B, layout do 3dbrew "GSP Shared Memory"):
//! - `0x000 + 0x40·thread`: fila de interrupções {u8 índice, u8 quantidade, u8 erro, u8, u32 perdidas, u8 slots[0x34]}
//! - `0x200 + 0x80·thread`: atualização de framebuffer, tela de cima em +0 e de baixo em +0x40:
//!   {u8 índice, u8 sujo, u16, 2× {u32 ativo, u32 esq, u32 dir, u32 stride, u32 formato, u32 mostrado, u32}}
//! - `0x800 + 0x200·thread`: fila de comandos {u8 índice, u8 quantidade, …} + 15 comandos de 0x20 B
//!
//! Interrupções: 0 PSC0, 1 PSC1, 2 PDC0 (VBlank de cima), 3 PDC1 (de baixo), 4 PPF, 5 P3D, 6 DMA.

use super::{new_event, new_shared, Ipc};
use crate::hle::kernel::{Reset, K};
use crate::hle::{gpu, mem};
use std::sync::Mutex;
use yw1_rt::{r32, r8, w32, w8};

pub const PSC0: u8 = 0;
pub const PSC1: u8 = 1;
pub const PDC0: u8 = 2;
pub const PDC1: u8 = 3;
pub const PPF: u8 = 4;
pub const P3D: u8 = 5;
pub const DMA: u8 = 6;

pub struct Gsp {
    pub shared: u32,
    /// endereço guest do bloco (0 até o jogo mapear)
    pub addr: u32,
    /// evento de interrupção que o jogo registrou (id do objeto)
    pub event: Option<usize>,
    /// espelho dos registradores escritos por WriteHWRegs (índice = offset/4, até `REGS_LEN`), esparso: o jogo escreve
    /// poucas dezenas; o vetor denso de antes comprometia 4 MB
    pub regs: std::collections::BTreeMap<u32, u32>,
}

/// Índices de registrador guardados pelo espelho (0x400000 bytes de E/S + 0x4000 a mais, como o vetor de antes).
const REGS_LEN: u32 = 0x0040_0000 / 4 + 0x1000;

pub static GSP: Mutex<Gsp> = Mutex::new(Gsp {
    shared: 0,
    addr: 0,
    event: None,
    regs: std::collections::BTreeMap::new(),
});

pub fn mapped(addr: u32) {
    GSP.lock().unwrap().addr = addr;
}

/// Põe a interrupção na fila da thread 0 e sinaliza o evento.
pub fn interrupt(id: u8) {
    // VBlank e GPU são threads do host diferentes: uma de cada vez mexe na fila de interrupções
    static ONE: Mutex<()> = Mutex::new(());
    let _one = ONE.lock().unwrap();
    let (addr, ev) = {
        let g = GSP.lock().unwrap();
        (g.addr, g.event)
    };
    if addr == 0 {
        return;
    }
    let q = addr;
    let index = r8(q);
    let count = r8(q + 1);
    if count >= 0x34 {
        // fila cheia: marca como perdida
        let m = r32(q + 4);
        w32(q + 4, m | 1 << id);
    } else {
        let slot = (index as u32 + count as u32) % 0x34;
        w8(q + 0xc + slot, id as u32);
        w8(q + 1, count as u32 + 1);
    }
    // VBlank: atualiza o framebuffer mostrado a partir do bloco de atualização
    if id == PDC0 || id == PDC1 {
        update_framebuffer(addr, (id - PDC0) as u32);
    }
    if let Some(e) = ev {
        K.signal_external(e);
    }
}

/// Trocas de framebuffer apresentadas por tela (quadros que o jogo mostrou).
pub static SWAPS: [std::sync::atomic::AtomicU64; 2] = [const { std::sync::atomic::AtomicU64::new(0) }; 2];

/// Quantos VBlanks o jogo levou entre uma troca da tela de cima e a seguinte: [1, 2, 3, 4 ou mais] (a janela lê e zera a
/// cada 5 s). A 30 quadros/s regulares tudo cai em 2; 2 e 3 misturados = o jogo tropeçando.
pub static SWAP_VB: [std::sync::atomic::AtomicU32; 4] = [const { std::sync::atomic::AtomicU32::new(0) }; 4];
static LAST_SWAP_VB: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Saídas (endereço virtual) dos DisplayTransfers que estão na fila da thread "gpu" e ainda não rodaram.
static PENDING_OUT: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// Trocas de framebuffer adiadas um VBlank porque o buffer novo ainda tinha um DisplayTransfer na fila da GPU (a janela lê
/// e zera a cada 5 s).
pub static SWAPS_ADIADAS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// `YW_SWAP_EARLY=1`: aplica a troca mesmo com o DisplayTransfer do buffer novo ainda na fila (comportamento antigo,
/// só para comparar).
fn swap_early() -> bool {
    static E: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *E.get_or_init(|| std::env::var("YW_SWAP_EARLY").is_ok_and(|v| v != "0"))
}

/// Copia a informação de framebuffer "suja" para o estado de apresentação.
///
/// A GPU daqui é uma thread do host que pode estar atrás do jogo: o jogo pede a troca para o buffer B logo depois de
/// mandar o DisplayTransfer do quadro N para B, e o VBlank chega antes de a thread "gpu" executá-lo. Trocar ali mostraria
/// B ainda com o quadro N−2 — a imagem volta um quadro e depois pula dois (o "vai e volta" ao andar). No console a
/// transferência termina antes do VBlank; aqui a troca espera o próximo VBlank em que a transferência já rodou.
fn update_framebuffer(addr: u32, screen: u32) {
    let b = addr + 0x200 + 0x40 * screen;
    let dirty = r8(b + 1);
    // modo mono: o jogo não publica a troca da tela de cima; usa a sintetizada no último DisplayTransfer dela
    let synth = if dirty == 0 && screen == 0 { gpu::take_synth_top() } else { None };
    if dirty == 0 && synth.is_none() {
        return;
    }
    let info = match synth {
        Some(i) => i,
        None => {
            let idx = r8(b) & 1;
            let fb = b + 4 + 0x1c * idx;
            gpu::FbInfo { active: r32(fb), left: r32(fb + 4), right: r32(fb + 8), stride: r32(fb + 12), format: r32(fb + 16) }
        }
    };
    let left = crate::hle::mem::pa_to_va(info.left).unwrap_or(info.left);
    // modo assíncrono: o present_hires_finish volta logo depois de enviar, e a imagem N× só é publicada quando a GPU
    // termina (thread "vk-publica") — a troca espera também por ela, senão a janela mostraria o nativo
    if !gpu_sync() && (PENDING_OUT.lock().unwrap().contains(&left) || crate::render::vulkan::publish_pending(left)) {
        SWAPS_ADIADAS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if !swap_early() {
            if let Some(i) = synth {
                gpu::put_back_synth_top(i);
            }
            return; // fica "suja": o próximo VBlank tenta de novo
        }
    }
    gpu::set_framebuffer(screen, info);
    SWAPS[screen as usize & 1].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if screen == 0 {
        use std::sync::atomic::Ordering::Relaxed;
        let vb = crate::hle::VBLANKS.load(Relaxed);
        let prev = LAST_SWAP_VB.swap(vb, Relaxed);
        if prev != 0 && vb > prev {
            SWAP_VB[((vb - prev) as usize).min(4) - 1].fetch_add(1, Relaxed);
        }
    }
    w8(b + 1, 0);
}

pub fn request(ipc: &Ipc) {
    match ipc.cmd() {
        // WriteHWRegs(reg, tamanho, desc, ptr)
        0x0001 | 0x0002 => {
            let reg = ipc.get(1);
            let size = ipc.get(2);
            let ptr = if ipc.cmd() == 0x0001 { ipc.get(4) } else { ipc.get(4) };
            let mut g = GSP.lock().unwrap();
            for i in 0..size / 4 {
                let a = (reg + 4 * i) / 4;
                if a < REGS_LEN {
                    g.regs.insert(a, r32(ptr + 4 * i));
                }
            }
            drop(g);
            gpu::hw_regs_written(reg, size, ptr);
            ipc.ok(1, 0);
        }
        // ReadHWRegs(reg, tamanho) → buffer estático 0
        0x0004 => {
            let reg = ipc.get(1);
            let size = ipc.get(2);
            let out = ipc.static_buf(0);
            let g = GSP.lock().unwrap();
            for i in 0..size / 4 {
                let a = (reg + 4 * i) / 4;
                w32(out + 4 * i, g.regs.get(&a).copied().unwrap_or(0));
            }
            ipc.ok(1, 2);
            ipc.set(2, (size << 14) | 2);
            ipc.set(3, out);
        }
        // SetBufferSwap(tela, info[7])
        0x0005 => {
            let screen = ipc.get(1);
            let info = gpu::FbInfo {
                active: ipc.get(2),
                left: ipc.get(3),
                right: ipc.get(4),
                stride: ipc.get(5),
                format: ipc.get(6),
            };
            gpu::set_framebuffer(screen, info);
            ipc.ok(1, 0);
        }
        // FlushDataCache / InvalidateDataCache / SetLcdForceBlack / SetAxiConfigQoSMode / SetPerfLogMode / StoreDataCache
        0x0008 | 0x0009 | 0x000a | 0x000b | 0x0010 | 0x001f | 0x001e => ipc.ok(1, 0),
        // TriggerCmdReqQueue
        0x000c => {
            process_queue();
            ipc.ok(1, 0);
        }
        // RegisterInterruptRelayQueue(flags, desc, evento) → índice da thread, handle do bloco
        0x0013 => {
            let ev_handle = ipc.get(3);
            let ev = K.lock().obj_of(ev_handle);
            let shared = {
                let mut g = GSP.lock().unwrap();
                g.event = ev;
                if g.shared == 0 {
                    g.shared = new_shared("gsp", 0x1000);
                }
                g.shared
            };
            // primeira thread a registrar: resultado 0x2A07 (sucesso, "primeira inicialização")
            ipc.reply(2, 2, 0x2a07);
            ipc.set(2, 0);
            ipc.set(3, 0);
            ipc.set(4, shared);
        }
        0x0014 => ipc.ok(1, 0),
        // AcquireRight / ReleaseRight
        0x0016 | 0x0017 => ipc.ok(1, 0),
        // ImportDisplayCaptureInfo → os dois framebuffers atuais
        0x0018 => {
            ipc.ok(9, 0);
            for i in 2..10 {
                ipc.set(i, 0);
            }
        }
        // SaveVramSysArea / RestoreVramSysArea
        0x0019 | 0x001a => ipc.ok(1, 0),
        _ => super::unknown("gsp::Gpu", ipc),
    }
}

/// Um comando da fila do GX, já copiado da memória compartilhada (a fila é liberada na hora, como o módulo GSP do
/// console faz ao despachar).
#[derive(Clone, Debug)]
enum GxCmd {
    Dma(u32, u32, u32),
    /// endereço, tamanho e as palavras copiadas no despacho
    List(u32, u32, Vec<u32>),
    Fill([u32; 7]),
    Transfer([u32; 5]),
    Copy([u32; 6]),
}

/// Resto de um DisplayTransfer para depois do PPF (imagem N× / interpolação; ver `gpu::display_transfer_native`).
type Post = Option<crate::render::vulkan::PresentChecked>;

/// Executa a parte nativa de um comando e devolve as interrupções que ele gera (na ordem) e o que sobra para depois
/// delas (só DisplayTransfer com imagem N×).
fn run_gx(c: GxCmd) -> (&'static [u8], Post) {
    match c {
        GxCmd::Dma(src, dst, size) => {
            // o que só existe na GPU (superfícies pendentes) volta à memória antes de ser lido; no destino, o que a
            // cópia sobrescreve por inteiro é descartado e o resto devolvido — senão a devolução preguiçosa
            // posterior estragaria o que o DMA escreveu (texturas de interface corrompidas)
            crate::render::before_read(src, src.wrapping_add(size));
            crate::render::before_write(dst, dst.wrapping_add(size));
            let mut buf = vec![0u8; size as usize];
            yw1_rt::read_bytes(src, &mut buf);
            if mem::is_mapped(dst) {
                yw1_rt::write_bytes(dst, &buf);
            }
            (&[DMA], None)
        }
        GxCmd::List(a, n, words) => {
            gpu::submit_cmdlist_words(a, n, &words);
            (&[P3D], None)
        }
        GxCmd::Fill(p) => {
            let control = p[6];
            gpu::memory_fill(p[0], p[1], p[2], control & 0xffff);
            if p[3] != 0 {
                gpu::memory_fill(p[3], p[4], p[5], control >> 16);
                (&[PSC0, PSC1], None)
            } else {
                (&[PSC0], None)
            }
        }
        GxCmd::Transfer(p) => {
            let post = gpu::display_transfer_native(p[0], p[1], p[2], p[3], p[4]);
            (&[PPF], post)
        }
        GxCmd::Copy(p) => {
            gpu::texture_copy(p[0], p[1], p[2], p[3], p[4], p[5]);
            (&[PPF], None)
        }
    }
}

/// `YW_GPU_SYNC=1`: executa os comandos do GX na própria thread do jogo, na hora (comportamento antigo).
fn gpu_sync() -> bool {
    static S: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *S.get_or_init(|| std::env::var("YW_GPU_SYNC").is_ok_and(|v| v != "0"))
}

/// A "GPU" do console: uma thread do host que executa os comandos do GX em ordem e dispara a interrupção de cada um
/// ao terminar — como o hardware, o jogo segue rodando enquanto a GPU trabalha e espera pelas interrupções
/// (P3D/PPF/PSC). Antes tudo rodava dentro da thread do jogo: quadro do jogo = lógica + renderização > 16,7 ms → a
/// espera de VBlank virava 3 períodos (20 quadros/s) em vez de 2 (30).
static GX: Mutex<Option<std::sync::mpsc::Sender<GxCmd>>> = Mutex::new(None);

/// Tempo (ns) que a "GPU" passou executando comandos do GX (soma desde o boot), incluindo a parte pós-PPF do
/// DisplayTransfer: a interpolação de quadros desconta disto o tempo dos próprios intermediários (que rodam dentro da
/// parte pós) e o que sobra no período do jogo é o orçamento deles (`render/interp.rs`). Se esta soma deixasse de fora
/// a parte pós, o desconto dos intermediários levaria o orçamento a zero.
pub static GPU_BUSY_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Só a parte pós-PPF do DisplayTransfer (imagem N×, intermediários da interpolação, publicação), também contida em
/// `GPU_BUSY_NS`: a linha de desempenho mostra "pós-transfer" à parte.
pub static GPU_POST_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// `YW_PPF_EARLY` (padrão ligado): o PPF de um DisplayTransfer é sinalizado ao jogo logo depois da parte nativa, e a
/// imagem N× / intermediários rodam depois, com o jogo já seguindo. `YW_PPF_EARLY=0`: ordem antiga (tudo antes do PPF).
fn ppf_early() -> bool {
    static E: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *E.get_or_init(|| std::env::var("YW_PPF_EARLY").map_or(true, |v| v != "0"))
}

fn run_timed(c: GxCmd) -> (&'static [u8], Post) {
    let t = std::time::Instant::now();
    let r = run_gx(c);
    GPU_BUSY_NS.fetch_add(t.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
    r
}

/// A parte pós-PPF de um DisplayTransfer (se houver).
fn run_post(post: Post) {
    let Some(c) = post else { return };
    let t = std::time::Instant::now();
    crate::render::present_hires_finish(c);
    let ns = t.elapsed().as_nanos() as u64;
    GPU_BUSY_NS.fetch_add(ns, std::sync::atomic::Ordering::Relaxed);
    GPU_POST_NS.fetch_add(ns, std::sync::atomic::Ordering::Relaxed);
}

fn dispatch(c: GxCmd) {
    if gpu_sync() {
        // na thread do jogo, que de qualquer forma só segue depois de tudo: ordem antiga (adiantar o PPF não ganha nada)
        let (ints, post) = run_timed(c);
        run_post(post);
        for &i in ints {
            interrupt(i);
        }
        return;
    }
    if let GxCmd::Transfer(p) = &c {
        PENDING_OUT.lock().unwrap().push(p[1]);
    }
    let mut g = GX.lock().unwrap();
    let tx = g.get_or_insert_with(|| {
        let (tx, rx) = std::sync::mpsc::channel::<GxCmd>();
        std::thread::Builder::new()
            .name("gpu".into())
            .spawn(move || {
                // Ordem com o PPF adiantado: parte nativa → PPF (o jogo segue) → parte pós (imagem N×, intermediários)
                // → sai de PENDING_OUT. A saída só deixa PENDING_OUT depois da parte pós: a troca de framebuffer para
                // ela (`update_framebuffer`) continua esperando a imagem N× ser publicada — trocar antes faria a janela
                // mostrar o nativo (o hash da imagem N× anterior desse buffer não confere mais) e quebraria a cadência
                // da interpolação, que se ancora na troca.
                for c in rx {
                    let out = if let GxCmd::Transfer(p) = &c { Some(p[1]) } else { None };
                    let (ints, post) = run_timed(c);
                    let done = || {
                        if let Some(o) = out {
                            let mut v = PENDING_OUT.lock().unwrap();
                            if let Some(i) = v.iter().position(|&a| a == o) {
                                v.remove(i);
                            }
                        }
                    };
                    if ppf_early() {
                        for &i in ints {
                            interrupt(i);
                        }
                        run_post(post);
                        done();
                    } else {
                        // ordem antiga: tudo pronto, sai da fila e só então o PPF
                        run_post(post);
                        done();
                        for &i in ints {
                            interrupt(i);
                        }
                    }
                }
            })
            .expect("thread da GPU");
        tx
    });
    let _ = tx.send(c);
}

/// Despacha a fila de comandos da thread 0 para a GPU.
fn process_queue() {
    let addr = GSP.lock().unwrap().addr;
    if addr == 0 {
        return;
    }
    let q = addr + 0x800;
    loop {
        let index = r8(q);
        let count = r8(q + 1);
        if count == 0 {
            break;
        }
        let cmd = q + 0x20 + 0x20 * index;
        let id = r8(cmd) as u8;
        let p = |i: u32| r32(cmd + 4 + 4 * i);
        let c = match id {
            0x00 => Some(GxCmd::Dma(p(0), p(1), p(2))),
            0x01 => Some(GxCmd::List(p(0), p(1), crate::pica::read_list(p(0), p(1)))),
            0x02 => Some(GxCmd::Fill([p(0), p(1), p(2), p(3), p(4), p(5), p(6)])),
            0x03 => Some(GxCmd::Transfer([p(0), p(1), p(2), p(3), p(4)])),
            0x04 => Some(GxCmd::Copy([p(0), p(1), p(2), p(3), p(4), p(5)])),
            0x05 => None,
            _ => {
                eprintln!("[gsp] comando de fila {id:#x} desconhecido");
                None
            }
        };
        w8(q, (index as u32 + 1) % 15);
        w8(q + 1, count as u32 - 1);
        if let Some(c) = c {
            dispatch(c);
        }
    }
}

/// Cria o evento interno não usado (mantém a API simétrica com outros serviços).
#[allow(dead_code)]
fn _event() -> (u32, usize) {
    new_event(Reset::OneShot, "gsp")
}
