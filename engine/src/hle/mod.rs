//! HLE do 3DS para o código recompilado: memória, kernel, serviços e o laço de VBlank.
//!
//! Velocidade travada na normal: VBlank a ≈59,83 Hz (LCD do 3DS) e quadros de áudio a 32728/160 Hz; o jogo se
//! cadencia sozinho esperando esses eventos. Sem desbloqueio de FPS por enquanto (pedido do Paulo).

pub mod fsr;
pub mod gpu;
pub mod hooks;
pub mod input;
pub mod kernel;
pub mod logs;
pub mod mem;
pub mod memmap;
pub mod menu;
pub mod present;
pub mod script;
pub mod services;
pub mod settings;
pub mod watchdog;
pub mod window;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Taxa de quadros do LCD do 3DS: 268111856 / (413 · 2 · 4 · 1355) ≈ 59,8261 Hz [3dbrew "LCD"].
pub const VBLANK_HZ: f64 = 59.8261;

/// Multiplicador da taxa de VBlank (`YW_VBLANK_MULT`, padrão 1): o console real tem 59,83 Hz; com 2 o jogo recebe o
/// dobro de VBlanks por segundo de relógio. O relógio do sistema (`GetSystemTick`, `console_now`) NÃO muda — é o que
/// mostra se a lógica do jogo anda por quadro ou por tempo (deltatime).
pub fn vblank_mult() -> f64 {
    static M: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *M.get_or_init(|| std::env::var("YW_VBLANK_MULT").ok().and_then(|v| v.parse().ok()).filter(|m: &f64| (0.25..=8.0).contains(m)).unwrap_or(1.0))
}

/// VBlanks por segundo de relógio de console (usado para converter a contagem de VBlanks em segundos).
pub fn vblank_hz() -> f64 {
    VBLANK_HZ * vblank_mult()
}
const AUDIO_HZ: f64 = 32728.498 / 160.0;

const BSS_END: u32 = 0x005f_ccd4 + 0x000a_8408;
const MAIN_PRIO: i32 = 0x30;
const MAIN_STACK: u32 = 0x6000;

static CRASH: Mutex<Option<String>> = Mutex::new(None);

/// Fator do relógio do console. **Sempre 1 para jogar** (velocidade travada na normal, pedido do Paulo).
/// `YW_SPEED=N` (0,25–8) só existe para os testes sem janela chegarem mais rápido longe no jogo: acelera por igual o
/// VBlank, os quadros de áudio, o tick do sistema e os timeouts do kernel, então o jogo vê o mesmo tempo de console.
pub fn speed() -> f64 {
    static S: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *S.get_or_init(|| {
        std::env::var("YW_SPEED")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .map_or(1.0, |v| v.clamp(0.25, 8.0))
    })
}
static STARTED: AtomicBool = AtomicBool::new(false);
pub static VBLANKS: AtomicU64 = AtomicU64::new(0);

/// Pausa do console (menu da janela aberto): os relógios de VBlank e DSP param e, ao voltar, a base de tempo deles
/// anda a duração da pausa (o jogo não "corre" para recuperar). `PAUSED_NS` = soma das pausas já terminadas.
/// O kernel também desconta a pausa: `kernel::system_tick` (svc GetSystemTick) e os prazos de sleep/timeout usam
/// `kernel::console_now()` = relógio do host − `paused_ns()`, então nada anda nem dispara enquanto o menu está aberto.
static PAUSED: AtomicBool = AtomicBool::new(false);
static PAUSE: Mutex<Option<Instant>> = Mutex::new(None);
static PAUSED_NS: AtomicU64 = AtomicU64::new(0);

pub fn set_paused(p: bool) {
    let mut g = PAUSE.lock().unwrap();
    match (p, *g) {
        (true, None) => *g = Some(Instant::now()),
        (false, Some(t)) => {
            // soma antes de liberar: quem vê PAUSED = false já vê a pausa contada
            PAUSED_NS.fetch_add(t.elapsed().as_nanos() as u64, Ordering::SeqCst);
            *g = None;
        }
        _ => return,
    }
    PAUSED.store(p, Ordering::SeqCst);
}

