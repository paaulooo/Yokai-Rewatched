//! Mapa da memória do processo (`YW_MEM_MAP=1`, só leitura): de onde vem a "RAM privada" da linha `[hle]`.
//!
//! Percorre o espaço de endereços com `VirtualQuery` (kernel32, declarado à mão) e soma o que está comprometido
//! (`MEM_COMMIT`), separado por tipo: `MEM_PRIVATE` (o que conta em `PrivateUsage`), `MEM_MAPPED` (seções: arquivos e
//! memória compartilhada, inclusive mapeamentos do driver) e `MEM_IMAGE` (DLLs/exe). O privado é agrupado por dono:
//! - espaço guest (a reserva de 4 GB de `mem.rs`), por faixa de endereço guest (código/.bss, heap, pilha, blocos
//!   compartilhados, linear, VRAM, páginas do sistema);
//! - heaps do processo (`GetProcessHeaps` + `HeapWalk`: regiões e blocos grandes alocados à parte), onde moram o
//!   alocador do Rust (`System` = `HeapAlloc` no heap do processo) e o CRT;
//! - pilhas de threads (alocações com página de guarda);
//! - o resto (driver Vulkan, etc.), com as maiores alocações listadas e o tipo de cache (WC = combinação de escrita,
//!   típico de memória do dispositivo mapeada).
//!
//! Também liga um contador no alocador global (`Counting`): bytes vivos alocados pelo Rust por faixa de tamanho, pico,
//! e uma tabela dos blocos ≥ 1 MB (tamanhos) para reconhecer os donos. O contador só trabalha com `YW_MEM_MAP`
//! definido (lido direto do ambiente na primeira alocação, sem alocar); desligado custa uma leitura atômica.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicI64, AtomicU8, AtomicUsize, Ordering};

// ---------------------------------------------------------------- alocador contador

/// 0 = ainda não leu o ambiente, 1 = ligado, 2 = desligado
static STATE: AtomicU8 = AtomicU8::new(0);
/// bytes vivos por faixa de tamanho do pedido: < 4 KB, < 64 KB, < 1 MB, < 16 MB, ≥ 16 MB
static LIVE: [AtomicI64; 5] = [const { AtomicI64::new(0) }; 5];
static COUNT: [AtomicI64; 5] = [const { AtomicI64::new(0) }; 5];
static PEAK: AtomicI64 = AtomicI64::new(0);
static TOTAL: AtomicI64 = AtomicI64::new(0);
/// blocos ≥ 1 MB vivos: (endereço, tamanho); endereço 0 = livre
const BIG_SLOTS: usize = 1024;
static BIG_ADDR: [AtomicUsize; BIG_SLOTS] = [const { AtomicUsize::new(0) }; BIG_SLOTS];
static BIG_SIZE: [AtomicUsize; BIG_SLOTS] = [const { AtomicUsize::new(0) }; BIG_SLOTS];
const BIG: usize = 1 << 20;

fn detect() -> bool {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        extern "system" {
            fn GetEnvironmentVariableW(name: *const u16, buf: *mut u16, n: u32) -> u32;
        }
        let name: [u16; 11] = [
            b'Y' as u16, b'W' as u16, b'_' as u16, b'M' as u16, b'E' as u16, b'M' as u16, b'_' as u16, b'M' as u16,
            b'A' as u16, b'P' as u16, 0,
        ];
        let mut buf = [0u16; 8];
        // SAFETY: nome terminado em 0, buffer de 8 posições; não aloca (pode ser chamado de dentro do alocador)
        let n = unsafe { GetEnvironmentVariableW(name.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
        n > 0 && !(n == 1 && buf[0] == b'0' as u16)
    }
    #[cfg(not(windows))]
    {
        extern "C" {
            fn getenv(name: *const u8) -> *const u8;
        }
        // SAFETY: getenv não aloca; nome terminado em 0
        let p = unsafe { getenv(b"YW_MEM_MAP\0".as_ptr()) };
        // SAFETY: getenv devolve nulo ou uma string C válida
        !p.is_null() && unsafe { *p } != 0 && !(unsafe { *p } == b'0' && unsafe { *p.add(1) } == 0)
    }
}

#[inline(always)]
fn on() -> bool {
    match STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let e = detect();
            STATE.store(if e { 1 } else { 2 }, Ordering::Relaxed);
            e
        }
    }
}

