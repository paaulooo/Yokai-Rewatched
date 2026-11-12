//! Validação dos backends contra o quadro real capturado no Azahar (`data/capture/cap`: o quarto do Nate).
//!
//! `cargo test --release --lib -- --ignored pica_frame --nocapture` (roda `pica_frame` e `pica_frame_vulkan`,
//! um de cada vez — os dois usam a memória guest global).
//! Carrega `linear.bin` (VA 0x14000000) e `vram_start.bin` (VA 0x1F000000), executa `list_01..list_07` e compara cada
//! render target desenhado com o mesmo intervalo de `vram_end.bin` (a VRAM depois do quadro no Azahar). Grava
//! `work/pica_test/<rt>_{ours,ref}.rgba` (imagem linear de cima para baixo, tamanho no nome) para inspeção
//! (`<rt>_vk_…` para o Vulkan).
//!
//! Métricas contra o Azahar: "comparáveis" = pixels em que a referência não é preta. Essa métrica sozinha quase não
//! discrimina — a VRAM inicial já tem o quadro anterior, quase igual — então cada linha mostra também a LINHA DE BASE
//! "sem desenho" (VRAM inicial × referência) e a nota só nos comparáveis que o quadro mudou (vram_end ≠ vram_start).

use crate::hle::gpu::{pica_color_fmt_to_gsp, read_pixel, tiled_offset, write_pixel};
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
    init_mem();
    static LINEAR: std::sync::Once = std::sync::Once::new();
    LINEAR.call_once(|| mem::map(mem::LINEAR, linear.len() as u32, "linear (captura)"));
    Cap { dir, linear, vram0, vram1, lists }
}

/// Reserva o espaço guest e mapeia a VRAM (uma vez por processo; os testes daqui são os únicos que fazem isso).
fn init_mem() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        mem::init();
        mem::map(mem::VRAM, mem::VRAM_SIZE, "vram (captura)");
    });
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
    /// o mesmo para o buffer de profundidade de cada lista que desenha: (endereço, largura, altura, formato) → bytes
    dsnaps: BTreeMap<RtKey, Vec<u8>>,
    ms: f64,
}

/// Executa as listas 1–7 com o backend atual.
fn run_lists(cap: &Cap, verbose: bool) -> Run {
    run_lists_hook(cap, verbose, &mut |_, _, _| {})
}

/// Como `run_lists`, chamando `hook(nº da lista, RT da lista, draws da lista)` depois de cada lista (com a memória já
/// atualizada): o quadro capturado reaproveita o mesmo endereço de VRAM para alvos diferentes, então quem precisa do
/// conteúdo de um alvo (o DisplayTransfer) tem de agir logo depois da lista que desenhou nele.
fn run_lists_hook(cap: &Cap, verbose: bool, hook: &mut dyn FnMut(u32, RtKey, u32)) -> Run {
    let mut rts: BTreeMap<RtKey, u32> = BTreeMap::new();
    let mut snaps: BTreeMap<RtKey, Vec<u8>> = BTreeMap::new();
    let mut dsnaps: BTreeMap<RtKey, Vec<u8>> = BTreeMap::new();
    let datas: Vec<Vec<u8>> = cap.lists.iter().map(|&(n, _, _)| std::fs::read(cap.dir.join(format!("list_{n:02}.bin"))).unwrap()).collect();
    let mut ms = 0.0;
    for (&(n, a, s), data) in cap.lists.iter().zip(&datas) {
        prep_list(n, a, data);
        let before = super::DRAWS.load(std::sync::atomic::Ordering::Relaxed);
        let t0 = std::time::Instant::now();
        super::execute(a, s);
        // a devolução dos render targets é preguiçosa: aqui alguém (o teste) vai ler a memória
        crate::render::vulkan::sync_all();
        ms += t0.elapsed().as_secs_f64() * 1000.0;
        let after = super::DRAWS.load(std::sync::atomic::Ordering::Relaxed);
        let g = super::STATE.lock().unwrap();
        let r = &g.as_ref().unwrap().regs;
        let dims = r[0x11e];
        let key = (r[0x11d] << 3, dims & 0x7ff, ((dims >> 12) & 0x3ff) + 1, (r[0x117] >> 16) & 7);
        let dkey = (r[0x11c] << 3, key.1, key.2, r[0x116] & 3);
        *rts.entry(key).or_default() += (after - before) as u32;
        if after > before && dkey.0 != 0 {
            if let Some(va) = mem::pa_to_va(dkey.0) {
                let dbpp = match dkey.3 {
                    0 => 2,
                    1 | 2 => 3,
                    _ => 4,
                };
                let mut buf = vec![0u8; (dkey.1 * dkey.2 * dbpp) as usize];
                yw1_rt::read_bytes(va, &mut buf);
                dsnaps.insert(dkey, buf);
            }
        }
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
        drop(g);
        hook(n as u32, key, (after - before) as u32);
    }
    Run { rts, snaps, dsnaps, ms }
}

/// Antes de executar a lista `n` (em `a`, conteúdo `data`): garante o conteúdo da lista e reproduz o que o jogo faz
/// antes dela fora das listas capturadas.
fn prep_list(n: u32, a: u32, data: &[u8]) {
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
            // pelo GSP (MemoryFill de 32 bits), como o jogo: o backend de GPU vê a escrita (devolução preguiçosa)
            crate::hle::gpu::memory_fill(dva, 0x00ff_ffff, dva + 4 * w * h, 0x200);
        }
    }
}

fn bpp(fmt: u32) -> u32 {
    match fmt {
        0 => 4,
        1 => 3,
        _ => 2,
    }
}

struct Imgs {
    ours: Vec<u8>,
    refi: Vec<u8>,
    /// o RT antes do quadro (`vram_start.bin`)
    start: Vec<u8>,
}

/// Imagem RGBA (de cima para baixo) de um RT: a nossa (do snapshot), a da referência (`vram_end.bin`) e a inicial.
fn images(cap: &Cap, run: &Run, key: RtKey) -> Option<Imgs> {
    let (pa, w, h, fmt) = key;
    let va = mem::pa_to_va(pa)?;
    let bpp = bpp(fmt);
    let mut ours = vec![0u8; (w * h * 4) as usize];
    let mut refi = vec![0u8; (w * h * 4) as usize];
    let mut start = vec![0u8; (w * h * 4) as usize];
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
                read_pixel(va + off, pica_color_fmt_to_gsp(fmt))
            };
            let vo = (va - mem::VRAM + off) as usize;
            let from = |v: &[u8]| {
                if vo + 4 <= v.len() {
                    let mut word = [0u8; 4];
                    word[..bpp as usize].copy_from_slice(&v[vo..vo + bpp as usize]);
                    decode(fmt, word)
                } else {
                    [0, 0, 0, 0]
                }
            };
            let i = ((y * w + x) * 4) as usize;
            ours[i..i + 4].copy_from_slice(&a);
            refi[i..i + 4].copy_from_slice(&from(&cap.vram1));
            start[i..i + 4].copy_from_slice(&from(&cap.vram0));
        }
    }
    Some(Imgs { ours, refi, start })
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

/// Compara só nos pixels comparáveis (referência não preta) que o quadro do Azahar mudou (referência ≠ VRAM inicial,
/// RGB): aqui uma passada que não desenha nada tira ~0%. (Pixels que ficaram pretos na referência são os que o Azahar
/// ainda não tinha devolvido para a VRAM; não entram.)
fn vs_ref_changed(ours: &[u8], refi: &[u8], start: &[u8]) -> (u64, u64) {
    let (mut eq, mut n) = (0, 0);
    for ((a, b), s) in ours.chunks_exact(4).zip(refi.chunks_exact(4)).zip(start.chunks_exact(4)) {
        if b[..3] != [0, 0, 0] && b[..3] != s[..3] {
            n += 1;
            eq += close(a, b) as u64;
        }
    }
    (eq, n)
}

/// Pixels (RGB) em que a nossa imagem difere da VRAM inicial — 0 = a passada não desenhou nada.
fn touched(ours: &[u8], start: &[u8]) -> u64 {
    ours.chunks_exact(4).zip(start.chunks_exact(4)).filter(|(a, s)| a[..3] != s[..3]).count() as u64
}

fn pct(a: u64, n: u64) -> f64 {
    100.0 * a as f64 / n.max(1) as f64
}