pub fn paused() -> bool {
    PAUSED.load(Ordering::SeqCst)
}

/// Tempo total de pausa até agora (pausas terminadas + a que está em andamento), para quem precisar descontar do
/// relógio do console (ex.: `kernel::system_tick`).
pub fn paused_ns() -> u64 {
    let g = PAUSE.lock().unwrap();
    PAUSED_NS.load(Ordering::SeqCst) + g.map_or(0, |t| t.elapsed().as_nanos() as u64)
}

/// Espera o instante do `n`-ésimo evento de um relógio de `hz` (começado em `t0`), descontando as pausas.
fn clock_wait(t0: Instant, n: u64, hz: f64) {
    loop {
        while paused() {
            std::thread::sleep(Duration::from_millis(5));
        }
        let due = t0 + Duration::from_nanos(PAUSED_NS.load(Ordering::SeqCst)) + Duration::from_secs_f64(n as f64 / hz);
        match due.checked_duration_since(Instant::now()) {
            Some(d) => std::thread::sleep(d),
            None if !paused() => return,
            None => {}
        }
    }
}

/// Uma thread guest parou com erro (panic no código recompilado, ExitProcess…).
pub fn crashed(msg: String) {
    let mut c = CRASH.lock().unwrap();
    if c.is_none() {
        *c = Some(msg);
    }
}

pub fn crash_message() -> Option<String> {
    CRASH.lock().unwrap().clone()
}

/// Ponto de preempção chamado pelo código recompilado (`rt::poll`) quando `rt::PREEMPT` está ligado.
#[no_mangle]
pub fn yw1_preempt(_c: &mut yw1_rt::Cpu) {
    kernel::K.preempt_point();
}

/// O endereço guest está mapeado? (verificadores de depuração do `rt`, que não podem arriscar uma falha de memória)
#[no_mangle]
pub fn yw1_is_mapped(a: u32) -> bool {
    mem::is_mapped(a)
}

/// Ponto de entrada das syscalls do código recompilado.
#[no_mangle]
pub fn yw1_svc(c: &mut yw1_rt::Cpu, imm: u32) {
    kernel::svc(c, imm);
}

fn code_bin() -> Vec<u8> {
    let p = std::env::var_os("YW_CODEBIN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../data/ghidra/code.bin"));
    std::fs::read(&p).unwrap_or_else(|e| panic!("[hle] {}: {e} (defina YW_CODEBIN)", p.display()))
}

/// Página de configuração (0x1FF80000) e página compartilhada (0x1FF81000), como o kernel 11.x preenche.
fn system_pages() {
    use yw1_rt::{w32, w8};
    let c = mem::CONFIG;
    w8(c + 0x02, 0); // revisão do kernel
    w8(c + 0x03, 0x34); // menor
    w8(c + 0x04, 2); // maior → 2.52
    w32(c + 0x10, 0x0004_0130); // NS tid
    w8(c + 0x1c, 1); // ENVINFO: varejo
    w8(c + 0x1d, 0); // UNITINFO: produto
    w32(c + 0x30, 0); // APPMEMTYPE: 64 MB
    w32(c + 0x40, mem::APP_MEMORY); // APPMEMALLOC
    w32(c + 0x44, 0x0140_0000); // SYSMEMALLOC
    w32(c + 0x48, 0x0120_0000); // BASEMEMALLOC
    w8(c + 0x62, 0);
    w8(c + 0x63, 0x34);
    w8(c + 0x64, 2);
    let s = mem::SHARED_PAGE;
    w32(s + 0x04, 1); // RUNNING_HW: produto
    // DATETIME_0: milissegundos desde 1900-01-01 (relógio do sistema, como o RTC do console)
    let unix_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let ms1900 = unix_ms + 2_208_988_800_000;
    w32(s + 0x20, ms1900 as u32);
    w32(s + 0x24, (ms1900 >> 32) as u32);
    w32(s + 0x30, 268_111_856);
    w32(s + 0x80, 0f32.to_bits()); // slider 3D
    w8(s + 0x85, 1); // bateria: adaptador
}