/// O diagnóstico está ligado (`YW_MEM_MAP`)?
pub fn enabled() -> bool {
    on()
}

fn bucket(n: usize) -> usize {
    match n {
        0..=4095 => 0,
        4096..=65535 => 1,
        65536..=1048575 => 2,
        1048576..=16777215 => 3,
        _ => 4,
    }
}

fn add(p: *mut u8, n: usize) {
    let b = bucket(n);
    LIVE[b].fetch_add(n as i64, Ordering::Relaxed);
    COUNT[b].fetch_add(1, Ordering::Relaxed);
    let t = TOTAL.fetch_add(n as i64, Ordering::Relaxed) + n as i64;
    PEAK.fetch_max(t, Ordering::Relaxed);
    if n >= BIG {
        for i in 0..BIG_SLOTS {
            if BIG_ADDR[i].compare_exchange(0, p as usize, Ordering::AcqRel, Ordering::Relaxed).is_ok() {
                BIG_SIZE[i].store(n, Ordering::Release);
                break;
            }
        }
    }
}

fn sub(p: *mut u8, n: usize) {
    let b = bucket(n);
    LIVE[b].fetch_sub(n as i64, Ordering::Relaxed);
    COUNT[b].fetch_sub(1, Ordering::Relaxed);
    TOTAL.fetch_sub(n as i64, Ordering::Relaxed);
    if n >= BIG {
        for i in 0..BIG_SLOTS {
            if BIG_ADDR[i].load(Ordering::Acquire) == p as usize {
                BIG_SIZE[i].store(0, Ordering::Release);
                BIG_ADDR[i].store(0, Ordering::Release);
                break;
            }
        }
    }
}

