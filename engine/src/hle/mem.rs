//! Espaço de endereços guest: uma reserva de 4 GB no host (`VirtualAlloc(MEM_RESERVE)`), com commit por região
//! conforme o jogo mapeia memória. Endereço guest `va` = `base + va` (ver `yw1_rt::r32`).
//!
//! Mapa (3DS antigo, aplicativo de 64 MB; endereços usados pelo kernel do console):
//! - `0x00100000` código: `.text`, `.rodata`, `.data` (do `code.bin`) e `.bss` (zerado)
//! - `0x08000000` heap (`ControlMemory` COMMIT)
//! - `0x10000000 - pilha` pilha da thread principal (topo em 0x10000000)
//! - `0x10000000+` blocos de memória compartilhada mapeados pelo kernel
//! - `0x14000000` heap linear (`ControlMemory` LINEAR; PA 0x20000000 — o mesmo das capturas do Azahar)
//! - `0x1F000000` VRAM (6 MB, PA 0x18000000)
//! - `0x1FF80000` página de configuração, `0x1FF81000` página compartilhada, `0x1FF82000` TLS das threads

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

pub const CODE: u32 = 0x0010_0000;
pub const HEAP: u32 = 0x0800_0000;
pub const STACK_TOP: u32 = 0x1000_0000;
pub const SHARED: u32 = 0x1000_0000;
pub const LINEAR: u32 = 0x1400_0000;
pub const LINEAR_PA: u32 = 0x2000_0000;
pub const VRAM: u32 = 0x1F00_0000;
pub const VRAM_PA: u32 = 0x1800_0000;
pub const VRAM_SIZE: u32 = 0x0060_0000;
pub const CONFIG: u32 = 0x1FF8_0000;
pub const SHARED_PAGE: u32 = 0x1FF8_1000;
pub const TLS: u32 = 0x1FF8_2000;
pub const TLS_SIZE: u32 = 0x200;
/// FCRAM disponível para o aplicativo (heap + linear) no modo de 64 MB
pub const APP_MEMORY: u32 = 64 << 20;

#[cfg(windows)]
mod os {
    #[link(name = "kernel32")]
    extern "system" {
        fn VirtualAlloc(addr: *mut u8, size: usize, kind: u32, prot: u32) -> *mut u8;
    }
    const MEM_COMMIT: u32 = 0x1000;
    const MEM_RESERVE: u32 = 0x2000;
    const PAGE_NOACCESS: u32 = 0x01;
    const PAGE_READWRITE: u32 = 0x04;

    pub fn reserve(size: usize) -> *mut u8 {
        // SAFETY: reserva nova, sem endereço pedido
        unsafe { VirtualAlloc(std::ptr::null_mut(), size, MEM_RESERVE, PAGE_NOACCESS) }
    }
    pub fn commit(p: *mut u8, size: usize) -> bool {
        // SAFETY: `p..p+size` está dentro da reserva
        unsafe { !VirtualAlloc(p, size, MEM_COMMIT, PAGE_READWRITE).is_null() }
    }
}

#[cfg(not(windows))]
mod os {
    // Fora do Windows: mmap anônimo PROT_NONE + mprotect (o alvo do port é Windows; mantido simples).
    extern "C" {
        fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
        fn mprotect(addr: *mut u8, len: usize, prot: i32) -> i32;
    }
    pub fn reserve(size: usize) -> *mut u8 {
        // MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE
        let p = unsafe { mmap(std::ptr::null_mut(), size, 0, 0x02 | 0x20 | 0x4000, -1, 0) };
        if p as isize == -1 {
            std::ptr::null_mut()
        } else {
            p
        }
    }
    pub fn commit(p: *mut u8, size: usize) -> bool {
        unsafe { mprotect(p, size, 3) == 0 }
    }
}

