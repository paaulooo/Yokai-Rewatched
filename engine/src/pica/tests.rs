//! Validação dos backends contra o quadro real capturado no Azahar (`data/capture/cap`: o quarto do Nate).
//!
//! `cargo test --release --lib -- --ignored pica_frame --nocapture` (roda `pica_frame` e `pica_frame_vulkan`,
//! um de cada vez — os dois usam a memória guest global).
//! Carrega `linear.bin` (VA 0x14000000) e `vram_start.bin` (VA 0x1F000000), executa `list_01..list_07` e compara cada
//! render target desenhado com o mesmo intervalo de `vram_end.bin` (a VRAM depois do quadro no Azahar). Grava
//! `work/pica_test/<rt>_{ours,ref}.rgba` (imagem linear de cima para baixo, tamanho no nome) para inspeção
//! (`<rt>_vk_…` para o Vulkan).

use crate::hle::gpu::{read_pixel, tiled_offset};
use crate::hle::mem;
use crate::render::{self, Backend};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// Os testes de quadro compartilham memória guest e estado do PICA: um de cada vez.
static FRAME_LOCK: Mutex<()> = Mutex::new(());

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

type RtKey = (u32, u32, u32, u32);

struct Cap {
    dir: PathBuf,
    linear: Vec<u8>,
    vram0: Vec<u8>,
    vram1: Vec<u8>,
    lists: Vec<(u32, u32, u32)>,
}

fn load_cap() -> Cap {
    let dir = std::env::var_os("YW_CAP").map(PathBuf::from).unwrap_or_else(|| root().join("data/capture/cap"));
    let linear = std::fs::read(dir.join("linear.bin")).expect("linear.bin");
    let vram0 = std::fs::read(dir.join("vram_start.bin")).expect("vram_start.bin");
    let vram1 = std::fs::read(dir.join("vram_end.bin")).expect("vram_end.bin");
    let index = std::fs::read_to_string(dir.join("index.txt")).expect("index.txt");
    let mut lists = Vec::new();
    for line in index.lines() {
        // "list_01.bin addr=0x16d6e820 size=0x270"
        let mut it = line.split_whitespace();
        let (Some(name), Some(a), Some(s)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        let n: u32 = name.trim_start_matches("list_").trim_end_matches(".bin").parse().unwrap_or(99);
        let a = u32::from_str_radix(a.trim_start_matches("addr=0x"), 16).unwrap();
        let s = u32::from_str_radix(s.trim_start_matches("size=0x"), 16).unwrap();
        if (1..=7).contains(&n) {
            lists.push((n, a, s));
        }
    }
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        mem::init();
        mem::map(mem::LINEAR, linear.len() as u32, "linear (captura)");
        mem::map(mem::VRAM, mem::VRAM_SIZE, "vram (captura)");
    });
    Cap { dir, linear, vram0, vram1, lists }
}

/// Memória e estado do PICA do início da captura.
fn reset(cap: &Cap) {
    yw1_rt::write_bytes(mem::LINEAR, &cap.linear);
    yw1_rt::write_bytes(mem::VRAM, &cap.vram0);
    *super::STATE.lock().unwrap() = None;
    // estado anterior à janela capturada que as listas não reescrevem: DEPTHMAP_SCALE (0x4D) — valor usual −1,0
    // (profundidade = −z/w) [H: a captura não tem o estado do GPU antes da lista 1]
    super::set_reg(0x4d, 0xbf_0000);
}

struct Run {
    /// render targets usados: (endereço, largura, altura, formato) → nº de draws
    rts: BTreeMap<RtKey, u32>,
    /// cópia do RT de cada geometria logo depois das listas que desenham nela (antes de outro RT sobrescrever)
    snaps: BTreeMap<RtKey, Vec<u8>>,
    ms: f64,
}