/// `System` com contadores (ligados só com `YW_MEM_MAP`).
pub struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = System.alloc(l);
        if !p.is_null() && on() {
            add(p, l.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = System.alloc_zeroed(l);
        if !p.is_null() && on() {
            add(p, l.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        if on() {
            sub(p, l.size());
        }
        System.dealloc(p, l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        let q = System.realloc(p, l, n);
        if !q.is_null() && on() {
            sub(p, l.size());
            add(q, n);
        }
        q
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

// ---------------------------------------------------------------- VirtualQuery

#[cfg(windows)]
mod os {
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    pub struct Mbi {
        pub base: usize,
        pub alloc_base: usize,
        pub alloc_protect: u32,
        pub partition: u16,
        pub region: usize,
        pub state: u32,
        pub protect: u32,
        pub ty: u32,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct HeapEntry {
        pub data: usize,
        pub cb_data: u32,
        pub overhead: u8,
        pub region_index: u8,
        pub flags: u16,
        /// união Block/Region: na Region, os 4 primeiros bytes são `dwCommittedSize` e os 4 seguintes
        /// `dwUnCommittedSize`
        pub u: [u32; 6],
    }
    #[link(name = "kernel32")]
    extern "system" {
        pub fn VirtualQuery(p: usize, out: *mut Mbi, n: usize) -> usize;
        pub fn GetProcessHeaps(n: u32, out: *mut usize) -> u32;
        pub fn GetProcessHeap() -> usize;
        pub fn HeapLock(h: usize) -> i32;
        pub fn HeapUnlock(h: usize) -> i32;
        pub fn HeapWalk(h: usize, e: *mut HeapEntry) -> i32;
        pub fn GetCurrentProcess() -> *mut u8;
        pub fn K32QueryWorkingSetEx(p: *mut u8, v: *mut WsEx, cb: u32) -> i32;
    }
    /// PSAPI_WORKING_SET_EX_INFORMATION: endereço + atributos (bit 0 = Valid: a página está no conjunto de trabalho,
    /// isto é, ocupa RAM física agora)
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    pub struct WsEx {
        pub va: usize,
        pub attr: usize,
    }

    /// Bytes residentes (páginas válidas no conjunto de trabalho) em `[a, a+len)`; `buf` é reaproveitado.
    pub fn resident(a: usize, len: usize, buf: &mut Vec<WsEx>) -> u64 {
        let mut n = 0u64;
        let mut p = a;
        let end = a + len;
        while p < end {
            let k = ((end - p) >> 12).min(buf.len());
            for (i, e) in buf[..k].iter_mut().enumerate() {
                *e = WsEx { va: p + (i << 12), attr: 0 };
            }
            // SAFETY: `buf[..k]` preenchido; só consulta
            if unsafe { K32QueryWorkingSetEx(GetCurrentProcess(), buf.as_mut_ptr(), (k * std::mem::size_of::<WsEx>()) as u32) } == 0 {
                return 0;
            }
            n += buf[..k].iter().filter(|e| e.attr & 1 != 0).count() as u64 * 4096;
            p += k << 12;
        }
        n
    }
    pub const MEM_COMMIT: u32 = 0x1000;
    pub const MEM_PRIVATE: u32 = 0x20000;
    pub const MEM_MAPPED: u32 = 0x40000;
    pub const MEM_IMAGE: u32 = 0x100_0000;
    pub const PAGE_GUARD: u32 = 0x100;
    pub const PAGE_NOCACHE: u32 = 0x200;
    pub const PAGE_WRITECOMBINE: u32 = 0x400;
    pub const HEAP_REGION: u16 = 0x1;
    pub const HEAP_BUSY: u16 = 0x4;

    pub fn query(p: usize) -> Option<Mbi> {
        let mut m = Mbi::default();
        // SAFETY: consulta de endereço, sem acesso à memória
        let n = unsafe { VirtualQuery(p, &mut m, std::mem::size_of::<Mbi>()) };
        (n != 0).then_some(m)
    }
}

const MB: f64 = 1024.0 * 1024.0;

/// Faixas do espaço guest (endereço guest inicial, nome); cada uma vai até o início da seguinte.
fn guest_bands() -> [(u64, &'static str); 9] {
    use super::mem;
    [
        (0, "abaixo do código"),
        (mem::CODE as u64, "código + .data + .bss"),
        (mem::HEAP as u64, "heap (ControlMemory) + pilha principal"),
        (mem::SHARED as u64, "blocos compartilhados (serviços, RAM do DSP)"),
        (mem::LINEAR as u64, "heap linear"),
        (mem::VRAM as u64, "VRAM"),
        ((mem::VRAM + mem::VRAM_SIZE) as u64, "entre VRAM e sistema"),
        (mem::CONFIG as u64, "páginas do sistema + TLS"),
        (1u64 << 32, "fim"),
    ]
}

/// Texto do mapa (várias linhas, começando por `[mem]`). Vazio fora do Windows.
pub fn report() -> String {
    #[cfg(not(windows))]
    {
        String::new()
    }
    #[cfg(windows)]
    {
        report_windows()
    }
}

#[cfg(windows)]
fn report_windows() -> String {
    use os::*;
    use std::fmt::Write;

    // 1) heaps: bases de alocação das regiões e dos blocos grandes (fora das regiões). Os vetores são alocados ANTES de
    // travar o heap (crescer durante o HeapWalk mexeria no próprio heap que está sendo percorrido).
    let mut heaps = [0usize; 256];
    // SAFETY: buffer de 256 posições
    let nh = (unsafe { GetProcessHeaps(heaps.len() as u32, heaps.as_mut_ptr()) } as usize).min(heaps.len());
    let proc_heap = unsafe { GetProcessHeap() };
    // (base da alocação, índice do heap)
    let mut heap_bases: Vec<(usize, usize)> = Vec::with_capacity(16384);
    // por heap: bytes ocupados (blocos em uso), comprometido nas regiões, blocos grandes à parte
    let mut heap_busy = vec![0u64; nh];
    let mut heap_regions = vec![0u64; nh];
    let mut heap_large = vec![0u64; nh];
    for (hi, &h) in heaps[..nh].iter().enumerate() {
        // SAFETY: handle devolvido por GetProcessHeaps; HeapLock/HeapUnlock em par
        if unsafe { HeapLock(h) } == 0 {
            continue;
        }
        let mut e = HeapEntry { data: 0, cb_data: 0, overhead: 0, region_index: 0, flags: 0, u: [0; 6] };
        let mut regions: [(usize, usize); 512] = [(0, 0); 512];
        let mut nreg = 0usize;
        // SAFETY: `e` começa com data = 0 (início do percurso), como pede a documentação
        while unsafe { HeapWalk(h, &mut e) } != 0 {
            if e.flags & HEAP_REGION != 0 {
                let committed = e.u[0] as u64;
                heap_regions[hi] += committed;
                if nreg < regions.len() {
                    regions[nreg] = (e.data, e.data + committed as usize + e.u[1] as usize);
                    nreg += 1;
                }
                if let Some(m) = query(e.data) {
                    if heap_bases.len() < heap_bases.capacity() {
                        heap_bases.push((m.alloc_base, hi));
                    }
                }
            } else if e.flags & HEAP_BUSY != 0 {
                heap_busy[hi] += e.cb_data as u64;
                let inside = regions[..nreg].iter().any(|&(a, b)| (a..b).contains(&e.data));
                if !inside && e.cb_data as usize >= 64 * 1024 {
                    heap_large[hi] += e.cb_data as u64;
                    if let Some(m) = query(e.data) {
                        if heap_bases.len() < heap_bases.capacity() {
                            heap_bases.push((m.alloc_base, hi));
                        }
                    }
                }
            }
        }
        // SAFETY: par do HeapLock acima
        unsafe { HeapUnlock(h) };
    }
    heap_bases.sort_unstable();
    heap_bases.dedup_by_key(|x| x.0);
    let heap_of = |base: usize| heap_bases.binary_search_by_key(&base, |x| x.0).ok().map(|i| heap_bases[i].1);

    // 2) percurso do espaço de endereços
    let gbase = yw1_rt::base() as usize;
    let gend = gbase + (1usize << 32);
    let bands = guest_bands();
    let mut guest_band = [0u64; 8];
    let (mut private, mut mapped, mut image) = (0u64, 0u64, 0u64);
    let (mut mapped_wc, mut private_wc, mut private_nc) = (0u64, 0u64, 0u64);
    let mut heap_priv = vec![0u64; nh];
    // residente (no conjunto de trabalho = RAM física agora): guest, heaps, resto, pilhas+outros
    let mut wsbuf = vec![WsEx::default(); 16384];
    let (mut ws_guest, mut ws_heap, mut ws_wc, mut ws_other) = (0u64, 0u64, 0u64, 0u64);
    let mut ws_mapped = 0u64;
    // alocações privadas fora do guest e dos heaps: base → (comprometido, tem guarda, proteção, WC/NC)
    let mut others: Vec<(usize, u64, bool, u32)> = Vec::new();
    let mut mapped_list: Vec<(usize, u64, u32)> = Vec::new();
    let mut p = 0x10000usize;
    while p < 0x7FFF_FFFF_0000 {
        let Some(m) = query(p) else { break };
        let next = m.base + m.region.max(0x1000);
        if m.state == MEM_COMMIT {
            let sz = m.region as u64;
            match m.ty {
                MEM_PRIVATE => {
                    private += sz;
                    let r = resident(m.base, m.region, &mut wsbuf);
                    if m.protect & PAGE_WRITECOMBINE != 0 {
                        private_wc += sz;
                    }
                    if m.protect & PAGE_NOCACHE != 0 {
                        private_nc += sz;
                    }
                    if (gbase..gend).contains(&m.base) {
                        ws_guest += r;
                        // pode cruzar faixas: corta por faixa
                        let (a, b) = ((m.base - gbase) as u64, (m.base - gbase) as u64 + sz);
                        for i in 0..8 {
                            let (lo, hi) = (bands[i].0.max(a), bands[i + 1].0.min(b));
                            if hi > lo {
                                guest_band[i] += hi - lo;
                            }
                        }
                    } else if let Some(hi) = heap_of(m.alloc_base) {
                        heap_priv[hi] += sz;
                        ws_heap += r;
                    } else {
                        if m.protect & PAGE_WRITECOMBINE != 0 {
                            ws_wc += r;
                        } else {
                            ws_other += r;
                        }
                        let guard = m.protect & PAGE_GUARD != 0;
                        match others.last_mut() {
                            Some(o) if o.0 == m.alloc_base => {
                                o.1 += sz;
                                o.2 |= guard;
                                o.3 |= m.protect;
                            }
                            _ => others.push((m.alloc_base, sz, guard, m.protect)),
                        }
                    }
                }
                MEM_MAPPED => {
                    mapped += sz;
                    ws_mapped += resident(m.base, m.region, &mut wsbuf);
                    if m.protect & PAGE_WRITECOMBINE != 0 {
                        mapped_wc += sz;
                    }
                    match mapped_list.last_mut() {
                        Some(o) if o.0 == m.alloc_base => o.1 += sz,
                        _ => mapped_list.push((m.alloc_base, sz, m.protect)),
                    }
                }
                MEM_IMAGE => image += sz,
                _ => {}
            }
        }
        if next <= p {
            break;
        }
        p = next;
    }
    // pilhas de thread: a página de guarda fica no meio da alocação; o percurso acima já juntou por base
    let stacks: u64 = others.iter().filter(|o| o.2).map(|o| o.1).sum();
    let nstacks = others.iter().filter(|o| o.2).count();
    others.retain(|o| !o.2);
    let rest: u64 = others.iter().map(|o| o.1).sum();
    others.sort_by(|a, b| b.1.cmp(&a.1));
    mapped_list.sort_by(|a, b| b.1.cmp(&a.1));
    let guest: u64 = guest_band.iter().sum();
    let heaps_total: u64 = heap_priv.iter().sum();

    let mut s = String::new();
    let _ = writeln!(
        s,
        "[mem] comprometido: privado {:.1} MB (escrita combinada {:.1} · sem cache {:.1}) · mapeado (seções) {:.1} MB (escrita combinada {:.1}) · imagens {:.1} MB",
        private as f64 / MB,
        private_wc as f64 / MB,
        private_nc as f64 / MB,
        mapped as f64 / MB,
        mapped_wc as f64 / MB,
        image as f64 / MB
    );
    let _ = writeln!(
        s,
        "[mem]   privado = guest {:.1} + heaps {:.1} + pilhas {:.1} ({nstacks}) + resto {:.1} ({} alocações)",
        guest as f64 / MB,
        heaps_total as f64 / MB,
        stacks as f64 / MB,
        rest as f64 / MB,
        others.len()
    );
    let _ = writeln!(
        s,
        "[mem]   residente (RAM física agora, MB): guest {:.1} · heaps {:.1} · resto com escrita combinada {:.1} · pilhas e resto comum {:.1} · mapeado {:.1}",
        ws_guest as f64 / MB,
        ws_heap as f64 / MB,
        ws_wc as f64 / MB,
        ws_other as f64 / MB,
        ws_mapped as f64 / MB
    );
    let mut g = String::new();
    for i in 0..8 {
        if guest_band[i] > 0 {
            let _ = write!(g, " · {} {:.2}", bands[i].1, guest_band[i] as f64 / MB);
        }
    }
    let _ = writeln!(s, "[mem]   guest (MB):{g}");
    let mut regs: Vec<(&str, u64)> = Vec::new();
    for (a, b, w) in super::mem::regions() {
        let n = (b - a) as u64;
        match regs.iter_mut().find(|r| r.0 == w) {
            Some(r) => r.1 += n,
            None => regs.push((w, n)),
        }
    }
    regs.sort_by(|a, b| b.1.cmp(&a.1));
    let r: Vec<String> = regs.iter().map(|(w, n)| format!("{w} {:.2}", *n as f64 / MB)).collect();
    let _ = writeln!(s, "[mem]   guest por pedido de commit (MB): {}", r.join(" · "));
    let _ = writeln!(
        s,
        "[mem]   guest: páginas com commit sob demanda {:.1} MB · ControlMemory FREE {} ({:.1} MB, commit mantido)",
        super::mem::LAZY_PAGES.load(Ordering::Relaxed) as f64 * 4096.0 / MB,
        super::mem::FREES.load(Ordering::Relaxed),
        super::mem::FREED_BYTES.load(Ordering::Relaxed) as f64 / MB
    );
    for hi in 0..nh {
        if heap_priv[hi] < 1 << 20 && heap_busy[hi] < 1 << 20 {
            continue;
        }
        let _ = writeln!(
            s,
            "[mem]   heap #{hi}{}: comprometido {:.1} MB (regiões {:.1} + blocos grandes {:.1}; em uso {:.1})",
            if heaps[hi] == proc_heap { " (do processo: Rust + CRT)" } else { "" },
            heap_priv[hi] as f64 / MB,
            heap_regions[hi] as f64 / MB,
            heap_large[hi] as f64 / MB,
            heap_busy[hi] as f64 / MB
        );
    }
    let ml = ["<4K", "<64K", "<1M", "<16M", ">=16M"];
    let al: Vec<String> = (0..5)
        .map(|i| format!("{} {:.1} MB/{}", ml[i], LIVE[i].load(Ordering::Relaxed) as f64 / MB, COUNT[i].load(Ordering::Relaxed)))
        .collect();
    let _ = writeln!(
        s,
        "[mem]   alocador do Rust: vivo {:.1} MB (pico {:.1}) · {}",
        TOTAL.load(Ordering::Relaxed) as f64 / MB,
        PEAK.load(Ordering::Relaxed) as f64 / MB,
        al.join(" · ")
    );
    let mut big: Vec<usize> = (0..BIG_SLOTS).map(|i| BIG_SIZE[i].load(Ordering::Acquire)).filter(|&n| n > 0).collect();
    big.sort_unstable_by(|a, b| b.cmp(a));
    let mut bs: Vec<(usize, usize)> = Vec::new();
    for n in big {
        match bs.last_mut() {
            Some(x) if x.0 == n => x.1 += 1,
            _ => bs.push((n, 1)),
        }
    }
    let b: Vec<String> = bs.iter().take(24).map(|(n, c)| format!("{:.2} MB ×{c}", *n as f64 / MB)).collect();
    let _ = writeln!(s, "[mem]   blocos do Rust ≥ 1 MB: {}", b.join(" · "));
    let o: Vec<String> = others
        .iter()
        .take(16)
        .map(|o| {
            let t = if o.3 & PAGE_WRITECOMBINE != 0 {
                " WC"
            } else if o.3 & PAGE_NOCACHE != 0 {
                " NC"
            } else {
                ""
            };
            format!("{:.2}{t}", o.1 as f64 / MB)
        })
        .collect();
    let _ = writeln!(s, "[mem]   resto, maiores alocações (MB): {}", o.join(" · "));
    let small = others.iter().filter(|o| o.1 < 1 << 20).map(|o| o.1).sum::<u64>();
    let _ = writeln!(s, "[mem]   resto em alocações < 1 MB: {:.1} MB", small as f64 / MB);
    let o: Vec<String> = mapped_list
        .iter()
        .take(12)
        .map(|o| format!("{:.2}{}", o.1 as f64 / MB, if o.2 & PAGE_WRITECOMBINE != 0 { " WC" } else { "" }))
        .collect();
    let _ = write!(s, "[mem]   mapeado, maiores (MB): {}", o.join(" · "));
    s
}
