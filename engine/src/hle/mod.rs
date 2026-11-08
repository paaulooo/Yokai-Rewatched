//! HLE do 3DS para o código recompilado: memória, kernel, serviços e o laço de VBlank.
//!
//! Velocidade travada na normal: VBlank a ≈59,83 Hz (LCD do 3DS) e quadros de áudio a 32728/160 Hz; o jogo se
//! cadencia sozinho esperando esses eventos. Sem desbloqueio de FPS por enquanto (pedido do Paulo).

pub mod gpu;
pub mod input;
pub mod kernel;
pub mod mem;
pub mod menu;
pub mod present;
pub mod script;
pub mod services;
pub mod settings;
pub mod window;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Taxa de quadros do LCD do 3DS: 268111856 / (413 · 2 · 4 · 1355) ≈ 59,8261 Hz [3dbrew "LCD"].
pub const VBLANK_HZ: f64 = 59.8261;
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

/// Mostra falhas de acesso do código guest (endereço guest, leitura/escrita) antes do processo cair.
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

/// Carrega o código, prepara a memória e começa a thread principal do jogo (entrada 0x100000).
pub fn boot() {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
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
                clock_wait(t0, n, VBLANK_HZ * speed());
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
        run_headless(secs);
        std::process::exit(if crash_message().is_some() { 1 } else { 0 });
    }
    // configurações (renderizador, idioma, volume) valem desde o início: lidas antes do boot
    let cfg = window::Config::load();
    boot();
    window::run(cfg);
    if let Some(m) = crash_message() {
        eprintln!("[hle] o jogo parou: {m}
[hle] threads:
{}", kernel::threads_report());
    }
    std::process::exit(0);
}

/// Roda sem janela por `seconds` e grava as telas em `work/recomp_frames/` (.rgba + tamanho no nome).
pub fn run_headless(seconds: f64) {
    boot();
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
    let mut next_shot = every;
    // a cada 5 s do console: quadros que o jogo apresentou por segundo (trocas da tela de cima) — a meta é 30
    let (mut next_rate, mut last_swaps) = (5.0, 0u64);
    let mut last_prof = [0u64; 6];
    // vigia: sem quadro novo por 3 s do console → mostra a thread em execução algumas vezes
    let (mut last_swap_count, mut last_swap_t, mut stall_dumps) = (0u64, 0.0f64, 0u32);
    let mut last_host = Instant::now();
    let mut last_idle = 0u64;
    while t0.elapsed().as_secs_f64() < seconds {
        std::thread::sleep(Duration::from_millis(20));
        if let Some(m) = crash_message() {
            eprintln!("[hle] parou: {m}");
            break;
        }
        let t = VBLANKS.load(Ordering::Relaxed) as f64 / VBLANK_HZ;
        let sw = services::gsp::SWAPS[0].load(Ordering::Relaxed);
        if sw != last_swap_count {
            (last_swap_count, last_swap_t, stall_dumps) = (sw, t, 0);
        } else if t - last_swap_t > 3.0 + stall_dumps as f64 * 0.5 && stall_dumps < 12 {
            stall_dumps += 1;
            eprintln!("[vigia] {t:.1}s sem quadro novo: {}", kernel::running_snapshot());
            if stall_dumps == 12 {
                eprintln!("[vigia] threads:
{}", kernel::threads_report());
            }
        }
        if t >= next_rate {
            let s = services::gsp::SWAPS[0].load(Ordering::Relaxed);
            // fração do tempo do host (em %) gasta em: lista PICA, shader de vértice, vulkan::draw, devolução de RT,
            // DisplayTransfer, MemoryFill
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
            last_host = Instant::now();
            eprintln!(
                "[hle] {t:.0}s: {:.1} quadros/s do jogo · % do host: ocioso {idle:.0} · lista PICA {} (shader {} · vk draw {}) · sincronismo GPU {} · transfer {} · fill {} · resto (código do jogo, HLE) {:.0}",
                (s - last_swaps) as f64 / 5.0,
                p[0], p[1], p[2], p[3], p[4], p[5],
                (100.0 - idle - p[0].parse::<f64>().unwrap_or(0.0) - p[3].parse::<f64>().unwrap_or(0.0) - p[4].parse::<f64>().unwrap_or(0.0) - p[5].parse::<f64>().unwrap_or(0.0)).max(0.0)
            );
            (next_rate, last_swaps) = (next_rate + 5.0, s);
        }
        if t < next_shot {
            continue;
        }
        next_shot += every;
        let shot = (t * 10.0).round() as u32;
        for s in 0..2 {
            if let Some((w, h, px)) = gpu::screen_rgba(s) {
                let _ = std::fs::write(out.join(format!("t{shot:05}_s{s}_{w}x{h}.rgba")), px);
            }
        }
    }
    eprintln!("[hle] threads:\n{}", kernel::threads_report());
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