/// Executa as listas 1–7 com o backend atual.
fn run_lists(cap: &Cap, verbose: bool) -> Run {
    let mut rts: BTreeMap<RtKey, u32> = BTreeMap::new();
    let mut snaps: BTreeMap<RtKey, Vec<u8>> = BTreeMap::new();
    let datas: Vec<Vec<u8>> = cap.lists.iter().map(|&(n, _, _)| std::fs::read(cap.dir.join(format!("list_{n:02}.bin"))).unwrap()).collect();
    let mut ms = 0.0;
    for (&(n, a, s), data) in cap.lists.iter().zip(&datas) {
        // a lista está no heap linear capturado; garante o conteúdo exato do arquivo da lista
        yw1_rt::write_bytes(a, data);
        // antes de cada passada 3D (listas 2 e 4) o jogo limpa a profundidade com um MemoryFill do GSP, que não está
        // nas listas capturadas: reproduz com o valor máximo (D24S8 = 0x00FFFFFF) [H: valor do fill não capturado]
        if n == 2 || n == 4 {
            let g = super::STATE.lock().unwrap();
            let r = &g.as_ref().unwrap().regs;
            let (dpa, dims) = (r[0x11c] << 3, r[0x11e]);
            drop(g);
            if let Some(dva) = mem::pa_to_va(dpa) {
                let (w, h) = (dims & 0x7ff, ((dims >> 12) & 0x3ff) + 1);
                for i in 0..w * h {
                    yw1_rt::w32(dva + 4 * i, 0x00ff_ffff);
                }
            }
        }
        let before = super::DRAWS.load(std::sync::atomic::Ordering::Relaxed);
        let t0 = std::time::Instant::now();
        super::execute(a, s);
        ms += t0.elapsed().as_secs_f64() * 1000.0;
        let after = super::DRAWS.load(std::sync::atomic::Ordering::Relaxed);
        let g = super::STATE.lock().unwrap();
        let r = &g.as_ref().unwrap().regs;
        let dims = r[0x11e];
        let key = (r[0x11d] << 3, dims & 0x7ff, ((dims >> 12) & 0x3ff) + 1, (r[0x117] >> 16) & 7);
        *rts.entry(key).or_default() += (after - before) as u32;
        if after > before {
            if let Some(va) = mem::pa_to_va(key.0) {
                let mut buf = vec![0u8; (key.1 * key.2 * bpp(key.3)) as usize];
                yw1_rt::read_bytes(va, &mut buf);
                snaps.insert(key, buf);
            }
        }
        if verbose {
            println!("list_{n:02}: {} draws, RT {:#x} {}x{} fmt {}", after - before, key.0, key.1, key.2, key.3);
        }
    }
    Run { rts, snaps, ms }
}

fn bpp(fmt: u32) -> u32 {
    match fmt {
        0 => 4,
        1 => 3,
        _ => 2,
    }
}

/// Imagem RGBA (de cima para baixo) de um RT: a nossa (do snapshot) e a da referência (`vram_end.bin`).
fn images(cap: &Cap, run: &Run, key: RtKey) -> Option<(Vec<u8>, Vec<u8>)> {
    let (pa, w, h, fmt) = key;
    let va = mem::pa_to_va(pa)?;
    let bpp = bpp(fmt);
    let mut ours = vec![0u8; (w * h * 4) as usize];
    let mut refi = vec![0u8; (w * h * 4) as usize];
    let snap = run.snaps.get(&key).cloned().unwrap_or_default();
    for y in 0..h {
        for x in 0..w {
            // memória: linhas de baixo para cima
            let off = tiled_offset(x, h - 1 - y, w, bpp);
            let a = if (off + bpp) as usize <= snap.len() {
                let mut word = [0u8; 4];
                word[..bpp as usize].copy_from_slice(&snap[off as usize..(off + bpp) as usize]);
                decode(fmt, word)
            } else {
                read_pixel(va + off, fmt)
            };
            let vo = (va - mem::VRAM + off) as usize;
            let b = if vo + 4 <= cap.vram1.len() {
                let mut word = [0u8; 4];
                word[..bpp as usize].copy_from_slice(&cap.vram1[vo..vo + bpp as usize]);
                decode(fmt, word)
            } else {
                [0, 0, 0, 0]
            };
            let i = ((y * w + x) * 4) as usize;
            ours[i..i + 4].copy_from_slice(&a);
            refi[i..i + 4].copy_from_slice(&b);
        }
    }
    Some((ours, refi))
}