#[test]
#[ignore]
fn pica_frame() {
    let _g = FRAME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let cap = load_cap();
    assert_eq!(render::set_backend(Backend::Soft), Backend::Soft);
    assert_eq!(render::backend(), Backend::Soft, "o backend escolhido tem de ser o que fica valendo");
    reset(&cap);
    let run = run_lists(&cap, true);
    println!("tempo de renderização: {:.0} ms", run.ms);

    let out = root().join("work/pica_test");
    let _ = std::fs::create_dir_all(&out);
    let (mut t_eq, mut t_n, mut t_base, mut c_eq, mut c_n, mut c_base, mut t_touch) = (0u64, 0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
    for (&key, &draws) in &run.rts {
        let (pa, w, h, fmt) = key;
        if draws == 0 || w == 0 || h == 0 {
            continue;
        }
        let Some(im) = images(&cap, &run, key) else { continue };
        let (eq, n) = vs_ref(&im.ours, &im.refi);
        let (base, _) = vs_ref(&im.start, &im.refi);
        let (ceq, cn) = vs_ref_changed(&im.ours, &im.refi, &im.start);
        let (cbase, _) = vs_ref_changed(&im.start, &im.refi, &im.start);
        let tch = touched(&im.ours, &im.start);
        t_eq += eq;
        t_n += n;
        t_base += base;
        c_eq += ceq;
        c_n += cn;
        c_base += cbase;
        t_touch += tch;
        println!(
            "RT {pa:#x} {w}x{h} fmt {fmt}: {draws} draws · {tch} pixels desenhados · iguais (±8) {:.1}% de {n} comparáveis (sem desenho {:.1}%) · {:.1}% de {cn} mudados no quadro (sem desenho {:.1}%)",
            pct(eq, n),
            pct(base, n),
            pct(ceq, cn),
            pct(cbase, cn)
        );
        let _ = std::fs::write(out.join(format!("{pa:08x}_ours_{w}x{h}.rgba")), &im.ours);
        let _ = std::fs::write(out.join(format!("{pa:08x}_ref_{w}x{h}.rgba")), &im.refi);
    }
    println!(
        "TOTAL: {:.1}% de {t_n} comparáveis (sem desenho {:.1}%) · {:.1}% de {c_n} mudados no quadro (sem desenho {:.1}%)",
        pct(t_eq, t_n),
        pct(t_base, t_n),
        pct(c_eq, c_n),
        pct(c_base, c_n)
    );
    assert!(t_touch > 0, "a passada soft não desenhou nada");
}

/// Registros dos shaders AOT gerados (`work/recomp/shaders/aot.txt`; com `dispatch`, os programas com fluxo trocados
/// pela variante no modo dispatch de `aot_dispatch.txt`), compilados aqui com o glslang para `work/pica_test/aot/`.
/// Vazio se o gerador não rodou ou não há glslang.
fn aot_list(dispatch: bool) -> Vec<(String, Vec<u8>)> {
    let dir = root().join("work/recomp/shaders");
    let read = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap_or_default();
    let recs = |t: &str| -> Vec<String> { t.lines().filter(|l| l.starts_with("name=")).map(String::from).collect() };
    let field = |r: &str, k: &str| r.split_whitespace().find_map(|f| f.strip_prefix(k)).map(String::from);
    let mut list = recs(&read("aot.txt"));
    if dispatch {
        let alt = recs(&read("aot_dispatch.txt"));
        for r in &mut list {
            if let Some(a) = alt.iter().find(|a| field(a, "name=") == field(r, "name=")) {
                *r = a.clone();
            }
        }
    }
    let tool = [std::env::var("YW_GLSLANG").unwrap_or_default(), "C:/msys64/ucrt64/bin/glslangValidator.exe".into(), "glslangValidator".into()]
        .into_iter()
        .filter(|t| !t.is_empty())
        .find(|t| std::process::Command::new(t).arg("--version").output().is_ok_and(|o| o.status.success()));
    let Some(tool) = tool else { return Vec::new() };
    let out = root().join("work/pica_test/aot");
    let _ = std::fs::create_dir_all(&out);
    let mut v = Vec::new();
    for r in list {
        let Some(file) = field(&r, "file=") else { continue };
        let dst = out.join(format!("{file}.spv"));
        let ok = std::process::Command::new(&tool)
            .arg("-V")
            .arg(dir.join(format!("{file}.vert")))
            .arg("-o")
            .arg(&dst)
            .output()
            .is_ok_and(|o| o.status.success());
        if let (true, Ok(spv)) = (ok, std::fs::read(&dst)) {
            v.push((r, spv));
        } else {
            println!("shader AOT {file} não compilou");
        }
    }
    v
}

fn register_aot_list(l: &[(String, Vec<u8>)]) -> usize {
    let v: Vec<(&str, &[u8])> = l.iter().map(|(r, s)| (r.as_str(), s.as_slice())).collect();
    render::register_aot(&v)
}

/// Mesmo quadro no backend Vulkan: compara com o soft (todos os pixels) e com o Azahar (só os comparáveis), e mede o
/// tempo das listas em cada backend (o Vulkan "frio" inclui criar pipelines e enviar texturas). Com os shaders AOT
/// gerados (`cd recomp && cargo run --release`), a passada principal usa os AOT e é comparada com o ubershader e com
/// a variante dispatch dos AOT.
#[test]
#[ignore]
fn pica_frame_vulkan() {
    let _g = FRAME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let cap = load_cap();

    // contadores do Vulkan (acumulados no processo): a passada soft não pode passar por ele, a Vulkan tem de passar
    let draws_vk = || render::vulkan::stats()[0];
    assert_eq!(render::set_backend(Backend::Soft), Backend::Soft);
    assert_eq!(render::backend(), Backend::Soft);
    let d0 = draws_vk();
    reset(&cap);
    let soft = run_lists(&cap, false);
    assert_eq!(draws_vk(), d0, "a passada soft foi para o Vulkan");

    // shaders AOT do jogo (se o gerador já rodou)
    let aot_main = aot_list(false);
    let aot_disp = aot_list(true);
    let n_aot = register_aot_list(&aot_main);
    render::aot::set_enabled(true);
    println!("shaders AOT registrados: {n_aot} (de work/recomp/shaders)");

    assert_eq!(render::set_backend(Backend::Vulkan), Backend::Vulkan, "Vulkan não inicializou");
    assert_eq!(render::backend(), Backend::Vulkan);
    let vs_counts = || {
        let s = render::vulkan::stats();
        (s[8], s[9])
    };
    let d1 = draws_vk();
    reset(&cap);
    let vk_cold = run_lists(&cap, true);
    assert!(draws_vk() > d1, "a passada Vulkan não desenhou nada no Vulkan");
    // pipelines AOT compilam em segundo plano (o 1º quadro usa o ubershader): espera para as passadas seguintes
    render::vulkan::wait_pipelines();
    let mut warm = Vec::new();
    let mut vk = None;
    for _ in 0..5 {
        reset(&cap);
        let r = run_lists(&cap, false);
        warm.push(r.ms);
        vk = Some(r);
    }
    let vk = vk.unwrap();
    let c0 = vs_counts();
    reset(&cap);
    let _ = run_lists(&cap, false);
    let c1 = vs_counts();
    let (q_aot, q_uber) = (c1.0 - c0.0, c1.1 - c0.1);
    println!("draws de vértice num quadro: AOT {q_aot} · ubershader {q_uber}");
    // o mesmo quadro só com o ubershader de vértice (= YW_VK_NO_AOT=1) e com a variante dispatch dos AOT
    let mut uber_warm = Vec::new();
    let mut vk_uber = None;
    let mut vk_disp = None;
    if n_aot > 0 {
        render::aot::set_enabled(false);
        reset(&cap);
        let _ = run_lists(&cap, false);
        for _ in 0..3 {
            reset(&cap);
            let r = run_lists(&cap, false);
            uber_warm.push(r.ms);
            vk_uber = Some(r);
        }
        render::aot::set_enabled(true);
        // `aot_list` pula (só imprime) o que o glslang recusa: a lista dispatch tem de ter os mesmos programas
        assert_eq!(aot_disp.len(), aot_main.len(), "alguma variante dispatch dos shaders AOT não compilou");
        if register_aot_list(&aot_disp) > 0 {
            reset(&cap);
            let _ = run_lists(&cap, false);
            render::vulkan::wait_pipelines();
            reset(&cap);
            let s0 = render::vulkan::stats();
            vk_disp = Some(run_lists(&cap, false));
            let s1 = render::vulkan::stats();
            // sem isto a comparação passaria por vacuidade: pipeline que falhou ou não ficou pronto cai em silêncio
            // no ubershader (= referência), e programa sem fluxo não tem variante dispatch
            let (da, du, dd) = (s1[8] - s0[8], s1[9] - s0[9], s1[12] - s0[12]);
            println!("passada dispatch: draws com AOT {da} (variante dispatch {dd}) · ubershader {du}");
            assert!(da > 0 && du == 0, "passada dispatch: {du} draws caíram no ubershader (AOT {da})");
            assert!(dd > 0, "passada dispatch: nenhum draw usou uma variante dispatch (a comparação não testaria nada)");
        }
        register_aot_list(&aot_main);
        reset(&cap);
        let _ = run_lists(&cap, false);
        render::vulkan::wait_pipelines();
    }
    // o mesmo quadro no Vulkan com o shader de vértice na CPU (= YW_VK_CPU_VS=1), para comparar os dois caminhos
    let gpu_vs = render::gpu_vs();
    let was_cpu = render::cpu_vs_forced();
    render::set_cpu_vs(true);
    let mut cpu_warm = Vec::new();
    let mut vk_cpu = None;
    for _ in 0..3 {
        reset(&cap);
        let r = run_lists(&cap, false);
        cpu_warm.push(r.ms);
        vk_cpu = Some(r);
    }
    let vk_cpu = vk_cpu.unwrap();
    render::set_cpu_vs(was_cpu);
    // custo do front-end sozinho (registradores; sem desenho não roda nem o shader de vértice)
    render::set_backend(Backend::None);
    reset(&cap);
    let none = run_lists(&cap, false);
    render::set_backend(Backend::Soft);

    let out = root().join("work/pica_test");
    let _ = std::fs::create_dir_all(&out);
    let (mut s_eq, mut s_n, mut s_exact, mut r_eq, mut r_n, mut rs_eq, mut base) = (0u64, 0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
    let (mut c_eq, mut c_n, mut cs_eq, mut c_base) = (0u64, 0u64, 0u64, 0u64);
    // vértices na GPU × vértices na CPU (ambos no Vulkan): iguais ±8, exatos; e o da CPU contra o Azahar
    let (mut g_eq, mut g_exact, mut rc_eq, mut cc_eq) = (0u64, 0u64, 0u64, 0u64);
    // AOT × ubershader, AOT dispatch × ubershader (±8, exatos) e o ubershader contra o Azahar
    let (mut u_eq, mut u_exact, mut ru_eq, mut cu_eq, mut x_eq, mut x_exact, mut x_n) = (0u64, 0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
    for (&key, &draws) in &vk.rts {
        let (pa, w, h, fmt) = key;
        if draws == 0 || w == 0 || h == 0 {
            continue;
        }
        let (Some(im), Some(sim)) = (images(&cap, &vk, key), images(&cap, &soft, key)) else {
            continue;
        };
        let (ours, refi, soft_img, start) = (&im.ours, &im.refi, &sim.ours, &im.start);
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
        let (req, rn) = vs_ref(ours, refi);
        let (rseq, _) = vs_ref(soft_img, refi);
        let (b0, _) = vs_ref(start, refi);
        let (ceq, cn) = vs_ref_changed(ours, refi, start);
        let (cseq, _) = vs_ref_changed(soft_img, refi, start);
        let (cb0, _) = vs_ref_changed(start, refi, start);
        base += b0;
        c_eq += ceq;
        c_n += cn;
        cs_eq += cseq;
        c_base += cb0;
        s_eq += eq;
        s_n += n;
        s_exact += exact;
        r_eq += req;
        r_n += rn;
        rs_eq += rseq;
        println!(
            "RT {pa:#x} {w}x{h} fmt {fmt}: {draws} draws · Vulkan = soft (±8) {:.2}% (exatos {:.2}%) de {n} · Vulkan = Azahar (±8) {:.1}% de {rn} comparáveis (soft {:.1}%, sem desenho {:.1}%) · {:.1}% de {cn} mudados no quadro (soft {:.1}%, sem desenho {:.1}%) · desenhados: Vulkan {} soft {}",
            pct(eq, n),
            pct(exact, n),
            pct(req, rn),
            pct(rseq, rn),
            pct(b0, rn),
            pct(ceq, cn),
            pct(cseq, cn),
            pct(cb0, cn),
            touched(ours, start),
            touched(soft_img, start),
        );
        let _ = std::fs::write(out.join(format!("{pa:08x}_vk_{w}x{h}.rgba")), ours);
        if let Some(cim) = images(&cap, &vk_cpu, key) {
            let cpu_img = &cim.ours;
            let geq = ours.chunks_exact(4).zip(cpu_img.chunks_exact(4)).filter(|(a, b)| close(a, b)).count() as u64;
            let gex = ours.chunks_exact(4).zip(cpu_img.chunks_exact(4)).filter(|(a, b)| a[..3] == b[..3]).count() as u64;
            let (rceq, _) = vs_ref(cpu_img, refi);
            let (cceq, _) = vs_ref_changed(cpu_img, refi, start);
            g_eq += geq;
            g_exact += gex;
            rc_eq += rceq;
            cc_eq += cceq;
            println!(
                "  vértices na GPU = na CPU (±8) {:.2}% (exatos {:.2}%) · CPU = Azahar {:.1}% · {:.1}% dos mudados",
                pct(geq, n),
                pct(gex, n),
                pct(rceq, rn),
                pct(cceq, cn)
            );
            let _ = std::fs::write(out.join(format!("{pa:08x}_vkcpu_{w}x{h}.rgba")), cpu_img);
        }
        let cmp = |a: &[u8], b: &[u8]| {
            let eq = a.chunks_exact(4).zip(b.chunks_exact(4)).filter(|(a, b)| close(a, b)).count() as u64;
            let ex = a.chunks_exact(4).zip(b.chunks_exact(4)).filter(|(a, b)| a[..3] == b[..3]).count() as u64;
            (eq, ex)
        };
        if let Some(uim) = vk_uber.as_ref().and_then(|r| images(&cap, r, key)) {
            let (eq, ex) = cmp(ours, &uim.ours);
            let (rueq, _) = vs_ref(&uim.ours, refi);
            let (cueq, _) = vs_ref_changed(&uim.ours, refi, start);
            u_eq += eq;
            u_exact += ex;
            ru_eq += rueq;
            cu_eq += cueq;
            println!("  AOT = ubershader (±8) {:.2}% (exatos {:.2}%) · ubershader = Azahar {:.1}%", pct(eq, n), pct(ex, n), pct(rueq, rn));
            if let Some(xim) = vk_disp.as_ref().and_then(|r| images(&cap, r, key)) {
                let (eq, ex) = cmp(&xim.ours, &uim.ours);
                x_eq += eq;
                x_exact += ex;
                x_n += n;
                println!("  AOT dispatch = ubershader (±8) {:.2}% (exatos {:.2}%)", pct(eq, n), pct(ex, n));
            }
        }
    }
    // cache de pipelines próprio (nos testes em work/pica_test/vk_pipelines.bin): a próxima execução o carrega
    render::vulkan::save_pipeline_cache();
    let st = render::vulkan::stats();
    println!(
        "TOTAL Vulkan = soft (±8): {:.2}% de {s_n} (exatos {:.2}%) · Vulkan = Azahar: {:.1}% de {r_n} comparáveis (soft {:.1}%, sem desenho {:.1}%) · {:.1}% de {c_n} mudados no quadro (soft {:.1}%, sem desenho {:.1}%)",
        pct(s_eq, s_n),
        pct(s_exact, s_n),
        pct(r_eq, r_n),
        pct(rs_eq, r_n),
        pct(base, r_n),
        pct(c_eq, c_n),
        pct(cs_eq, c_n),
        pct(c_base, c_n),
    );
    println!(
        "TOTAL vértices na {} = vértices na CPU (±8): {:.2}% (exatos {:.2}%) · Vulkan com vértices na CPU = Azahar: {:.1}% de {r_n} · {:.1}% de {c_n} mudados",
        if gpu_vs { "GPU" } else { "CPU (YW_VK_CPU_VS)" },
        pct(g_eq, s_n),
        pct(g_exact, s_n),
        pct(rc_eq, r_n),
        pct(cc_eq, c_n)
    );
    if !uber_warm.is_empty() {
        let uber_avg = uber_warm.iter().sum::<f64>() / uber_warm.len() as f64;
        println!(
            "TOTAL AOT = ubershader (±8): {:.2}% de {s_n} (exatos {:.2}%) · ubershader = Azahar {:.1}% de {r_n} · {:.1}% de {c_n} mudados · tempo das listas só com o ubershader: {uber_avg:.1} ms (média de {})",
            pct(u_eq, s_n),
            pct(u_exact, s_n),
            pct(ru_eq, r_n),
            pct(cu_eq, c_n),
            uber_warm.len()
        );
        if x_n > 0 {
            println!("TOTAL AOT no modo dispatch = ubershader (±8): {:.2}% de {x_n} (exatos {:.2}%)", pct(x_eq, x_n), pct(x_exact, x_n));
        }
        assert!(q_aot > 0, "nenhum draw usou shader AOT (o programa do quarto é o SHD001 do fix.xr)");
        assert_eq!(q_uber, 0, "draws da passada AOT caíram no ubershader");
    }
    let cpu_avg = cpu_warm.iter().sum::<f64>() / cpu_warm.len() as f64;
    println!("tempo das listas no Vulkan com vértices na CPU: {cpu_avg:.1} ms (média de {})", cpu_warm.len());
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

/// Resolução interna (`render::vulkan::hires`): o quadro capturado com a sombra N× ligada e um DisplayTransfer do alvo da
/// tela de cima (480×400, redução 2×1, RGB8, com e sem inversão vertical) para um framebuffer do LCD.
/// - N = 1: a imagem final da sombra tem de bater com o framebuffer nativo escrito pela própria transferência
///   (pixel a pixel, ±1): valida a orientação (girar/inverter/reduzir) e que a sombra recebe os mesmos draws;
/// - N = 3: a média de cada bloco 3×3 tem de ficar próxima do nativo (a sombra é o mesmo quadro com mais amostras).
#[test]
#[ignore]
fn pica_frame_hires() {
    let _g = FRAME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let cap = load_cap();
    assert_eq!(render::set_backend(Backend::Vulkan), Backend::Vulkan);
    render::set_hires_force(true);
    let out = mem::LINEAR + cap.linear.len() as u32 - 0x60000;
    let native = |out: u32| -> Vec<[u8; 4]> {
        // framebuffer do LCD: coluna X do LCD = linha X da memória (720 bytes), Y do LCD = 239 − posição na linha
        let mut v = Vec::with_capacity(400 * 240);
        for y in 0..240u32 {
            for x in 0..400u32 {
                v.push(crate::hle::gpu::read_pixel(out + x * 720 + (239 - y) * 3, 1));
            }
        }
        v
    };
    // última lista que desenha no alvo de cima (480×400): o DisplayTransfer do jogo vem logo depois dela
    let mut last_top = 0u32;
    {
        reset(&cap);
        run_lists_hook(&cap, false, &mut |n, k, d| {
            if k.1 == 480 && k.2 == 400 && d > 0 {
                last_top = n;
            }
        });
    }
    assert!(last_top > 0, "nenhuma lista desenha no alvo 480×400");
    for (nome, flags) in [("com inversão (0x1001001)", 0x100_1001u32), ("sem inversão (0x1001000)", 0x100_1000u32)] {
        for n in [1u32, 3] {
            render::set_scale(n);
            reset(&cap);
            // limpa o framebuffer antes: o que sobrar seria pixel não escrito
            yw1_rt::write_bytes(out, &vec![0xEEu8; 400 * 240 * 3]);
            let mut got = None;
            let _ = run_lists_hook(&cap, false, &mut |ln, key, _| {
                if ln == last_top {
                    let va = mem::pa_to_va(key.0).unwrap();
                    crate::hle::gpu::display_transfer(va, out, 480 | 400 << 16, 480 | 400 << 16, flags);
                    got = render::vulkan::published(out);
                }
            });
            let hi = got.unwrap_or_else(|| panic!("{nome} N={n}: sem imagem em alta resolução"));
            assert_eq!((hi.w, hi.h), (400 * n, 240 * n), "{nome} N={n}: tamanho");
            let nat = native(out);
            let (mut exact, mut near, mut sum) = (0u64, 0u64, 0u64);
            for y in 0..240usize {
                for x in 0..400usize {
                    let want = nat[y * 400 + x];
                    // média do bloco n×n da imagem em alta resolução
                    let mut acc = [0u32; 3];
                    for sy in 0..n as usize {
                        for sx in 0..n as usize {
                            let i = ((y * n as usize + sy) * hi.w as usize + x * n as usize + sx) * 4;
                            for c in 0..3 {
                                acc[c] += hi.rgba[i + c] as u32;
                            }
                        }
                    }
                    let got = acc.map(|a| (a / (n * n)) as i32);
                    let d = (0..3).map(|c| (got[c] - want[c] as i32).abs()).max().unwrap();
                    exact += (d == 0) as u64;
                    near += (d <= if n == 1 { 1 } else { 24 }) as u64;
                    sum += d as u64;
                }
            }
            let tot = 400 * 240u64;
            println!(
                "{nome} N={n}: iguais {:.2}% · dentro da tolerância {:.2}% · diferença média {:.3} (de {tot} pixels)",
                pct(exact, tot),
                pct(near, tot),
                sum as f64 / tot as f64
            );
            if n == 1 {
                assert!(near * 1000 >= tot * 999, "{nome} N=1: a imagem final não bate com o DisplayTransfer nativo");
            } else {
                assert!(near * 100 >= tot * 90, "{nome} N={n}: mais de 10% dos blocos longe do nativo");
            }
            let _ = std::fs::write(
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../work/pica_test/hires_n{n}_{}.rgba", if flags & 1 != 0 { "flip" } else { "noflip" })),
                &hi.rgba,
            );
        }
    }
    // FXAA de cena (N = 3): roda, muda só uma parte pequena dos pixels (bordas da cena 3D) e deixa o resto igual
    let shot = |fxaa: bool| -> std::sync::Arc<render::vulkan::HiRes> {
        render::set_fxaa(fxaa);
        render::set_scale(3);
        reset(&cap);
        yw1_rt::write_bytes(out, &vec![0xEEu8; 400 * 240 * 3]);
        let mut got = None;
        let _ = run_lists_hook(&cap, false, &mut |ln, key, _| {
            if ln == last_top {
                let va = mem::pa_to_va(key.0).unwrap();
                crate::hle::gpu::display_transfer(va, out, 480 | 400 << 16, 480 | 400 << 16, 0x100_1001);
                got = render::vulkan::published(out);
            }
        });
        got.expect("sem imagem em alta resolução")
    };
    let plain = shot(false);
    let aa = shot(true);
    let n = plain.rgba.len() / 4;
    let changed = plain.rgba.chunks_exact(4).zip(aa.rgba.chunks_exact(4)).filter(|(a, b)| a[..3] != b[..3]).count();
    let big = plain
        .rgba
        .chunks_exact(4)
        .zip(aa.rgba.chunks_exact(4))
        .filter(|(a, b)| (0..3).any(|c| (a[c] as i32 - b[c] as i32).abs() > 24))
        .count();
    println!(
        "FXAA de cena (N=3): {:.2}% dos pixels mudaram ({:.2}% por mais de 24) de {n}",
        pct(changed as u64, n as u64),
        pct(big as u64, n as u64)
    );
    let _ = std::fs::write(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../work/pica_test/hires_n3_fxaa.rgba"), &aa.rgba);
    assert!(changed > 0, "o FXAA não mudou nenhum pixel");
    assert!(changed * 2 < n, "o FXAA mudou mais da metade da imagem");
    render::set_fxaa(false);
    render::set_scale(1);
    render::set_hires_force(false);
    render::set_backend(Backend::Soft);
}

/// Filtro anisotrópico do Vulkan (`pica.frag`, opção `YW_ANISO`/menu; o PICA real não tem). Mesmo quadro com o filtro
/// desligado (duas vezes: tem de ser determinístico) e com 16 amostras: o desligado é o caminho de uma amostra de
/// sempre; o ligado só pode mudar pixels de superfícies em que a pegada do pixel passa de 1 texel — o resto tem de
/// ficar igual. Mostra quanto muda, por render target.
#[test]
#[ignore]
fn pica_frame_aniso() {
    let _g = FRAME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let cap = load_cap();
    assert_eq!(render::set_backend(Backend::Vulkan), Backend::Vulkan);
    let go = |an: u32| -> Run {
        render::set_aniso(an);
        reset(&cap);
        run_lists(&cap, false)
    };
    let _ = go(0);
    render::vulkan::wait_pipelines();
    let off1 = go(0);
    let off2 = go(0);
    let a16 = go(16);
    let a4 = go(4);
    render::set_aniso(0);
    render::set_backend(Backend::Soft);
    let words = |a: &[u8], b: &[u8]| {
        let n = a.len().min(b.len()) / 4;
        let ne = a.chunks_exact(4).zip(b.chunks_exact(4)).filter(|(x, y)| x != y).count();
        let big = a
            .chunks_exact(4)
            .zip(b.chunks_exact(4))
            .filter(|(x, y)| x.iter().zip(y.iter()).any(|(p, q)| (*p as i32 - *q as i32).abs() > 16))
            .count();
        (n, ne, big)
    };
    let mut total_changed = 0usize;
    for (key, base) in &off1.snaps {
        let again = off2.snaps.get(key).expect("RT some entre execuções");
        let (_, ne, _) = words(base, again);
        assert_eq!(ne, 0, "RT {key:?}: o filtro desligado não é determinístico ({ne} palavras diferentes)");
        for (nome, run) in [("16 amostras", &a16), ("4 amostras", &a4)] {
            let Some(o) = run.snaps.get(key) else { continue };
            let (n, ne, big) = words(base, o);
            println!(
                "RT {:#x} {}x{}: aniso {nome}: {:.2}% das palavras mudaram ({:.2}% por mais de 16) de {n}",
                key.0, key.1, key.2, pct(ne as u64, n as u64), pct(big as u64, n as u64)
            );
            if nome == "16 amostras" {
                total_changed += ne;
            }
        }
    }
    println!("anisotrópico 16×: {total_changed} palavras mudaram no total (0 = o quadro não tem superfície inclinada)");
    // imagens decodificadas (RGBA) para olhar a diferença: work/pica_test/aniso_{desligado,16}_WxH.rgba
    let out = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../work/pica_test");
    let _ = std::fs::create_dir_all(&out);
    for key in off1.snaps.keys() {
        for (nome, run) in [("desligado", &off1), ("16", &a16)] {
            if let Some(im) = images(&cap, run, *key) {
                let _ = std::fs::write(out.join(format!("aniso_{nome}_{}x{}.rgba", key.1, key.2)), im.ours);
            }
        }
    }
}

/// Banco de ensaio de GPU: o quadro capturado (quarto) com os shaders AOT, repetido `YW_BENCH_N` vezes (padrão 40) na
/// escala `YW_BENCH_SCALE` (padrão 3) com aniso `YW_BENCH_ANISO` (padrão 16). Imprime o tempo de GPU por quadro (nativo e
/// sombra N×, timestamps de `Vk::flush`), mediana e mínimo. Para comparar variantes do renderizador sem rodar a rota.
/// `cargo test --release --lib -- --ignored pica_frame_bench --nocapture`
#[test]
#[ignore]
fn pica_frame_bench() {
    let _g = FRAME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let cap = load_cap();
    let env = |k: &str, d: u32| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
    let (n, scale, an) = (env("YW_BENCH_N", 40), env("YW_BENCH_SCALE", 3), env("YW_BENCH_ANISO", 16));
    let n_aot = register_aot_list(&aot_list(false));
    render::aot::set_enabled(true);
    assert_eq!(render::set_backend(Backend::Vulkan), Backend::Vulkan);
    // Exatidão do fragmento especializado (YW_VK_FSPEC) contra o ubershader, bit a bit, nos alvos nativos:
    //  (1) 1×, sem sombra, aniso `an` (o filtro anisotrópico no especializado = no ubershader);
    //  (2) N× com sombra, aniso 0;
    //  (3) N× com sombra, aniso `an`: a passada nativa fica sem o filtro → igual a (2) no ubershader.
    let spec = render::vulkan::fs_spec();
    let pass = |fs: bool, sc: u32, a: u32, hires: bool| -> (Run, u64, u64) {
        render::vulkan::set_fs_spec(fs);
        render::set_scale(sc);
        render::set_aniso(a);
        render::set_hires_force(hires);
        for _ in 0..2 {
            reset(&cap);
            let _ = run_lists(&cap, false);
            render::vulkan::wait_pipelines();
        }
        let s0 = render::vulkan::stats();
        reset(&cap);
        let r = run_lists(&cap, false);
        let s1 = render::vulkan::stats();
        (r, s1[15] - s0[15], s1[16] - s0[16])
    };
    let bytes_diff = |x: &Run, y: &Run| -> usize {
        x.snaps.iter().map(|(k, a)| {
            let b = y.snaps.get(k).expect("RT some entre passadas");
            a.iter().zip(b.iter()).filter(|(p, q)| p != q).count()
        }).sum()
    };
    if spec {
        for (nome, sc, a, hires, ref_a) in [("1× aniso", 1, an, false, an), ("N× aniso 0", scale, 0, true, 0), ("N× aniso (nativa sem)", scale, an, true, 0)] {
            let (u, _, _) = pass(false, sc, ref_a, hires);
            let (r, ok, late) = pass(true, sc, a, hires);
            let diff = bytes_diff(&u, &r);
            println!("fragmento especializado, {nome}: {ok} draws (ainda compilando: {late}) · bytes diferentes do ubershader: {diff}");
            assert!(ok > 0 && late == 0, "{nome}: o fragmento especializado não foi usado ({ok} draws, {late} compilando)");
            assert_eq!(diff, 0, "{nome}: fragmento especializado diferente do ubershader");
        }
    }
    render::vulkan::set_fs_spec(spec);
    render::set_scale(scale);
    render::set_aniso(an);
    render::set_hires_force(true);
    for _ in 0..3 {
        reset(&cap);
        let _ = run_lists(&cap, false);
        render::vulkan::wait_pipelines();
    }
    let (mut nat, mut sh, mut tot) = (Vec::new(), Vec::new(), Vec::new());
    for _ in 0..n {
        let s0 = render::vulkan::stats();
        reset(&cap);
        let _ = run_lists(&cap, false);
        render::vulkan::sync_all();
        let s1 = render::vulkan::stats();
        let (a, b) = ((s1[13] - s0[13]) as f64 / 1000.0, (s1[14] - s0[14]) as f64 / 1000.0);
        nat.push(a);
        sh.push(b);
        tot.push(a + b);
    }
    render::set_hires_force(false);
    render::set_scale(1);
    render::set_aniso(0);
    render::set_backend(Backend::Soft);
    let med = |v: &mut Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        (v[v.len() / 2], v[0])
    };
    let (nm, n0) = med(&mut nat);
    let (sm, s0) = med(&mut sh);
    let (tm, t0) = med(&mut tot);
    println!(
        "BENCH escala {scale} aniso {an} ({n_aot} AOT, {n} quadros): GPU por quadro: nativo {nm:.3} ms (mín {n0:.3}) · sombra {sm:.3} ms (mín {s0:.3}) · total {tm:.3} ms (mín {t0:.3})"
    );
}

/// Banco da interpolação de quadros (F2/F4 do plano): o quadro capturado (quarto) na escala `YW_BENCH_SCALE` (padrão 3),
/// aniso `YW_BENCH_ANISO` (padrão 16), interpolação ligada (120 Hz), com os shaders AOT.
/// - EXATIDÃO, pelo caminho real (DisplayTransfer → `present_finish` → `interp_frame_done` → grupo publicado): o mesmo
///   quadro repetido, então o anterior = o atual e todo intermediário (α = i/(m+1)) usa os uniforms do quadro novo
///   (`lerp_shading` de dois iguais devolve o novo; é o caso que reproduz o quadro real, equivalente a α = 1). Cada
///   intermediário tem de sair IGUAL, byte a byte, à imagem real da sombra N× do mesmo grupo. A captura não tem o
///   MemoryFill do começo do quadro na tela de cima (sem ele o quadro não é interpolável: `usable` exige o clear
///   inicial), então o teste limpa cor e profundidade do alvo de cima antes da 1ª lista dele [H: valores do fill], e
///   descarta antes os desenhos da tela de baixo capturados no mesmo endereço.
/// - TEMPO, por intermediário (`YW_BENCH_N` repetições, padrão 40): re-renderização forçada
///   (`render::vulkan::test_interp_replay`, α = 0,5 do quadro contra si mesmo — o mesmo trabalho de um intermediário)
///   com GPU pelos timestamps de `Vk::flush` (stats 13 + 14) e CPU de parede da chamada (inclui a espera da GPU e a
///   leitura da imagem, como o `cost_ms` de `interp_make`). Mostra também a sombra N× do quadro real, para comparar.
/// `cargo test --release --lib -- --ignored pica_frame_bench_interp --nocapture`
#[test]
#[ignore]
fn pica_frame_bench_interp() {
    let _g = FRAME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let cap = load_cap();
    let env = |k: &str, d: u32| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
    let (n, scale, an) = (env("YW_BENCH_N", 40), env("YW_BENCH_SCALE", 3), env("YW_BENCH_ANISO", 16));
    let n_aot = register_aot_list(&aot_list(false));
    render::aot::set_enabled(true);
    assert_eq!(render::set_backend(Backend::Vulkan), Backend::Vulkan);
    render::set_scale(scale);
    render::set_aniso(an);
    render::set_hires_force(true);
    render::set_interp(120);
    let out = mem::LINEAR + cap.linear.len() as u32 - 0x60000;
    let va = |pa: u32| mem::pa_to_va(pa).unwrap_or_else(|| panic!("endereço {pa:#x} fora da memória mapeada"));

    // sonda: 1ª e última lista do alvo de cima (480×400), o endereço dele e o da profundidade que ele usa
    let (mut first, mut last) = (None::<(u32, u32, u32, u32, u32)>, None::<(u32, u32)>);
    reset(&cap);
    render::vulkan::test_interp_discard();
    run_lists_hook(&cap, false, &mut |ln, k, d| {
        if k.1 == 480 && k.2 == 400 && d > 0 {
            last = Some((ln, va(k.0)));
            if first.is_none() {
                let g = super::STATE.lock().unwrap();
                let r = &g.as_ref().unwrap().regs;
                let (dpa, dfmt) = (r[0x11c] << 3, r[0x116] & 3);
                drop(g);
                first = Some((ln, va(k.0), va(dpa), k.3, dfmt));
            }
        }
    });
    render::vulkan::test_interp_discard();
    let (first_top, cva, dva, cfmt, dfmt) = first.expect("nenhuma lista desenha no alvo 480×400");
    let (last_top, top) = last.unwrap();
    assert_eq!(top, cva, "o alvo de cima muda de endereço no meio do quadro");
    println!("alvo de cima: listas {first_top}–{last_top}, cor {cva:#x} (formato {cfmt}), profundidade {dva:#x} (formato {dfmt})");
    let datas: Vec<Vec<u8>> = cap.lists.iter().map(|&(ln, _, _)| std::fs::read(cap.dir.join(format!("list_{ln:02}.bin"))).unwrap()).collect();
    // MemoryFill com o tamanho de pixel do formato (cor: RGBA8/RGB8/16 bits; profundidade: D16/D24/D24S8)
    let fill = |a: u32, px: u32, value: u32| {
        let ctl = match px {
            4 => 0x200,
            3 => 0x100,
            _ => 0,
        };
        crate::hle::gpu::memory_fill(a, value, a + px * 480 * 400, ctl);
    };
    let cpx = bpp(cfmt);
    let dpx = match dfmt {
        0 => 2,
        1 | 2 => 3,
        _ => 4,
    };
    // um quadro: clear da tela de cima antes da 1ª lista dela, listas, e (opcional) o DisplayTransfer logo depois da
    // última lista dela (o endereço é reaproveitado por outros alvos depois)
    let quadro = |transfer: bool| {
        reset(&cap);
        for (&(ln, a, s), data) in cap.lists.iter().zip(&datas) {
            if ln == first_top {
                // a tela de baixo (240×320) usa o MESMO endereço de cor/profundidade antes: no jogo a transferência
                // dela vem antes do clear da de cima; aqui os desenhos dela saem da captura (senão o clear da de cima
                // contaria como "clear no meio do quadro" e o quadro não seria interpolável)
                render::vulkan::test_interp_discard();
                fill(cva, cpx, 0);
                fill(dva, dpx, if dpx == 2 { 0xffff } else { 0x00ff_ffff });
            }
            if (first_top..=last_top).contains(&ln) {
                // dentro do quadro da tela de cima: só o conteúdo da lista. O fill de profundidade que `prep_list`
                // supõe antes da 2ª passada 3D (lista 4) cairia no alvo de cima depois de desenhos nele = "clear no
                // meio do quadro", que a interpolação não re-renderiza [H: esse fill não está na captura]
                yw1_rt::write_bytes(a, data);
            } else {
                prep_list(ln, a, data);
            }
            super::execute(a, s);
            if transfer && ln == last_top {
                crate::hle::gpu::display_transfer(cva, out, 480 | 400 << 16, 480 | 400 << 16, 0x100_1001);
            }
        }
        render::vulkan::sync_all();
    };

    // --- exatidão pelo caminho real ---
    // `out` passa a ser o framebuffer que o LCD de cima mostra como olho esquerdo (só esse é interpolado): o valor do
    // registrador vem da memória guest — usa a 1ª palavra do próprio `out` (a transferência a sobrescreve depois)
    yw1_rt::write_bytes(out, &out.to_le_bytes());
    crate::hle::gpu::hw_regs_written(0x0040_0468, 4, out);
    assert!(crate::hle::gpu::top_shown(out), "o framebuffer de teste não ficou registrado como olho esquerdo");
    // aquecimento SEM DisplayTransfer: o histórico de ritmo da interpolação (VBlanks por quadro, teto por atraso) vive
    // o processo inteiro; quadros fora do ritmo aqui o envenenariam (um "quadro atrasado" corta os intermediários por
    // dezenas de quadros)
    for _ in 0..3 {
        quadro(false);
        render::vulkan::wait_pipelines();
    }
    let (mut grupos, mut mids, mut dif_bytes, mut dif_px) = (0u32, 0u32, 0usize, 0usize);
    // um grupo deixado por outro teste no mesmo framebuffer não conta
    let mut ultimo: Option<std::sync::Arc<render::vulkan::Group>> = render::vulkan::published_group(out);
    let mut por_quadro = Vec::new();
    // ritmo fixo (`YW_BENCH_PERIOD_MS`, padrão 100 = 6 VBlanks): `interp_make` decide quantos intermediários pelo
    // período medido entre quadros e corta quando um quadro atrasa; sem ritmo, o quadro do teste (~20 ms) cairia em
    // 1 VBlank e quase nunca sobraria orçamento. Com 100 ms cabem os 5 intermediários (o teto) sem atrasar o quadro
    let periodo = std::time::Duration::from_millis(env("YW_BENCH_PERIOD_MS", 100) as u64);
    let mut prox = std::time::Instant::now();
    // independente da ordem dos testes: o histórico de ritmo que outro teste (ou um quadro lento, GPU dividida) tenha
    // deixado se refaz com mais quadros no ritmo — continua além de `YW_BENCH_FRAMES` até sair algum intermediário
    // (no máximo 8×)
    let nq = env("YW_BENCH_FRAMES", 40);
    let mut feitos = 0u32;
    while feitos < nq || (mids == 0 && feitos < nq * 8) {
        feitos += 1;
        let t0 = std::time::Instant::now();
        quadro(true);
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        prox += periodo;
        let agora = std::time::Instant::now();
        if prox > agora {
            std::thread::sleep(prox - agora);
        } else {
            prox = agora;
        }
        let Some(g) = render::vulkan::published_group(out) else { continue };
        if ultimo.as_ref().is_some_and(|u| std::sync::Arc::ptr_eq(u, &g)) {
            continue; // grupo de um quadro anterior (este não interpolou)
        }
        grupos += 1;
        por_quadro.push((g.mids.len(), ms));
        for (i, m) in g.mids.iter().enumerate() {
            mids += 1;
            assert_eq!((m.w, m.h), (g.real.w, g.real.h), "intermediário {i}: tamanho diferente do real");
            let db = m.rgba.iter().zip(g.real.rgba.iter()).filter(|(a, b)| a != b).count();
            let dp = m.rgba.chunks_exact(4).zip(g.real.rgba.chunks_exact(4)).filter(|(a, b)| a != b).count();
            if db > 0 && dif_bytes == 0 {
                let d = root().join("work/pica_test");
                let _ = std::fs::create_dir_all(&d);
                let _ = std::fs::write(d.join(format!("interp_mid_{}x{}.rgba", m.w, m.h)), &m.rgba);
                let _ = std::fs::write(d.join(format!("interp_real_{}x{}.rgba", m.w, m.h)), &g.real.rgba);
            }
            dif_bytes += db;
            dif_px += dp;
        }
        ultimo = Some(g);
    }
    println!(
        "interpolação, caminho real ({} quadros): {grupos} grupos, {mids} intermediários (por grupo, com ms do quadro: {:?}) · diferença intermediário × sombra real: {dif_bytes} bytes ({dif_px} pixels)",
        feitos,
        por_quadro.iter().map(|(m, ms)| format!("{m}@{ms:.0}")).collect::<Vec<_>>()
    );

    // --- tempo por intermediário (replay forçado) ---
    let req = render::vulkan::PresentReq { inp: cva, iw: 480, ih: 400, sx: 2, sy: 1, flip: true, ow: 240, hh: 400, out: 0, out_bytes: 240 * 400 * 3 };
    for _ in 0..3 {
        render::vulkan::test_interp_discard();
        quadro(false);
        render::vulkan::test_interp_replay(req).unwrap_or_else(|e| panic!("replay: {e}"));
        render::vulkan::wait_pipelines();
    }
    let (mut gpu, mut g13, mut g14, mut cpu, mut sombra) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for _ in 0..n {
        render::vulkan::test_interp_discard();
        let s0 = render::vulkan::stats();
        quadro(false);
        let s1 = render::vulkan::stats();
        let t0 = std::time::Instant::now();
        render::vulkan::test_interp_replay(req).unwrap_or_else(|e| panic!("replay: {e}"));
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        let s2 = render::vulkan::stats();
        let (a, b) = ((s2[13] - s1[13]) as f64 / 1000.0, (s2[14] - s1[14]) as f64 / 1000.0);
        g13.push(a);
        g14.push(b);
        gpu.push(a + b);
        cpu.push(ms);
        sombra.push((s1[14] - s0[14]) as f64 / 1000.0);
    }
    render::set_interp(0);
    render::set_hires_force(false);
    render::set_scale(1);
    render::set_aniso(0);
    render::set_backend(Backend::Soft);
    let med = |v: &mut Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        (v[v.len() / 2], v[0])
    };
    let ((gm, g0), (am, _), (bm, _), (cm, c0), (sm, s0)) = (med(&mut gpu), med(&mut g13), med(&mut g14), med(&mut cpu), med(&mut sombra));
    println!(
        "BENCH_INTERP escala {scale} aniso {an} ({n_aot} AOT, {n} intermediários): GPU por intermediário {gm:.3} ms (mín {g0:.3}; lote principal {am:.3} + sombra {bm:.3}) · CPU de parede por intermediário {cm:.3} ms (mín {c0:.3}) · sombra N× do quadro real {sm:.3} ms (mín {s0:.3}) · exatidão: {dif_bytes} bytes diferentes em {mids} intermediários"
    );
    assert!(mids > 0, "o caminho real não fez nenhum intermediário: a exatidão não foi conferida");
    assert_eq!(dif_bytes, 0, "intermediário do quadro repetido diferente da sombra real ({dif_px} pixels)");
}

/// Referência bit a bit do renderizador Vulkan para refatorações (F2 do plano: Vulkan 1.4, push descriptors, dynamic
/// rendering…): o quadro capturado em várias configurações, com as cores e profundidades de cada alvo e a imagem N×
/// publicada. `YW_REF_MODE=save` grava em `YW_REF_DIR` (padrão work/pica_test/ref); `cmp` (padrão) exige 0 bytes
/// diferentes do que foi gravado.
#[test]
#[ignore]
fn pica_ref() {
    let _g = FRAME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let cap = load_cap();
    let dir = std::env::var_os("YW_REF_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../work/pica_test/ref"));
    let save = std::env::var("YW_REF_MODE").is_ok_and(|m| m == "save");
    let _ = std::fs::create_dir_all(&dir);
    register_aot_list(&aot_list(false));
    render::aot::set_enabled(true);
    assert_eq!(render::set_backend(Backend::Vulkan), Backend::Vulkan);
    let out = mem::LINEAR + cap.linear.len() as u32 - 0x60000;
    let mut last_top = 0u32;
    reset(&cap);
    run_lists_hook(&cap, false, &mut |n, k, d| {
        if k.1 == 480 && k.2 == 400 && d > 0 {
            last_top = n;
        }
    });
    // (nome, escala, aniso, sombra forçada, vértices na CPU, w-buffer forçado)
    let cfgs: [(&str, u32, u32, bool, bool, bool); 6] = [
        ("nativo", 1, 0, false, false, false),
        ("nativo_aniso16", 1, 16, false, false, false),
        ("cpu_vs", 1, 0, false, true, false),
        ("wbuf", 1, 0, false, false, true),
        ("n3", 3, 0, true, false, false),
        ("n3_aniso16", 3, 16, true, false, false),
    ];
    let mut total = 0usize;
    for (nome, sc, an, hires, cpu, wbuf) in cfgs {
        render::set_scale(sc);
        render::set_aniso(an);
        render::set_hires_force(hires);
        render::set_cpu_vs(cpu);
        if wbuf {
            super::FORCE_ZMAP.store(((-1.0f32 / 4096.0).to_bits() as u64) << 32 | (1.0f32 / 1024.0).to_bits() as u64, std::sync::atomic::Ordering::Relaxed);
            super::FORCE_WBUF.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let go = |want_hi: bool| -> (Run, Option<Vec<u8>>) {
            reset(&cap);
            yw1_rt::write_bytes(out, &vec![0xEEu8; 400 * 240 * 3]);
            let mut hi = None;
            let r = run_lists_hook(&cap, false, &mut |ln, key, _| {
                if want_hi && ln == last_top {
                    let va = mem::pa_to_va(key.0).unwrap();
                    crate::hle::gpu::display_transfer(va, out, 480 | 400 << 16, 480 | 400 << 16, 0x100_1001);
                    hi = render::vulkan::published(out).map(|h| h.rgba.to_vec());
                }
            });
            (r, hi)
        };
        // duas passadas de aquecimento: pipelines AOT/especializados prontos (a referência é o estado estável)
        for _ in 0..2 {
            let _ = go(hires);
            render::vulkan::wait_pipelines();
        }
        let (r, hi) = go(hires);
        super::FORCE_WBUF.store(false, std::sync::atomic::Ordering::Relaxed);
        super::FORCE_ZMAP.store(0, std::sync::atomic::Ordering::Relaxed);
        let mut items: Vec<(String, Vec<u8>)> = Vec::new();
        for (k, v) in &r.snaps {
            items.push((format!("{nome}_cor_{:x}_{}x{}", k.0, k.1, k.2), v.clone()));
        }
        for (k, v) in &r.dsnaps {
            items.push((format!("{nome}_prof_{:x}_{}x{}", k.0, k.1, k.2), v.clone()));
        }
        if let Some(h) = hi {
            items.push((format!("{nome}_hires"), h));
        } else if hires {
            panic!("{nome}: sem imagem em alta resolução");
        }
        for (f, v) in items {
            let p = dir.join(format!("{f}.bin"));
            if save {
                std::fs::write(&p, &v).unwrap();
            } else {
                let refv = std::fs::read(&p).unwrap_or_else(|_| panic!("sem referência {} (rode com YW_REF_MODE=save)", p.display()));
                let d = refv.len().abs_diff(v.len()) + refv.iter().zip(v.iter()).filter(|(a, b)| a != b).count();
                // imagem da sombra N× (`*_hires`): a profundidade rápida (F4, `YW_VK_FASTZ`) usa o rasterizador fixo, que
                // difere do caminho exato só em empates < 1 unidade de profundidade — tolera até 0,1 %; o que vai para a
                // memória do jogo (cores/profundidades nativas) continua exigindo 0 bytes
                let tol = if f.ends_with("_hires") { v.len() / 1000 } else { 0 };
                println!("{f}: {d} bytes diferentes de {}{}", v.len(), if tol > 0 { format!(" (tolerância {tol})") } else { String::new() });
                if d > tol {
                    total += d;
                }
            }
        }
    }
    render::set_cpu_vs(false);
    render::set_hires_force(false);
    render::set_scale(1);
    render::set_aniso(0);
    render::set_backend(Backend::Soft);
    if save {
        println!("referência gravada em {}", dir.display());
    } else {
        assert_eq!(total, 0, "renderizador diferente da referência");
    }
}

/// W-buffer (0x6D bit 0 = 0), que o quadro capturado não liga: o mesmo quadro com o w-buffer forçado em todos os
/// draws, no soft, no Vulkan com vértices na GPU (AOT/ubershader) e no Vulkan com vértices na CPU. Os três calculam
/// a profundidade do PICA (z/w·zscale + zoffset)·w com w interpolado com correção de perspectiva, cada um do seu
/// jeito (raster.rs; pica.frag com gl_FragCoord.z/gl_FragCoord.w nos dois caminhos), então o buffer de profundidade
/// final e as cores têm de bater. Também mostra quantos pixels de profundidade mudam em relação ao z-buffer.
#[test]
#[ignore]
fn pica_frame_wbuffer() {
    let _g = FRAME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let cap = load_cap();
    let pass = |b: Backend, cpu_vs: bool| -> Run {
        assert_eq!(render::set_backend(b), b);
        render::set_cpu_vs(cpu_vs);
        reset(&cap);
        run_lists(&cap, false)
    };
    let was_cpu = render::cpu_vs_forced();
    let zbuf = pass(Backend::Soft, false);
    // referência de precisão: o z-buffer (sem forçar nada) nos dois caminhos do Vulkan
    let _ = pass(Backend::Vulkan, false);
    render::vulkan::wait_pipelines();
    let zgpu = pass(Backend::Vulkan, false);
    let zcpu = pass(Backend::Vulkan, true);
    // com o zscale do quadro (−1) a profundidade do w-buffer (−z de recorte, distância) satura em tudo: o jogo que liga
    // o w-buffer põe zscale ≈ −1/distância máxima. Escala e deslocamento escolhidos para caber em 0..1 e exercitar
    // também o termo w·zoffset. YW_WBUF_ZMAP="escala,deslocamento" troca.
    let (zs, zo) = std::env::var("YW_WBUF_ZMAP")
        .ok()
        .and_then(|v| v.split_once(',').and_then(|(a, b)| Some((a.trim().parse().ok()?, b.trim().parse().ok()?))))
        .unwrap_or((-1.0f32 / 4096.0, 1.0f32 / 1024.0));
    super::FORCE_ZMAP.store((zs.to_bits() as u64) << 32 | zo.to_bits() as u64, std::sync::atomic::Ordering::Relaxed);
    println!("w-buffer forçado com zscale {zs}, zoffset {zo}");
    super::FORCE_WBUF.store(true, std::sync::atomic::Ordering::Relaxed);
    let soft = pass(Backend::Soft, false);
    let _ = pass(Backend::Vulkan, false);
    render::vulkan::wait_pipelines();
    let gpu = pass(Backend::Vulkan, false);
    let cpu = pass(Backend::Vulkan, true);
    super::raster::WBUF_OLD.store(true, std::sync::atomic::Ordering::Relaxed);
    let old = pass(Backend::Soft, false);
    super::raster::WBUF_OLD.store(false, std::sync::atomic::Ordering::Relaxed);
    super::FORCE_WBUF.store(false, std::sync::atomic::Ordering::Relaxed);
    super::FORCE_ZMAP.store(0, std::sync::atomic::Ordering::Relaxed);
    render::set_cpu_vs(was_cpu);
    render::set_backend(Backend::Soft);

    // |diferença| de profundidade (unidades do formato, 2⁻²⁴ no D24) entre a e b, por faixa: 0, ≤1, ≤16, ≤256,
    // ≤4096, mais; e quantos pixels de a estão no valor máximo (fundo/saturado)
    const EDGES: [i64; 5] = [0, 1, 16, 256, 4096];
    let cmp_depth = |a: &Run, b: &Run| -> ([u64; 6], u64, u64) {
        let (mut h, mut n, mut sat) = ([0u64; 6], 0, 0);
        for (key, da) in &a.dsnaps {
            let Some(db) = b.dsnaps.get(key) else { continue };
            let bpp = da.len() / (key.1 * key.2) as usize;
            let nb = if bpp == 2 { 2 } else { 3 };
            let max = (1i64 << (8 * nb)) - 1;
            let val = |p: &[u8]| p[..nb].iter().rev().fold(0i64, |v, &x| v << 8 | x as i64);
            for (p, q) in da.chunks_exact(bpp).zip(db.chunks_exact(bpp)) {
                let d = (val(p) - val(q)).abs();
                n += 1;
                h[EDGES.iter().position(|&e| d <= e).unwrap_or(5)] += 1;
                sat += (val(p) == max) as u64;
            }
        }
        (h, n, sat)
    };
    // fração com |diferença| ≤ EDGES[k]
    let within = |h: &[u64; 6], n: u64, k: usize| pct(h[..=k].iter().sum(), n);
    let show = |what: &str, h: &[u64; 6], n: u64| {
        println!(
            "{what}: |Δprof.| 0: {:.2}% · ≤1: {:.2}% · ≤16: {:.2}% · ≤256: {:.2}% · ≤4096: {:.2}% de {n}",
            within(h, n, 0),
            within(h, n, 1),
            within(h, n, 2),
            within(h, n, 3),
            within(h, n, 4)
        )
    };
    for (key, _) in &soft.dsnaps {
        println!("buffer de profundidade {:#x} {}x{} fmt {}", key.0, key.1, key.2, key.3);
    }
    let (hz, zn, wsat) = cmp_depth(&soft, &zbuf);
    let (_, _, zsat) = cmp_depth(&zbuf, &soft);
    // valores distintos do w-buffer (um teste em que tudo satura não testa nada)
    let distinct: std::collections::HashSet<&[u8]> =
        soft.dsnaps.values().flat_map(|d| d.chunks_exact(4).map(|p| &p[..3])).collect();
    println!(
        "w-buffer × z-buffer (soft): profundidade diferente em {:.1}% de {zn} · no valor máximo: w-buffer {:.1}%, z-buffer {:.1}% · {} valores distintos com o w-buffer",
        100.0 - within(&hz, zn, 0),
        pct(wsat, zn),
        pct(zsat, zn),
        distinct.len()
    );
    // precisão de referência: o z-buffer (já aceito) nos dois caminhos do Vulkan contra o soft
    let mut base = [0.0f64; 2];
    for (i, (name, r)) in [("vértices na GPU", &zgpu), ("vértices na CPU", &zcpu)].into_iter().enumerate() {
        let (h, n, _) = cmp_depth(r, &zbuf);
        show(&format!("z-buffer, Vulkan ({name}) × soft"), &h, n);
        base[i] = within(&h, n, 3);
    }
    let (ho, no, _) = cmp_depth(&old, &soft);
    show("w-buffer, fórmula ANTIGA (soft) × nova (soft)", &ho, no);
    let mut ok = true;
    for (i, (name, r)) in [("vértices na GPU", &gpu), ("vértices na CPU", &cpu)].into_iter().enumerate() {
        let (h, n, _) = cmp_depth(r, &soft);
        let (mut eq, mut tot) = (0u64, 0u64);
        for (&key, &draws) in &soft.rts {
            if draws == 0 || key.1 == 0 || key.2 == 0 {
                continue;
            }
            let (Some(a), Some(b)) = (images(&cap, r, key), images(&cap, &soft, key)) else { continue };
            eq += a.ours.chunks_exact(4).zip(b.ours.chunks_exact(4)).filter(|(x, y)| close(x, y)).count() as u64;
            tot += (key.1 * key.2) as u64;
        }
        show(&format!("w-buffer, Vulkan ({name}) × soft"), &h, n);
        println!("  cor (±8) {:.2}% de {tot}", pct(eq, tot));
        // a GPU não é exata nem no z-buffer (interpolação do hardware): critério = fração com |Δ| ≤ 256 (≈ 1,5·10⁻⁵
        // da faixa) no mínimo a do z-buffer no mesmo caminho (−0,5 ponto) e acima de 98%
        let w = within(&h, n, 3);
        ok &= n > 0 && w >= base[i].min(99.0) - 0.5 && w > 98.0 && pct(eq, tot) > 99.0;
    }
    assert!(within(&hz, zn, 0) < 50.0, "forçar o w-buffer não mudou a profundidade (o teste não testaria nada)");
    assert!(pct(wsat, zn) < 50.0 && distinct.len() > 1000, "w-buffer saturado: ajuste YW_WBUF_ZMAP");
    assert!(within(&ho, no, 3) < 99.0, "a fórmula antiga passaria no critério: o teste não distingue as duas");
    assert!(ok, "w-buffer: Vulkan e soft divergem");
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

/// Formatos de cor de render target na numeração do PICA (RGBA8 0, RGB8 1, RGB5A1 2, RGB565 3, RGBA4 4): ler com
/// `read_pixel(pica_color_fmt_to_gsp(f))` tem de dar o mesmo que `decode` (escrito na ordem do PICA, à parte), e
/// escrever de volta o valor lido tem de reproduzir os bytes. Antes da correção o RT RGB5A1/RGB565 era lido e
/// escrito com a numeração do GSP (os dois trocados).
#[test]
fn rt_color_formats_roundtrip() {
    let _g = FRAME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    init_mem();
    let a = mem::VRAM + mem::VRAM_SIZE - 0x100;
    let raws: [u32; 6] = [0x1234_5678, 0xabcd_ef01, 0x0000_f801, 0x0000_07e0, 0x00ff_001f, 0x8000_0001];
    for fmt in 0..5u32 {
        let bpp = bpp(fmt) as usize;
        for raw in raws {
            let mut word = raw.to_le_bytes();
            word[bpp..].fill(0);
            yw1_rt::write_bytes(a, &[0; 4]);
            yw1_rt::write_bytes(a, &word[..bpp]);
            let got = read_pixel(a, pica_color_fmt_to_gsp(fmt));
            assert_eq!(got, decode(fmt, word), "leitura fmt {fmt} raw {raw:#x}");
            yw1_rt::write_bytes(a, &[0; 4]);
            write_pixel(a, pica_color_fmt_to_gsp(fmt), got);
            let mut back = [0u8; 4];
            yw1_rt::read_bytes(a, &mut back[..bpp]);
            assert_eq!(back, word, "escrita fmt {fmt} raw {raw:#x}");
        }
    }
}

/// A transferência em bloco (`gpu::transfer_buffered`) dá exatamente os mesmos bytes que a pixel a pixel, em todos os
/// formatos de entrada e saída, reduções (nenhuma, 2×1, 2×2), inversão vertical e modos (blocos → linear, linear →
/// blocos, sem swizzle), inclusive tamanhos que não são múltiplos de 8. A saída começa com lixo: os bytes que a
/// transferência não escreve têm de ficar como estavam.
#[test]
fn display_transfer_igual_ao_pixel_a_pixel() {
    init_mem();
    const BASE: u32 = 0x3000_0000;
    static MAP: std::sync::Once = std::sync::Once::new();
    MAP.call_once(|| mem::map(BASE, 0x30_0000, "teste DisplayTransfer"));
    let (inp, o1, o2) = (BASE, BASE + 0x10_0000, BASE + 0x20_0000);
    let mut seed = 0x1234_5678u32;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    let data: Vec<u8> = (0..0x10_0000).map(|_| rnd() as u8).collect();
    let lixo: Vec<u8> = (0..0x10_0000).map(|_| rnd() as u8).collect();
    yw1_rt::write_bytes(inp, &data);
    let mut casos = 0;
    for (iw, ih, ow, oh) in [(64u32, 48u32, 64u32, 48u32), (240, 400, 240, 400), (40, 24, 40, 24), (64, 48, 128, 96), (72, 20, 72, 20)] {
        for fin in 0..5u32 {
            for fout in 0..5u32 {
                for scale in 0..3u32 {
                    for modo in [0u32, 2, 0x20, 0x22] {
                        for flip in [0u32, 1] {
                            let flags = flip | modo | fin << 8 | fout << 12 | scale << 24;
                            let (ind, outd) = (ih << 16 | iw, oh << 16 | ow);
                            yw1_rt::write_bytes(o1, &lixo);
                            yw1_rt::write_bytes(o2, &lixo);
                            crate::hle::gpu::transfer_per_pixel(inp, o1, ind, outd, flags);
                            crate::hle::gpu::transfer_fast_for_test(inp, o2, ind, outd, flags);
                            let (mut a, mut b) = (vec![0u8; 0x10_0000], vec![0u8; 0x10_0000]);
                            yw1_rt::read_bytes(o1, &mut a);
                            yw1_rt::read_bytes(o2, &mut b);
                            if let Some(i) = a.iter().zip(&b).position(|(x, y)| x != y) {
                                panic!("{iw}x{ih} → {ow}x{oh}, flags {flags:#x}: primeiro byte diferente em +{i:#x} ({} vs {})", a[i], b[i]);
                            }
                            casos += 1;
                        }
                    }
                }
            }
        }
    }
    assert_eq!(casos, 5 * 5 * 5 * 3 * 4 * 2);
}

/// Interpolação de quadros × passada nativa (bug do texto embaralhado na HUD, H1): o replay (re-renderização de um
/// quadro intermediário) não pode deixar rastro no cache de hash de textura por lista (`tex_hash`). Antes, o replay
/// relia a textura da memória e gravava o hash com o nº da lista SEGUINTE; se a CPU mudasse a textura depois disso, a
/// 1ª lista nativa seguinte achava o hash velho no cache e desenhava com a versão velha.
///
/// Roteiro: quadro 1 (listas 1–7, captura ligada) → replay forçado (α = 0,5, o quadro contra si mesmo) → a CPU muda
/// os bytes de TODAS as texturas que os desenhos capturados usam → quadro 2 a partir da 1ª lista do alvo de cima (a
/// lista com o mesmo nº de lista do replay). A memória (VRAM + heap linear) tem de sair idêntica, byte a byte, à de
/// uma rodada sem o replay com a mesma mudança. Confere também que a mudança altera o resultado (senão o teste não
/// provaria nada) e que a rodada sem replay é determinística.
#[test]
#[ignore]
fn interp_nao_contamina_nativo() {
    let _g = FRAME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let cap = load_cap();
    assert_eq!(render::set_backend(Backend::Vulkan), Backend::Vulkan);
    let spec = render::vulkan::fs_spec();
    // só o ubershader de fragmento: o especializado compila em segundo plano e trocaria de pipeline entre rodadas
    render::vulkan::set_fs_spec(false);
    // captura da interpolação ligada em TODAS as rodadas (liga também a resolução interna); só o replay muda
    render::set_interp(120);
    let datas: Vec<Vec<u8>> = cap.lists.iter().map(|&(n, _, _)| std::fs::read(cap.dir.join(format!("list_{n:02}.bin"))).unwrap()).collect();

    // 1ª lista que desenha no alvo de cima (480×400) e o endereço dele
    let mut top: Option<(u32, u32)> = None;
    reset(&cap);
    render::vulkan::test_interp_discard();
    run_lists_hook(&cap, false, &mut |n, k, d| {
        if top.is_none() && k.1 == 480 && k.2 == 400 && d > 0 {
            top = Some((n, mem::pa_to_va(k.0).unwrap()));
        }
    });
    render::vulkan::wait_pipelines();
    let (first_top, va) = top.expect("nenhuma lista desenha no alvo 480×400");
    // DisplayTransfer da tela de cima (480×400 → 240×400, redução 2×1, invertida); a saída não é lida no replay
    let req = render::vulkan::PresentReq { inp: va, iw: 480, ih: 400, sx: 2, sy: 1, flip: true, ow: 240, hh: 400, out: 0, out_bytes: 240 * 400 * 3 };

    let memoria = || -> Vec<u8> {
        render::vulkan::sync_all();
        let mut v = vec![0u8; mem::VRAM_SIZE as usize + cap.linear.len()];
        let (a, b) = v.split_at_mut(mem::VRAM_SIZE as usize);
        yw1_rt::read_bytes(mem::VRAM, a);
        yw1_rt::read_bytes(mem::LINEAR, b);
        v
    };
    // uma rodada: quadro 1, replay (ou não), mudança das texturas (ou não), quadro 2 a partir de `first_top`
    let rodada = |replay: bool, mudar: &[(u32, u32)]| -> (Vec<u8>, Vec<(u32, u32)>) {
        reset(&cap);
        render::vulkan::test_interp_discard();
        let _ = run_lists(&cap, false);
        let texs = if replay {
            render::vulkan::test_interp_replay(req).unwrap_or_else(|e| panic!("replay: {e}"))
        } else {
            render::vulkan::test_interp_discard();
            Vec::new()
        };
        for &(a, n) in mudar {
            let mut b = vec![0u8; n as usize];
            yw1_rt::read_bytes(a, &mut b);
            for x in b.iter_mut() {
                *x ^= 0x5a;
            }
            yw1_rt::write_bytes(a, &b);
        }
        for (&(n, a, s), data) in cap.lists.iter().zip(&datas) {
            if n < first_top {
                continue;
            }
            prep_list(n, a, data);
            super::execute(a, s);
        }
        (memoria(), texs)
    };
    let difs = |a: &[u8], b: &[u8]| a.iter().zip(b).filter(|(x, y)| x != y).count();

    // sonda: quais texturas os desenhos capturados no alvo de cima usam
    let (_, mut texs) = rodada(true, &[]);
    texs.sort_unstable();
    texs.dedup();
    assert!(!texs.is_empty(), "os desenhos capturados no alvo de cima não usam textura");
    println!("alvo de cima {va:#x} (1ª lista {first_top}): {} texturas capturadas", texs.len());

    let base = rodada(false, &[]).0;
    let ref1 = rodada(false, &texs).0;
    let ref2 = rodada(false, &texs).0;
    let got = rodada(true, &texs).0;
    let (d_det, d_mud, d_got) = (difs(&ref1, &ref2), difs(&base, &ref1), difs(&ref1, &got));
    println!("sem replay ×2: {d_det} bytes diferentes · efeito da mudança das texturas: {d_mud} bytes · com replay × sem: {d_got} bytes");
    render::set_interp(0);
    render::vulkan::set_fs_spec(spec);
    render::set_backend(Backend::Soft);
    assert_eq!(d_det, 0, "a rodada sem replay não é determinística: o teste não pode decidir");
    assert!(d_mud > 0, "mudar as texturas não mudou a memória: o teste não prova nada");
    assert_eq!(d_got, 0, "o replay da interpolação contaminou a passada nativa seguinte ({d_got} bytes diferentes)");
}

/// F4: profundidade rápida (`render::vulkan::fastz`) na sombra N× — quantos draws a usam e quanto a imagem N×
/// publicada muda em relação ao caminho exato (gl_FragDepth). Os alvos nativos (memória do jogo, cor e profundidade)
/// têm de sair iguais, bit a bit; a imagem N× pode mudar pouco (desempates < 1 unidade de profundidade): aceite
/// ≤ 0,1 % dos pixels. Escala `YW_BENCH_SCALE` (padrão 3), aniso `YW_BENCH_ANISO` (padrão 16).
/// `cargo test --release --lib -- --ignored pica_frame_fastz --nocapture`
#[test]
#[ignore]
fn pica_frame_fastz() {
    let _g = FRAME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let cap = load_cap();
    let env = |k: &str, d: u32| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
    let (scale, an) = (env("YW_BENCH_SCALE", 3), env("YW_BENCH_ANISO", 16));
    register_aot_list(&aot_list(false));
    render::aot::set_enabled(true);
    assert_eq!(render::set_backend(Backend::Vulkan), Backend::Vulkan);
    let out = mem::LINEAR + cap.linear.len() as u32 - 0x60000;
    let mut last_top = 0u32;
    reset(&cap);
    run_lists_hook(&cap, false, &mut |n, k, d| {
        if k.1 == 480 && k.2 == 400 && d > 0 {
            last_top = n;
        }
    });
    let was = render::vulkan::fastz();
    render::set_scale(scale);
    render::set_aniso(an);
    render::set_hires_force(true);
    let go = || -> (Run, Vec<u8>) {
        reset(&cap);
        yw1_rt::write_bytes(out, &vec![0xEEu8; 400 * 240 * 3]);
        let mut hi = None;
        let r = run_lists_hook(&cap, false, &mut |ln, key, _| {
            if ln == last_top {
                let va = mem::pa_to_va(key.0).unwrap();
                crate::hle::gpu::display_transfer(va, out, 480 | 400 << 16, 480 | 400 << 16, 0x100_1001);
                hi = render::vulkan::published(out).map(|h| h.rgba.to_vec());
            }
        });
        (r, hi.expect("sem imagem em alta resolução"))
    };
    let pass = |fz: bool| {
        render::vulkan::set_fastz(fz);
        for _ in 0..2 {
            let _ = go();
            render::vulkan::wait_pipelines();
        }
        let s0 = render::vulkan::stats();
        let (r, hi) = go();
        let s1 = render::vulkan::stats();
        (r, hi, s1[0] - s0[0], s1[18] - s0[18], s1[19] - s0[19])
    };
    let (re, he, _, fe, _) = pass(false);
    let (rf, hf, draws, fz, late) = pass(true);
    render::vulkan::set_fastz(was);
    render::set_hires_force(false);
    render::set_scale(1);
    render::set_aniso(0);
    render::set_backend(Backend::Soft);
    let difs = |a: &BTreeMap<RtKey, Vec<u8>>, b: &BTreeMap<RtKey, Vec<u8>>| -> usize {
        a.iter().map(|(k, x)| b.get(k).map_or(x.len(), |y| x.len().abs_diff(y.len()) + x.iter().zip(y).filter(|(p, q)| p != q).count())).sum()
    };
    let (dc, dz) = (difs(&re.snaps, &rf.snaps), difs(&re.dsnaps, &rf.dsnaps));
    assert_eq!(he.len(), hf.len());
    let px = he.len() / 4;
    let dpx = he.chunks_exact(4).zip(hf.chunks_exact(4)).filter(|(a, b)| a != b).count();
    let pct = 100.0 * dpx as f64 / px.max(1) as f64;
    println!(
        "FASTZ escala {scale} aniso {an}: {draws} draws no quadro, {fz} com profundidade rápida (ainda compilando: {late}) · nativo: cor {dc} bytes, profundidade {dz} bytes diferentes · imagem N×: {dpx} de {px} pixels diferentes ({pct:.4} %)"
    );
    assert_eq!(fe, 0, "YW_VK_FASTZ desligado e ainda houve draws rápidos");
    assert!(fz > 0 && late == 0, "a profundidade rápida não foi usada ({fz} draws, {late} compilando)");
    assert_eq!(dc + dz, 0, "a profundidade rápida mudou a passada nativa (memória do jogo)");
    assert!(pct <= 0.1, "imagem N× com {pct:.4} % dos pixels diferentes do caminho exato (aceite ≤ 0,1 %)");
}