/// Faz o commit sob demanda das páginas guest (`mem::commit_on_fault`) e mostra as falhas de acesso de verdade do
/// código guest (endereço guest, leitura/escrita) antes do processo cair.
#[cfg(windows)]
fn install_fault_handler() {
    #[repr(C)]
    struct ExceptionRecord {
        code: u32,
        flags: u32,
        next: *mut ExceptionRecord,
        address: *mut u8,
        nparams: u32,
        info: [usize; 15],
    }
    #[repr(C)]
    struct Pointers {
        record: *mut ExceptionRecord,
        context: *mut u8,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn AddVectoredExceptionHandler(first: u32, h: unsafe extern "system" fn(*mut Pointers) -> i32) -> *mut u8;
    }
    unsafe extern "system" fn handler(p: *mut Pointers) -> i32 {
        // SAFETY: o Windows passa um EXCEPTION_POINTERS válido
        let r = unsafe { &*(*p).record };
        match r.code {
            0xc000_0005 => {
                // página guest marcada e ainda sem commit (commit sob demanda, `mem::commit_on_fault`): repete o acesso
                if mem::commit_on_fault(r.info[1]) {
                    return -1; // EXCEPTION_CONTINUE_EXECUTION
                }
                let base = yw1_rt::base() as usize;
                let a = r.info[1];
                let kind = if r.info[0] == 1 { "escrita" } else { "leitura" };
                if a >= base && a < base + (1usize << 32) {
                    eprintln!(
                        "[hle] FALHA de {kind} no endereço guest {:#010x} (thread {}, código host {:p})",
                        a - base,
                        kernel::current(),
                        r.address
                    );
                    let cpu = kernel::CPU.with(|p| p.get());
                    if !cpu.is_null() {
                        // SAFETY: ponteiro para o Cpu da própria thread (vivo enquanto ela roda)
                        let c = unsafe { &*cpu };
                        eprintln!("[hle] função guest {:#010x}
{}", c.func, kernel::dump(c));
                        eprintln!("[hle] últimas chamadas da fila de áudio:
{}", yw1_rt::trace_dump(300));
                    }
                } else {
                    eprintln!("[hle] FALHA de {kind} em {a:#x} fora da memória guest (código host {:p})", r.address);
                }
            }
            0xc000_00fd => eprintln!("[hle] estouro de pilha do host na thread {}", kernel::current()),
            _ => {}
        }
        0 // EXCEPTION_CONTINUE_SEARCH
    }
    // SAFETY: registra um tratador que só lê o registro da exceção
    unsafe {
        AddVectoredExceptionHandler(1, handler);
    }
}
#[cfg(not(windows))]
fn install_fault_handler() {}

/// O modo estéreo já foi decidido (pela janela, `window::Config::load`)?
static STEREO_SET: AtomicBool = AtomicBool::new(false);

/// 3D estéreo (`graficos.estereo_3d`; `YW_STEREO=1/0` vence o arquivo). O jogo escolhe o modo de tela uma vez, no
/// boot (`FUN_00114588`, gancho "antes" registrado em `boot`): tem de valer antes da primeira instrução guest.
pub fn set_stereo_from_settings(arquivo: bool) {
    let on = match std::env::var("YW_STEREO") {
        Ok(v) => v != "0",
        Err(_) => arquivo,
    };
    yw1_rt::set_stereo(on);
    STEREO_SET.store(true, Ordering::SeqCst);
}

/// Ganchos que o jogo precisa para rodar como configurado (`hooks.rs`; valem mesmo com `YW_HOOKS=0`). Registrados no
/// `boot`, antes da primeira instrução guest.
fn registrar_ganchos_essenciais() {
    // FUN_00114588(config, ...): inicialização do vídeo. `if ((char)param_1[5] != 0) { FUN_001254c8(0x702);
    // DAT_00608b47 = 1; }` — o byte em config+0x14 liga o 3D estéreo (FUN_001254c8 = nngxSetDisplayMode, único
    // chamador com 0x702; DAT_00608b47 = "estéreo ligado", que o controle do 3D e o menu 3D consultam). Sem estéreo
    // (`rt::stereo()` falso, o padrão) o byte é zerado na entrada: o jogo fica no modo 0x700 e desenha um olho só.
    // Gancho de entrada = só chamadas de verdade (pc = entrada, r0 = config). Era uma sonda do gerador (TRACES).
    let mono = hooks::registrar(
        0x114588,
        hooks::Tipo::antes(|c| {
            if !yw1_rt::stereo() {
                yw1_rt::w8(c.r[0].wrapping_add(0x14), 0);
            }
        }),
        "modo mono (FUN_00114588)",
        0,
        true,
    );
    if let Err(e) = mono {
        panic!("[hle] {e}");
    }
}

/// Carrega o código, prepara a memória e começa a thread principal do jogo (entrada 0x100000).
pub fn boot() {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    // sem janela (`run_headless`) ninguém leu as configurações ainda: o modo estéreo vem do arquivo aqui
    if !STEREO_SET.load(Ordering::SeqCst) {
        set_stereo_from_settings(settings::load().graficos.estereo_3d);
    }
    eprintln!("[hle] 3D estéreo {}", if yw1_rt::stereo() { "ligado" } else { "desligado (modo mono)" });
    registrar_ganchos_essenciais();
    install_fault_handler();
    mem::init();
    let code = code_bin();
    mem::load_code(&code, BSS_END);
    mem::map(mem::STACK_TOP - 0x10_0000, 0x10_0000, "pilha principal");
    mem::map(services::dsp::DSP_RAM, 0x8_0000, "RAM do DSP");
    system_pages();
    let _ = MAIN_STACK;
    kernel::spawn_thread(mem::CODE, 0, mem::STACK_TOP, MAIN_PRIO);
    // relógios do console: VBlank (GSP + HID) e quadros de áudio (DSP)
    std::thread::Builder::new()
        .name("vblank".into())
        .spawn(|| {
            let t0 = Instant::now();
            let mut n: u64 = 0;
            loop {
                n += 1;
                clock_wait(t0, n, vblank_hz() * speed());
                services::gsp::interrupt(services::gsp::PDC0);
                services::gsp::interrupt(services::gsp::PDC1);
                services::hid::update(kernel::system_tick());
                VBLANKS.fetch_add(1, Ordering::Relaxed);
            }
        })
        .unwrap();
    std::thread::Builder::new()
        .name("dsp".into())
        .spawn(|| {
            let t0 = Instant::now();
            let mut n: u64 = 0;
            loop {
                n += 1;
                clock_wait(t0, n, AUDIO_HZ * speed());
                services::dsp::audio_frame();
            }
        })
        .unwrap();
}

/// Entrada do executável gerado (`work/recomp/app`): abre a janela do jogo. Com `YW_HEADLESS_SECS=N` roda sem
/// janela por N segundos e grava as telas em `work/recomp_frames/` (testes / comparação com o Azahar).
pub fn main() {
    if let Some(secs) = std::env::var("YW_HEADLESS_SECS").ok().and_then(|s| s.parse().ok()) {
        logs::init("sem janela");
        run_headless(secs);
        logs::flush();
        std::process::exit(if crash_message().is_some() { 1 } else { 0 });
    }
    logs::init("janela");
    // configurações (renderizador, idioma, volume) valem desde o início: lidas antes do boot
    let cfg = window::Config::load();
    boot();
    watchdog::start();
    // roteiro de entrada (`YW_INPUT`/`YW_INPUT_FILE`) também com janela: `tools/window_run.py --input`
    script::start_from_env();
    window::run(cfg);
    if let Some(m) = crash_message() {
        eprintln!("[hle] o jogo parou: {m}
[hle] threads:
{}", kernel::threads_report());
    }
    hooks::log_resumo();
    // cache do driver + lista de pipelines usados (pré-compilação do próximo boot) também ao fechar a janela
    if crate::render::backend() == crate::render::Backend::Vulkan {
        crate::render::vulkan::save_pipeline_cache();
    }
    logs::flush();
    std::process::exit(0);
}

/// Roda sem janela por `seconds` e grava as telas em `work/recomp_frames/` (.rgba + tamanho no nome).
pub fn run_headless(seconds: f64) {
    boot();
    watchdog::start();
    script::start_from_env();
    if speed() != 1.0 {
        eprintln!("[hle] relógio do console acelerado {}× (YW_SPEED, só para testes)", speed());
    }
    let t0 = Instant::now();
    let out = std::env::var_os("YW_FRAMES_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../work/recomp_frames"));
    let _ = std::fs::create_dir_all(&out);
    // YW_SHOT_EVERY = intervalo entre capturas (s do console, padrão 0,5); nome = instante do console em décimos
    let every: f64 = std::env::var("YW_SHOT_EVERY").ok().and_then(|s| s.parse().ok()).unwrap_or(0.5);
    // YW_SHOT_FROM = primeira captura (s do console; para capturar todo quadro só num trecho)
    let mut next_shot = std::env::var("YW_SHOT_FROM").ok().and_then(|s| s.parse().ok()).unwrap_or(every);
    // a cada 5 s do console: quadros que o jogo apresentou por segundo (trocas da tela de cima) — a meta é 30
    let (mut next_rate, mut last_swaps) = (5.0, 0u64);
    let mut last_prof = [0u64; crate::pica::PROF.len()];
    let mut last_st = [0u64; crate::render::vulkan::NSTATS];
    let mut last_host = Instant::now();
    let mut last_idle = 0u64;
    let mut last_post = 0u64;
    // draws do shader de vértice na GPU: com shader AOT / com o ubershader (contadores do Vulkan)
    let vs_counts = || {
        if crate::render::backend() == crate::render::Backend::Vulkan {
            let st = crate::render::vulkan::stats();
            (st[8], st[9])
        } else {
            (0, 0)
        }
    };
    let mut last_vs = (0u64, 0u64);
    // mapa da memória do processo a cada 30 s do console e no fim (`YW_MEM_MAP=1`, `memmap.rs`)
    let mut next_map = 30.0;
    while t0.elapsed().as_secs_f64() < seconds {
        std::thread::sleep(Duration::from_millis(20));
        if let Some(m) = crash_message() {
            eprintln!("[hle] parou: {m}");
            break;
        }
        let t = VBLANKS.load(Ordering::Relaxed) as f64 / vblank_hz();
        if t >= next_rate {
            let s = services::gsp::SWAPS[0].load(Ordering::Relaxed);
            // fração do tempo de relógio (em %) gasta em: lista PICA, shader de vértice, vulkan::draw, sincronismo com a
            // GPU (devolução de RT / espera de fence), DisplayTransfer, MemoryFill, alta resolução + interpolação — tudo
            // na thread "gpu" (gsp.rs), que roda ao lado da CPU do jogo; "ociosa" é só a CPU do jogo (sem thread guest)
            let host = last_host.elapsed().as_nanos().max(1) as f64;
            let p: Vec<String> = crate::pica::PROF
                .iter()
                .zip(last_prof.iter_mut())
                .map(|(a, l)| {
                    let v = a.load(Ordering::Relaxed);
                    let d = v - *l;
                    *l = v;
                    format!("{:.0}", d as f64 / host * 100.0)
                })
                .collect();
            let idle_ns = kernel::IDLE_NS.load(Ordering::Relaxed);
            let idle = (idle_ns - last_idle) as f64 / host * 100.0;
            last_idle = idle_ns;
            // parte do DisplayTransfer depois do PPF (imagem N×, intermediários; `gsp::GPU_POST_NS`): já contida em
            // "alta resolução e interpolação", mostrada à parte para ver o que saiu do caminho do jogo
            let post_ns = services::gsp::GPU_POST_NS.load(Ordering::Relaxed);
            let post = (post_ns - last_post) as f64 / host * 100.0;
            last_post = post_ns;
            last_host = Instant::now();
            let vs = vs_counts();
            let (aot, uber) = (vs.0 - last_vs.0, vs.1 - last_vs.1);
            last_vs = vs;
            // backend Vulkan: lotes/s, ms/s esperando a GPU, GPU executando (cena nativa · sombra N×), convertendo
            let st = crate::render::vulkan::stats();
            let dv = |i: usize| st[i].saturating_sub(last_st[i]) as f64 / 5.0;
            let vk_line = format!(
                " · vulkan: {:.0} lotes/s, espera {:.0} ms/s (GPU executando: nativo {:.0} + sombra {:.0} ms/s; sombra pulada no olho escondido {:.0} lotes/s), convertendo {:.0} ms/s · fragmento especializado {:.0}% dos draws",
                dv(1),
                dv(5) / 1000.0,
                dv(13) / 1000.0,
                dv(14) / 1000.0,
                dv(17),
                dv(6) / 1000.0,
                dv(15) * 100.0 / (dv(15) + dv(16)).max(1.0)
            );
            last_st = st;
            let adiadas = services::gsp::SWAPS_ADIADAS.swap(0, Ordering::Relaxed);
            let vk_line = format!("{vk_line} · trocas adiadas (buffer ainda na fila da GPU) {adiadas}");
            // memória: RAM privada do processo (Windows: PROCESS_MEMORY_COUNTERS_EX.PrivateUsage, via
            // K32GetProcessMemoryInfo do kernel32, declarado à mão como o resto do projeto) e o que o backend Vulkan
            // informa (`mem_report`: VRAM usada/orçamento do VK_EXT_memory_budget, buffers mapeados do host, texturas)
            fn ram_privada() -> u64 {
                #[cfg(windows)]
                {
                    #[repr(C)]
                    #[derive(Default)]
                    struct Pmc {
                        cb: u32,
                        page_faults: u32,
                        peak_ws: usize,
                        ws: usize,
                        quota_peak_paged: usize,
                        quota_paged: usize,
                        quota_peak_nonpaged: usize,
                        quota_nonpaged: usize,
                        pagefile: usize,
                        peak_pagefile: usize,
                        private: usize,
                    }
                    #[link(name = "kernel32")]
                    extern "system" {
                        fn GetCurrentProcess() -> *mut u8;
                        fn K32GetProcessMemoryInfo(p: *mut u8, c: *mut Pmc, cb: u32) -> i32;
                    }
                    let mut c = Pmc { cb: std::mem::size_of::<Pmc>() as u32, ..Default::default() };
                    // SAFETY: pseudo-handle do próprio processo; `c` tem o tamanho anunciado em `cb`
                    if unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) } != 0 {
                        return c.private as u64;
                    }
                }
                0
            }
            let mb = |b: u64| b as f64 / (1024.0 * 1024.0);
            let (vram, orc, host, tex) = crate::render::vulkan::mem_report();
            let vk_line = format!(
                "{vk_line} · memória: RAM privada {:.0} MB · VRAM {:.0}/{:.0} MB · host mapeado {:.0} MB · texturas {:.0} MB",
                mb(ram_privada()),
                mb(vram),
                mb(orc),
                mb(host),
                mb(tex)
            );
            eprintln!(
                "[hle] {t:.0}s: {:.1} quadros/s do jogo · CPU do jogo ociosa {idle:.0}% · thread da GPU (% do relógio): lista PICA {} (shader {} · vk draw {}) · sincronismo {} · transfer {} · fill {} · alta resolução e interpolação {} (pós-transfer {post:.0} %) · draws de vértice AOT {aot} / ubershader {uber}{vk_line}",
                (s - last_swaps) as f64 / 5.0,
                p[0], p[1], p[2], p[3], p[4], p[5], p[6]
            );
            (next_rate, last_swaps) = (next_rate + 5.0, s);
        }
        if memmap::enabled() && t >= next_map {
            next_map += 30.0;
            eprintln!("[mem] {t:.0}s\n{}", memmap::report());
        }
        if t < next_shot {
            continue;
        }
        next_shot += every;
        // nome = instante em décimos; capturas mais densas que 0,1 s usam centésimos (senão se sobrescrevem)
        let shot = if every < 0.1 { (t * 100.0).round() as u32 } else { (t * 10.0).round() as u32 };
        for s in 0..2 {
            // com resolução interna/FXAA grava a imagem em alta resolução (o nome leva o tamanho real)
            if let Some(sh) = gpu::screen_hires(s) {
                let hi = &sh.real;
                let _ = std::fs::write(out.join(format!("t{shot:05}_s{s}_{}x{}.rgba", hi.w, hi.h)), &hi.rgba);
                // interpolação: os intermediários desse quadro (entre o anterior e ele), à parte, para conferência visual
                // (`YW_SHOT_MIDS=1`; são várias imagens grandes por captura)
                if let Some(g) = sh.group.as_ref().filter(|_| std::env::var_os("YW_SHOT_MIDS").is_some()) {
                    let d = out.join("mids");
                    let _ = std::fs::create_dir_all(&d);
                    for (i, m) in g.mids.iter().enumerate() {
                        let _ = std::fs::write(d.join(format!("t{shot:05}_s{s}_m{}de{}_{}x{}.rgba", i + 1, g.steps, m.w, m.h)), &m.rgba);
                    }
                }
            } else if let Some((w, h, px)) = gpu::screen_rgba(s) {
                let _ = std::fs::write(out.join(format!("t{shot:05}_s{s}_{w}x{h}.rgba")), px);
            }
        }
    }
    if memmap::enabled() {
        eprintln!("[mem] fim\n{}", memmap::report());
    }
    eprintln!("[hle] threads:\n{}", kernel::threads_report());
    hooks::log_resumo();
    if crate::render::backend() == crate::render::Backend::Vulkan {
        crate::render::vulkan::save_pipeline_cache();
    }
    let vs = vs_counts();
    if vs.0 + vs.1 > 0 {
        let st = crate::render::vulkan::stats();
        eprintln!(
            "[hle] vulkan: anéis de upload (tamanho · maior lote · maior ocupação, MB): {}",
            crate::render::vulkan::arena_peaks()
                .iter()
                .map(|(n, s, l, t)| format!("{n} {:.0}·{:.1}·{:.1}", *s as f64 / 1048576.0, *l as f64 / 1048576.0, *t as f64 / 1048576.0))
                .collect::<Vec<_>>()
                .join(" · ")
        );
        eprintln!(
            "[hle] vulkan: draws de vértice na GPU {} — AOT {} ({:.1}%) · ubershader {} · pipelines {} (compilação dos AOT {:.0} ms)",
            vs.0 + vs.1,
            vs.0,
            vs.0 as f64 * 100.0 / (vs.0 + vs.1) as f64,
            vs.1,
            st[4],
            st[10] as f64 / 1000.0
        );
        // travadas por pipeline e a pré-compilação do boot (lista de pipelines, `render::vulkan` stats[20..28])
        eprintln!(
            "[hle] vulkan: compilações síncronas no jogo {} ({:.0} ms, maior travada {:.0} ms) · pré-compiladas no boot {} de {} ({:.0} ms de compilação, {:.1} s de parede; {} usadas)",
            st[20],
            st[21] as f64 / 1000.0,
            st[22] as f64 / 1000.0,
            st[23],
            st[26],
            st[24] as f64 / 1000.0,
            st[27] as f64 / 1e6,
            st[25]
        );
    }
    let g = gpu::GPU.lock().unwrap();
    eprintln!(
        "[hle] {:.1}s: VBlanks {} · listas PICA {} (draws {}) · preenchimentos {} · transferências {} · framebuffers {:?}",
        t0.elapsed().as_secs_f64(),
        VBLANKS.load(Ordering::Relaxed),
        g.cmdlists,
        crate::pica::DRAWS.load(Ordering::Relaxed),
        g.fills,
        g.transfers,
        g.fb
    );
}