fn close(a: &[u8], b: &[u8]) -> bool {
    (0..3).all(|k| (a[k] as i32 - b[k] as i32).abs() <= 8)
}

/// Compara com a referência só onde o Azahar já tinha descarregado o RT para a VRAM (referência não preta).
fn vs_ref(ours: &[u8], refi: &[u8]) -> (u64, u64) {
    let (mut eq, mut n) = (0, 0);
    for (a, b) in ours.chunks_exact(4).zip(refi.chunks_exact(4)) {
        if b[..3] != [0, 0, 0] {
            n += 1;
            eq += close(a, b) as u64;
        }
    }
    (eq, n)
}

#[test]
#[ignore]
fn pica_frame() {
    let _g = FRAME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let cap = load_cap();
    render::set_backend(Backend::Soft);
    reset(&cap);
    let run = run_lists(&cap, true);
    println!("tempo de renderização: {:.0} ms", run.ms);

    let out = root().join("work/pica_test");
    let _ = std::fs::create_dir_all(&out);
    let mut total_eq = 0u64;
    let mut total = 0u64;
    for (&key, &draws) in &run.rts {
        let (pa, w, h, fmt) = key;
        if draws == 0 || w == 0 || h == 0 {
            continue;
        }
        let Some((ours, refi)) = images(&cap, &run, key) else { continue };
        let (eq, n) = vs_ref(&ours, &refi);
        let n = n.max(1);
        total_eq += eq;
        total += n;
        println!(
            "RT {pa:#x} {w}x{h} fmt {fmt}: {draws} draws · pixels iguais (±8) {:.1}% de {n} comparáveis",
            100.0 * eq as f64 / n as f64
        );
        let _ = std::fs::write(out.join(format!("{pa:08x}_ours_{w}x{h}.rgba")), &ours);
        let _ = std::fs::write(out.join(format!("{pa:08x}_ref_{w}x{h}.rgba")), &refi);
    }
    println!("TOTAL: {:.1}% de {total} pixels", 100.0 * total_eq as f64 / total.max(1) as f64);
}