struct Regions {
    /// (início, fim) das regiões com commit, para depuração e para `QueryMemory`
    list: Vec<(u32, u32, &'static str)>,
    /// fim atual do heap e do linear
    heap_end: u32,
    linear_end: u32,
}

static REGIONS: Mutex<Regions> = Mutex::new(Regions {
    list: Vec::new(),
    heap_end: HEAP,
    linear_end: LINEAR,
});

/// Commit sob demanda (Windows; `YW_MEM_LAZY=0` desliga): heap, linear e pilha principal só ganham as páginas
/// marcadas em `PAGES`; o commit de cada página (4 KB) acontece no primeiro acesso, pelo tratador de exceções
/// (`commit_on_fault`, chamado de `install_fault_handler` em `mod.rs`). O jogo pede ~64 MB de heap+linear e toca
/// pouco mais da metade: o resto deixava de contar na RAM privada (commit) à toa. Páginas novas vêm zeradas como
/// antes. Acessos do kernel do Windows (ReadFile direto na memória guest) não passam pelo tratador: o HLE sempre lê
/// arquivos para um buffer próprio e copia (`services/fs.rs`).
static LAZY: AtomicBool = AtomicBool::new(false);
/// páginas que receberam commit no primeiro acesso
pub static LAZY_PAGES: AtomicU64 = AtomicU64::new(0);

/// Regiões que podem ficar sem commit até o primeiro acesso (as demais: código, VRAM, blocos dos serviços e páginas do
/// sistema, recebem commit na hora — são pequenas ou escritas logo pelo HLE).
fn lazy_region(what: &str) -> bool {
    matches!(what, "heap" | "linear" | "pilha principal")
}

/// Reserva os 4 GB e liga o ponteiro base do runtime. Chamar uma vez, antes de qualquer código guest.
pub fn init() {
    let p = os::reserve(1usize << 32);
    assert!(!p.is_null(), "[hle] não consegui reservar 4 GB de endereços virtuais");
    // SAFETY: a reserva vive até o fim do processo
    unsafe { yw1_rt::set_base(p) };
    LAZY.store(cfg!(windows) && std::env::var("YW_MEM_LAZY").map_or(true, |v| v != "0"), Ordering::Relaxed);
}

/// Tratador de falha de acesso (`mod.rs`): `host` caiu numa página guest marcada mas ainda sem commit? Faz o commit
/// da página e devolve `true` (a instrução é repetida). Não aloca nem trava nada (roda dentro da exceção, em qualquer
/// thread).
pub fn commit_on_fault(host: usize) -> bool {
    if !LAZY.load(Ordering::Relaxed) {
        return false;
    }
    let base = yw1_rt::base() as usize;
    if host < base || host - base >= 1usize << 32 {
        return false;
    }
    let va = (host - base) as u32;
    if !is_mapped(va) {
        return false;
    }
    // duas threads na mesma página: os dois commits dão certo (commit de página já comprometida não muda nada)
    let ok = os::commit(yw1_rt::host_ptr(va & !0xfff), 0x1000);
    if ok {
        LAZY_PAGES.fetch_add(1, Ordering::Relaxed);
    }
    ok
}

/// `ControlMemory` FREE: o commit é mantido [H]; só conta, para o mapa de memória (`memmap.rs`).
pub static FREES: AtomicU64 = AtomicU64::new(0);
pub static FREED_BYTES: AtomicU64 = AtomicU64::new(0);
pub fn free(_addr: u32, size: u32) {
    FREES.fetch_add(1, Ordering::Relaxed);
    FREED_BYTES.fetch_add(size as u64, Ordering::Relaxed);
}

/// Faz commit de `[va, va+size)` (arredondado a páginas de 4 KB); com commit sob demanda (`LAZY`), heap/linear/pilha
/// só ficam marcados e o commit vem no primeiro acesso.
pub fn map(va: u32, size: u32, what: &'static str) {
    let start = va & !0xfff;
    let end = (va as u64 + size as u64 + 0xfff) & !0xfff;
    let len = (end - start as u64) as usize;
    if !(LAZY.load(Ordering::Relaxed) && lazy_region(what)) {
        let ok = os::commit(yw1_rt::host_ptr(start), len);
        assert!(ok, "[hle] commit falhou em {start:#x} (+{len:#x}) para {what}");
    }
    REGIONS
        .lock()
        .unwrap()
        .list
        .push((start, end.min(u32::MAX as u64) as u32, what));
    // páginas marcadas DEPOIS do commit: quem vê o bit pode acessar a página (sob demanda: o acesso faz o commit)
    for pg in (start as u64 >> 12)..(end >> 12) {
        PAGES[(pg >> 6) as usize].fetch_or(1 << (pg & 63), Ordering::Release);
    }
}

/// Páginas de 4 KB com commit (1 bit por página: 4 GB / 4 KB = 2^20 bits = 128 KB). `is_mapped` é chamado a cada
/// acesso do HLE/GPU à memória guest (`guest_slice`, transferências, fills): antes era mutex + varredura da lista de
/// regiões. Só `map` liga bits (o HLE não desfaz commits).
static PAGES: [AtomicU64; 1 << 14] = [const { AtomicU64::new(0) }; 1 << 14];

pub fn is_mapped(va: u32) -> bool {
    let pg = va >> 12;
    PAGES[(pg >> 6) as usize].load(Ordering::Acquire) & (1 << (pg & 63)) != 0
}

/// `ControlMemory` COMMIT (op 3) no heap: devolve o endereço.
pub fn heap_commit(addr: u32, size: u32) -> u32 {
    let mut r = REGIONS.lock().unwrap();
    let a = if addr == 0 { r.heap_end } else { addr };
    drop(r);
    map(a, size, "heap");
    r = REGIONS.lock().unwrap();
    r.heap_end = r.heap_end.max(a + size);
    a
}

/// `ControlMemory` COMMIT | LINEAR: aloca no heap linear e devolve o endereço.
pub fn linear_commit(size: u32) -> u32 {
    let mut r = REGIONS.lock().unwrap();
    let a = r.linear_end;
    r.linear_end += (size + 0xfff) & !0xfff;
    drop(r);
    map(a, size, "linear");
    a
}

/// Regiões com commit (início, fim, para quê), na ordem dos pedidos — para o mapa de memória (`memmap.rs`).
pub fn regions() -> Vec<(u32, u32, &'static str)> {
    REGIONS.lock().unwrap().list.clone()
}

/// Quanto o aplicativo já usa (para `GetResourceLimitCurrentValues`).
pub fn used() -> u32 {
    let r = REGIONS.lock().unwrap();
    (r.heap_end - HEAP) + (r.linear_end - LINEAR)
}

/// Físico → virtual (o que o GPU recebe nas listas de comando).
pub fn pa_to_va(pa: u32) -> Option<u32> {
    if (LINEAR_PA..LINEAR_PA + 0x0800_0000).contains(&pa) {
        Some(pa - LINEAR_PA + LINEAR)
    } else if (VRAM_PA..VRAM_PA + VRAM_SIZE).contains(&pa) {
        Some(pa - VRAM_PA + VRAM)
    } else {
        None
    }
}

/// Carrega o `code.bin` (segmentos do exheader) e prepara pilha, VRAM e páginas do sistema.
pub fn load_code(code: &[u8], bss_end: u32) {
    let end = CODE + code.len() as u32;
    map(CODE, bss_end - CODE, "código");
    yw1_rt::write_bytes(CODE, code);
    // .bss já vem zerado do commit
    let _ = end;
    map(VRAM, VRAM_SIZE, "vram");
    map(CONFIG, 0x3000 + 0x200 * 64, "páginas do sistema + TLS");
}
