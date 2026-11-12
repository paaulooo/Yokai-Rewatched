//! Pasta de logs de cada execução (`work/logs/NNNN/`, fora do git), para depurar sem caçar mensagens no console.
//!
//! Tudo que o jogo escreve em stderr (`eprintln!`) passa por um "tee": continua indo para o console de sempre e
//! também é gravado em arquivos da pasta da execução:
//!
//! | arquivo     | conteúdo                                                                          |
//! |-------------|-----------------------------------------------------------------------------------|
//! | `game.log`  | TUDO, na ordem                                                                    |
//! | `perf.log`  | linha de desempenho a cada 5 s (quadros/s, % do host por parte)                   |
//! | `gfx.log`   | `[vulkan]`, `[render]`, `[pica]`, `[gpu]`                                         |
//! | `dsp.log`   | `[dsp]` (estatísticas do áudio, ruidosas: não vão para o console)                 |
//! | `input.log` | `[input]` (roteiro/entrada ao vivo)                                               |
//! | `stall.log` | `[vigia]` e o relatório completo de travamento (`logs::stall_report`)             |
//! | `meta.txt`  | modo, argumentos, variáveis `YW_*`, hora de início                                |
//!
//! Pasta: `YW_LOG_DIR` se definida; senão `work/logs/NNNN` (número sequencial) ao lado do projeto, e o caminho fica
//! em `work/logs/latest.txt`. `YW_LOG=0` desliga tudo (volta ao stderr puro). `YW_LOG_CONSOLE_ALL=1` mostra também
//! as linhas `[dsp]` no console (por padrão só vão para `dsp.log`).

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

static DIR: OnceLock<PathBuf> = OnceLock::new();
static FILES: Mutex<Vec<(String, File)>> = Mutex::new(Vec::new());

/// Pasta da execução (se o log em arquivos está ligado).
pub fn dir() -> Option<&'static Path> {
    DIR.get().map(|p| p.as_path())
}

fn base_dir() -> PathBuf {
    let m = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    m.parent().map_or(m.clone(), |p| p.to_path_buf()).join("work").join("logs")
}

/// Cria a pasta da execução, grava `meta.txt` e passa a copiar o stderr para ela. Chamar uma vez, no começo.
pub fn init(mode: &str) {
    if std::env::var("YW_LOG").is_ok_and(|v| v == "0") || DIR.get().is_some() {
        return;
    }
    let dir = match std::env::var_os("YW_LOG_DIR") {
        Some(d) => PathBuf::from(d),
        None => {
            let base = base_dir();
            let _ = std::fs::create_dir_all(&base);
            // próximo número livre (0001, 0002…); `latest.txt` aponta para a pasta desta execução
            let n = std::fs::read_dir(&base)
                .map(|it| {
                    it.filter_map(|e| e.ok()?.file_name().to_str()?.parse::<u32>().ok()).max().unwrap_or(0) + 1
                })
                .unwrap_or(1);
            base.join(format!("{n:04}"))
        }
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let _ = DIR.set(dir.clone());
    if let Some(parent) = dir.parent() {
        let _ = std::fs::write(parent.join("latest.txt"), dir.display().to_string());
    }
    let mut meta = format!(
        "modo: {mode}\nexecutável: {:?}\nargumentos: {:?}\niniciado (s desde 1970, UTC): {}\n\nvariáveis YW_*:\n",
        std::env::current_exe().ok(),
        std::env::args().collect::<Vec<_>>(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
    );
    let mut vars: Vec<_> = std::env::vars().filter(|(k, _)| k.starts_with("YW_")).collect();
    vars.sort();
    for (k, v) in vars {
        let v = if v.len() > 300 { format!("{}… ({} bytes)", &v[..300], v.len()) } else { v };
        meta.push_str(&format!("  {k}={v}\n"));
    }
    let _ = std::fs::write(dir.join("meta.txt"), meta);
    tee_stderr();
}

/// Anexa `text` ao arquivo `name` da pasta de logs (cria na primeira vez). Sem pasta de logs, não faz nada.
pub fn append(name: &str, text: &str) {
    let Some(dir) = dir() else { return };
    let mut files = FILES.lock().unwrap_or_else(|e| e.into_inner());
    if !files.iter().any(|(n, _)| n == name) {
        let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join(name)) else { return };
        files.push((name.to_string(), f));
    }
    if let Some((_, f)) = files.iter_mut().find(|(n, _)| n == name) {
        let _ = f.write_all(text.as_bytes());
    }
}

/// Arquivo da categoria de uma linha de log (além do `game.log`, que recebe tudo).
fn category(line: &str) -> Option<&'static str> {
    if line.starts_with("[dsp]") {
        Some("dsp.log")
    } else if line.starts_with("[input]") {
        Some("input.log")
    } else if line.starts_with("[vigia]") || line.starts_with("[stall]") {
        Some("stall.log")
    } else if line.starts_with("[vulkan]")
        || line.starts_with("[render]")
        || line.starts_with("[pica]")
        || line.starts_with("[gpu]")
    {
        Some("gfx.log")
    } else if line.starts_with("[hle]") && line.contains("quadros/s") {
        Some("perf.log")
    } else {
        None
    }
}