/// Mesmo quadro no backend Vulkan: compara com o soft (todos os pixels) e com o Azahar (só os comparáveis), e mede o
/// tempo das listas em cada backend (o Vulkan "frio" inclui criar pipelines e enviar texturas).
#[test]
#[ignore]
fn pica_frame_vulkan() {
    let _g = FRAME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let cap = load_cap();

    render::set_backend(Backend::Soft);
    reset(&cap);
    let soft = run_lists(&cap, false);

    assert_eq!(render::set_backend(Backend::Vulkan), Backend::Vulkan, "Vulkan não inicializou");
    reset(&cap);
    let vk_cold = run_lists(&cap, true);
    let mut warm = Vec::new();
    let mut vk = None;
    for _ in 0..5 {
        reset(&cap);
        let r = run_lists(&cap, false);
        warm.push(r.ms);
        vk = Some(r);
    }
    let vk = vk.unwrap();
    // custo do front-end sozinho (registradores; sem desenho não roda nem o shader de vértice)
    render::set_backend(Backend::None);
    reset(&cap);
    let none = run_lists(&cap, false);
    render::set_backend(Backend::Soft);

    let out = root().join("work/pica_test");
    let _ = std::fs::create_dir_all(&out);
    let (mut s_eq, mut s_n, mut s_exact, mut r_eq, mut r_n, mut rs_eq) = (0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
    for (&key, &draws) in &vk.rts {
        let (pa, w, h, fmt) = key;
        if draws == 0 || w == 0 || h == 0 {
            continue;
        }
        let (Some((ours, refi)), Some((soft_img, _))) = (images(&cap, &vk, key), images(&cap, &soft, key)) else {
            continue;
        };
        let n = (w * h) as u64;
        let eq = ours.chunks_exact(4).zip(soft_img.chunks_exact(4)).filter(|(a, b)| close(a, b)).count() as u64;
        let exact = ours.chunks_exact(4).zip(soft_img.chunks_exact(4)).filter(|(a, b)| a[..3] == b[..3]).count() as u64;
        // histograma da maior diferença RGB por pixel (0, 1, 2, 3–8, > 8)
        let mut hist = [0u64; 5];
        for (a, b) in ours.chunks_exact(4).zip(soft_img.chunks_exact(4)) {
            let d = (0..3).map(|k| (a[k] as i32 - b[k] as i32).abs()).max().unwrap();
            hist[match d {
                0 => 0,
                1 => 1,
                2 => 2,
                3..=8 => 3,
                _ => 4,
            }] += 1;
        }
        println!("  |Vulkan − soft| máx por pixel: 0:{} 1:{} 2:{} 3–8:{} >8:{}", hist[0], hist[1], hist[2], hist[3], hist[4]);
        let (req, rn) = vs_ref(&ours, &refi);
        let (rseq, _) = vs_ref(&soft_img, &refi);
        s_eq += eq;
        s_n += n;
        s_exact += exact;
        r_eq += req;
        r_n += rn;
        rs_eq += rseq;
        println!(
            "RT {pa:#x} {w}x{h} fmt {fmt}: {draws} draws · Vulkan = soft (±8) {:.2}% (exatos {:.2}%) de {n} · Vulkan = Azahar (±8) {:.1}% de {rn} comparáveis (soft {:.1}%)",
            100.0 * eq as f64 / n as f64,
            100.0 * exact as f64 / n as f64,
            100.0 * req as f64 / rn.max(1) as f64,
            100.0 * rseq as f64 / rn.max(1) as f64,
        );
        let _ = std::fs::write(out.join(format!("{pa:08x}_vk_{w}x{h}.rgba")), &ours);
    }
    let st = render::vulkan::stats();
    println!(
        "TOTAL Vulkan = soft (±8): {:.2}% de {s_n} (exatos {:.2}%) · Vulkan = Azahar: {:.1}% de {r_n} (soft {:.1}%)",
        100.0 * s_eq as f64 / s_n.max(1) as f64,
        100.0 * s_exact as f64 / s_n.max(1) as f64,
        100.0 * r_eq as f64 / r_n.max(1) as f64,
        100.0 * rs_eq as f64 / r_n.max(1) as f64,
    );
    let avg = warm.iter().sum::<f64>() / warm.len() as f64;
    let min = warm.iter().cloned().fold(f64::MAX, f64::min);
    println!(
        "tempo das listas: soft {:.0} ms · Vulkan frio {:.1} ms · Vulkan quente {:.1} ms (média de {}, mín {:.1}) · sem desenho {:.1} ms",
        soft.ms,
        vk_cold.ms,
        avg,
        warm.len(),
        min,
        none.ms
    );
    println!(
        "vulkan (todas as {} execuções): draws {} · lotes {} · uploads de RT {} · texturas {} · pipelines {} · GPU (submit+espera) {:.1} ms · conversão RT {:.1} ms · decodificação de texturas {:.1} ms",
        1 + warm.len(),
        st[0],
        st[1],
        st[2],
        st[3],
        st[4],
        st[5] as f64 / 1000.0,
        st[6] as f64 / 1000.0,
        st[7] as f64 / 1000.0
    );
}

fn decode(fmt: u32, b: [u8; 4]) -> [u8; 4] {
    let v = u32::from_le_bytes(b);
    match fmt {
        0 => [(v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8],
        1 => [b[2], b[1], b[0], 255],
        2 => {
            let c = |s: u32| {
                let x = (v >> s) & 31;
                (x << 3 | x >> 2) as u8
            };
            [c(11), c(6), c(1), if v & 1 != 0 { 255 } else { 0 }]
        }
        3 => {
            let (r, g, bl) = ((v >> 11) & 31, (v >> 5) & 63, v & 31);
            [(r << 3 | r >> 2) as u8, (g << 2 | g >> 4) as u8, (bl << 3 | bl >> 2) as u8, 255]
        }
        _ => {
            let c = |s: u32| (((v >> s) & 15) * 17) as u8;
            [c(12), c(8), c(4), c(0)]
        }
    }
}
