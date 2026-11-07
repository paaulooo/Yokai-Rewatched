//! HLE do 3DS para o código recompilado: memória, kernel, serviços e o laço de VBlank.
//!
//! Velocidade travada na normal: VBlank a ≈59,83 Hz (LCD do 3DS) e quadros de áudio a 32728/160 Hz; o jogo se
//! cadencia sozinho esperando esses eventos. Sem desbloqueio de FPS por enquanto (pedido do Paulo).

pub mod gpu;
pub mod kernel;
pub mod mem;
pub mod services;
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
static STARTED: AtomicBool = AtomicBool::new(false);
pub static VBLANKS: AtomicU64 = AtomicU64::new(0);

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
                let due = t0 + Duration::from_secs_f64(n as f64 / VBLANK_HZ);
                if let Some(d) = due.checked_duration_since(Instant::now()) {
                    std::thread::sleep(d);
                }
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
                let due = t0 + Duration::from_secs_f64(n as f64 / AUDIO_HZ);
                if let Some(d) = due.checked_duration_since(Instant::now()) {
                    std::thread::sleep(d);
                }
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
    boot();
    window::run();
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
    let t0 = Instant::now();
    let out = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../work/recomp_frames");
    let _ = std::fs::create_dir_all(&out);
    let mut shot = 0;
    while t0.elapsed().as_secs_f64() < seconds {
        std::thread::sleep(Duration::from_millis(500));
        if let Some(m) = crash_message() {
            eprintln!("[hle] parou: {m}");
            break;
        }
        shot += 1;
        for s in 0..2 {
            if let Some((w, h, px)) = gpu::screen_rgba(s) {
                let _ = std::fs::write(out.join(format!("t{shot:03}_s{s}_{w}x{h}.rgba")), px);
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