fn console_hides(line: &str) -> bool {
    line.starts_with("[dsp]") && !std::env::var("YW_LOG_CONSOLE_ALL").is_ok_and(|v| v != "0")
}

/// Processa uma linha vinda do stderr: arquivos da pasta + console.
fn route(line: &str, console: &mut Option<File>) {
    let mut t = String::with_capacity(line.len() + 1);
    t.push_str(line);
    t.push('\n');
    append("game.log", &t);
    if let Some(c) = category(line) {
        append(c, &t);
    }
    if !console_hides(line) {
        if let Some(c) = console {
            let _ = c.write_all(t.as_bytes());
        }
    }
}

#[cfg(windows)]
fn tee_stderr() {
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use std::{ffi::c_void, mem::ManuallyDrop};
    extern "system" {
        fn GetStdHandle(n: u32) -> *mut c_void;
        fn SetStdHandle(n: u32, h: *mut c_void) -> i32;
        fn CreatePipe(r: *mut *mut c_void, w: *mut *mut c_void, attr: *mut c_void, size: u32) -> i32;
    }
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    unsafe {
        let orig = GetStdHandle(STD_ERROR_HANDLE);
        let (mut r, mut w) = (std::ptr::null_mut::<c_void>(), std::ptr::null_mut::<c_void>());
        if CreatePipe(&mut r, &mut w, std::ptr::null_mut(), 1 << 16) == 0 {
            return;
        }
        // console original (pode não existir: janela sem console → só os arquivos)
        let console = if orig.is_null() || orig as isize == -1 {
            None
        } else {
            Some(File::from_raw_handle(orig))
        };
        let reader = File::from_raw_handle(r);
        if SetStdHandle(STD_ERROR_HANDLE, w) == 0 {
            let _ = ManuallyDrop::new(reader);
            return;
        }
        let _ = reader.as_raw_handle();
        std::thread::Builder::new()
            .name("log-tee".into())
            .spawn(move || {
                let mut console = console;
                let mut rd = BufReader::new(reader);
                let mut buf = Vec::new();
                loop {
                    buf.clear();
                    match rd.read_until(b'\n', &mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            let s = String::from_utf8_lossy(&buf);
                            route(s.trim_end_matches(['\r', '\n']), &mut console);
                        }
                    }
                }
            })
            .ok();
    }
}

#[cfg(not(windows))]
fn tee_stderr() {}

/// Dá tempo à thread do tee de gravar o que sobrou antes de o processo terminar.
pub fn flush() {
    if DIR.get().is_some() {
        eprintln!("[hle] fim do log");
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
}
