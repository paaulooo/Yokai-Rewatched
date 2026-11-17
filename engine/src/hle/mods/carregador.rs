//! Carregador dos mods de CÓDIGO: DLL com ABI C (`modapi/`), carregada por `LoadLibraryExW`/`GetProcAddress`
//! (FFI direto no kernel32, como o resto do motor) e a tabela [`Yw1Api`] que o motor entrega a ela.
//!
//! Ordem: `yw1_mod_api_versao()` é lido antes de tudo e conferido com [`yw1_modapi::versao_compativel`]; só então
//! `yw1_mod_init(api, YW1_API_VERSAO)`. `false` (ou pânico contido pelo invólucro) = mod recusado: os ganchos que ele
//! registrou durante o init são removidos. A DLL nunca é descarregada depois de aceita (os callbacks dela ficam
//! registrados), e uma recusada também não (um gancho em andamento poderia ainda estar nela) [decisão conservadora].

use crate::hle::hooks::{self, Entrada, IdGancho, Original, Tipo};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr};
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use yw1_modapi::{Yw1Api, Yw1Callback, Yw1Ctx, Yw1ModApiVersao, Yw1ModInit, GANCHO_ANTES, GANCHO_DEPOIS, GANCHO_SUBSTITUIR, YW1_API_VERSAO};
use yw1_rt::Cpu;

thread_local! {
    /// mod "falando" agora nesta thread (init ou callback): prefixo do log e dono dos ganchos registrados
    static MOD_ATUAL: RefCell<String> = const { RefCell::new(String::new()) };
}

fn com_mod<R>(nome: &str, f: impl FnOnce() -> R) -> R {
    let antes = MOD_ATUAL.with(|m| std::mem::replace(&mut *m.borrow_mut(), nome.to_string()));
    let r = f();
    MOD_ATUAL.with(|m| *m.borrow_mut() = antes);
    r
}

fn mod_atual() -> String {
    MOD_ATUAL.with(|m| m.borrow().clone())
}

/// Ganchos registrados pelos mods: id da API → (id do motor, mod dono).
#[derive(Default)]
struct Ganchos {
    prox: u64,
    mapa: HashMap<u64, (IdGancho, String)>,
}

fn ganchos() -> std::sync::MutexGuard<'static, Ganchos> {
    static G: OnceLock<Mutex<Ganchos>> = OnceLock::new();
    G.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// Contexto do motor por trás do `*mut Yw1Ctx` (vive na pilha do gancho durante o callback).
struct Ctx<'a> {
    c: &'a mut Cpu,
    orig: Option<&'a Original>,
    entrada: Option<Entrada>,
    va: u32,
}

/// # Safety
/// `p` é o `*mut Yw1Ctx` que o motor passou ao callback (um `Ctx` vivo).
unsafe fn ctx<'a>(p: *mut Yw1Ctx) -> Option<&'a mut Ctx<'a>> {
    // SAFETY: garantido pelo chamador; nulo = mod chamou fora de um gancho
    unsafe { (p as *mut Ctx<'a>).as_mut() }
}

fn chamar_cb(cb: Yw1Callback, user: usize, c: &mut Cpu, orig: Option<&Original>, entrada: Option<Entrada>, va: u32, dono: &str) {
    let mut x = Ctx { c, orig, entrada, va };
    com_mod(dono, || cb(&mut x as *mut Ctx as *mut Yw1Ctx, user as *mut c_void));
}

extern "C" fn api_registrar(va: u32, tipo: u32, prioridade: i32, nome: *const c_char, cb: Yw1Callback, user: *mut c_void) -> u64 {
    let dono = mod_atual();
    let nome = if nome.is_null() {
        String::from("?")
    } else {
        // SAFETY: texto C do mod (terminado em zero, pela ABI)
        unsafe { CStr::from_ptr(nome) }.to_string_lossy().into_owned()
    };
    let nome_g: &'static str = Box::leak(format!("mod {dono}: {nome}").into_boxed_str());
    let user = user as usize;
    let d = dono.clone();
    let t = match tipo {
        GANCHO_ANTES => Tipo::antes(move |c| {
            let va = c.func & !1;
            chamar_cb(cb, user, c, None, None, va, &d)
        }),
        GANCHO_SUBSTITUIR => Tipo::substituir(move |c, o| chamar_cb(cb, user, c, Some(o), None, o.va(), &d)),
        GANCHO_DEPOIS => Tipo::depois(move |c, e| {
            let va = c.func & !1;
            chamar_cb(cb, user, c, None, Some(*e), va, &d)
        }),
        _ => {
            eprintln!("[mod {dono}] registrar_gancho: tipo {tipo} inválido");
            return 0;
        }
    };
    match hooks::registrar(va, t, nome_g, prioridade, false) {
        Ok(id) => {
            let mut g = ganchos();
            g.prox += 1;
            let n = g.prox;
            g.mapa.insert(n, (id, dono));
            n
        }
        Err(e) => {
            eprintln!("[mod {dono}] {e}");
            0
        }
    }
}

extern "C" fn api_remover(id: u64) -> bool {
    let Some((g, _)) = ganchos().mapa.remove(&id) else { return false };
    hooks::remover(g)
}

extern "C" fn api_chamar_original(p: *mut Yw1Ctx) -> bool {
    // SAFETY: contexto do callback
    let Some(x) = (unsafe { ctx(p) }) else { return false };
    match x.orig {
        Some(o) => {
            o.chamar(x.c);
            true
        }
        None => false,
    }
}

extern "C" fn api_ler_reg(p: *mut Yw1Ctx, i: u32) -> u32 {
    // SAFETY: contexto do callback
    let Some(x) = (unsafe { ctx(p) }) else { return 0 };
    match i {
        0..=14 => x.c.r[i as usize],
        15 => x.va,
        _ => 0,
    }
}

extern "C" fn api_escrever_reg(p: *mut Yw1Ctx, i: u32, v: u32) {
    // SAFETY: contexto do callback
    let Some(x) = (unsafe { ctx(p) }) else { return };
    if i <= 14 {
        x.c.r[i as usize] = v;
    }
}

extern "C" fn api_ler_entrada(p: *mut Yw1Ctx, i: u32) -> u32 {
    // SAFETY: contexto do callback
    let Some(x) = (unsafe { ctx(p) }) else { return 0 };
    match (x.entrada, i) {
        (Some(e), 0..=3) => e.r[i as usize],
        (Some(e), 4) => e.sp,
        (Some(e), 5) => e.lr,
        (None, 0..=3) => x.c.r[i as usize],
        (None, 4) => x.c.r[13],
        (None, 5) => x.c.r[14],
        _ => 0,
    }
}

extern "C" fn api_ler_mem(va: u32, dst: *mut u8, len: usize) -> bool {
    if dst.is_null() || len > u32::MAX as usize || !super::arquivos::mapeado(va, len as u32) {
        return false;
    }
    // SAFETY: `dst` tem `len` bytes (ABI); a origem está mapeada
    yw1_rt::read_bytes(va, unsafe { std::slice::from_raw_parts_mut(dst, len) });
    true
}

extern "C" fn api_escrever_mem(va: u32, src: *const u8, len: usize) -> bool {
    if src.is_null() || len > u32::MAX as usize || !super::arquivos::mapeado(va, len as u32) {
        return false;
    }
    // SAFETY: `src` tem `len` bytes (ABI); o destino está mapeado
    yw1_rt::write_bytes(va, unsafe { std::slice::from_raw_parts(src, len) });
    true
}

extern "C" fn api_chamar_funcao(p: *mut Yw1Ctx, va: u32, args: *const u32, n: usize) -> u64 {
    // SAFETY: contexto do callback
    let Some(x) = (unsafe { ctx(p) }) else {
        eprintln!("[mod {}] chamar_funcao fora de um gancho (ignorado)", mod_atual());
        return 0;
    };
    let a: &[u32] = if args.is_null() || n == 0 {
        &[]
    } else {
        // SAFETY: `args` tem `n` palavras (ABI)
        unsafe { std::slice::from_raw_parts(args, n.min(64)) }
    };
    super::arquivos::chamar_jogo(x.c, va, a)
}

extern "C" fn api_substituir_arquivo(caminho: *const c_char, dados: *const u8, len: usize) -> bool {
    if caminho.is_null() || (dados.is_null() && len > 0) {
        return false;
    }
    // SAFETY: texto C e `len` bytes do mod (ABI)
    let c = unsafe { CStr::from_ptr(caminho) }.to_string_lossy().into_owned();
    let d = if len == 0 { Vec::new() } else { unsafe { std::slice::from_raw_parts(dados, len) }.to_vec() };
    let dono = mod_atual();
    let ok = super::arquivos::substituir_em_memoria(&c, d, &format!("{dono} (código)"));
    eprintln!("[mod {dono}] substituir_arquivo({c}, {len} bytes){}", if ok { "" } else { ": caminho inválido" });
    ok
}

extern "C" fn api_log(nivel: u32, msg: *const c_char) {
    if msg.is_null() {
        return;
    }
    // SAFETY: texto C do mod
    let m = unsafe { CStr::from_ptr(msg) }.to_string_lossy();
    let n = match nivel {
        yw1_modapi::LOG_AVISO => " aviso:",
        yw1_modapi::LOG_ERRO => " ERRO:",
        _ => "",
    };
    eprintln!("[mod {}]{n} {m}", mod_atual());
}

extern "C" fn api_vblank() -> u64 {
    crate::hle::VBLANKS.load(std::sync::atomic::Ordering::Relaxed)
}

/// A tabela entregue a todos os mods (estática: vale até o fim do processo).
pub fn api() -> &'static Yw1Api {
    static API: Yw1Api = Yw1Api {
        versao: YW1_API_VERSAO,
        tamanho: std::mem::size_of::<Yw1Api>() as u32,
        registrar_gancho: api_registrar,
        remover_gancho: api_remover,
        chamar_original: api_chamar_original,
        ler_reg: api_ler_reg,
        escrever_reg: api_escrever_reg,
        ler_entrada: api_ler_entrada,
        ler_mem: api_ler_mem,
        escrever_mem: api_escrever_mem,
        chamar_funcao: api_chamar_funcao,
        substituir_arquivo: api_substituir_arquivo,
        log: api_log,
        vblank: api_vblank,
    };
    &API
}

// ------------------------------------------------------------------------------------------------ DLL (Windows)

#[cfg(windows)]
mod so {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryExW(nome: *const u16, arquivo: *mut c_void, flags: u32) -> *mut c_void;
        fn GetProcAddress(m: *mut c_void, nome: *const u8) -> *mut c_void;
        fn GetLastError() -> u32;
    }
    /// procura as dependências da DLL na pasta dela e nas pastas padrão do sistema (não na pasta atual)
    const LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR: u32 = 0x100;
    const LOAD_LIBRARY_SEARCH_DEFAULT_DIRS: u32 = 0x1000;

    pub fn abrir(p: &std::path::Path) -> Result<*mut c_void, String> {
        use std::os::windows::ffi::OsStrExt;
        let w: Vec<u16> = p.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: caminho UTF-16 terminado em zero
        let h = unsafe { LoadLibraryExW(w.as_ptr(), std::ptr::null_mut(), LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS) };
        if h.is_null() {
            // SAFETY: sem argumentos
            Err(format!("LoadLibraryExW falhou (erro {} do Windows)", unsafe { GetLastError() }))
        } else {
            Ok(h)
        }
    }

    pub fn simbolo(h: *mut c_void, nome: &str) -> *mut c_void {
        let n: Vec<u8> = nome.bytes().chain(Some(0)).collect();
        // SAFETY: módulo carregado; nome ASCII terminado em zero
        unsafe { GetProcAddress(h, n.as_ptr()) }
    }
}

#[cfg(not(windows))]
mod so {
    use std::ffi::c_void;
    pub fn abrir(_p: &std::path::Path) -> Result<*mut c_void, String> {
        Err("mods de código só existem no Windows por enquanto".into())
    }
    pub fn simbolo(_h: *mut c_void, _nome: &str) -> *mut c_void {
        std::ptr::null_mut()
    }
}

/// Carrega a DLL de um mod e chama o init. `Ok(nº de ganchos registrados)` ou o motivo da recusa (nunca pânico).
pub fn carregar(id: &str, dll: &Path) -> Result<usize, String> {
    if !dll.is_file() {
        return Err(format!("DLL não encontrada: {}", dll.display()));
    }
    let h = so::abrir(dll)?;
    let pv = so::simbolo(h, "yw1_mod_api_versao");
    let pi = so::simbolo(h, "yw1_mod_init");
    if pv.is_null() || pi.is_null() {
        return Err("a DLL não exporta yw1_mod_api_versao e yw1_mod_init (não é um mod do yw1)".into());
    }
    // SAFETY: símbolos exportados com as assinaturas da ABI (`modapi`)
    let (versao, init): (Yw1ModApiVersao, Yw1ModInit) = unsafe { (std::mem::transmute(pv), std::mem::transmute(pi)) };
    // SAFETY: função C sem argumentos
    let v = unsafe { versao() };
    yw1_modapi::versao_compativel(v)?;
    let antes: Vec<u64> = ganchos().mapa.keys().copied().collect();
    // SAFETY: tabela estática; o mod confere a versão do motor
    let ok = com_mod(id, || unsafe { init(api(), YW1_API_VERSAO) });
    let novos: Vec<u64> = ganchos().mapa.iter().filter(|(k, (_, d))| d == id && !antes.contains(k)).map(|(k, _)| *k).collect();
    if !ok {
        for k in &novos {
            api_remover(*k);
        }
        return Err(format!("yw1_mod_init devolveu false (API do mod {v}); {} ganchos desfeitos", novos.len()));
    }
    Ok(novos.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mods_dll_ausente_sem_panico() {
        let e = carregar("x", Path::new("Z:/nao/existe/mod.dll")).unwrap_err();
        assert!(e.contains("não encontrada"), "{e}");
    }

    #[test]
    fn mods_arquivo_que_nao_e_dll() {
        let d = std::env::temp_dir().join(format!("yw1_mods_dll_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("falsa.dll");
        std::fs::write(&p, b"isto nao e uma DLL").unwrap();
        let e = carregar("x", &std::path::absolute(&p).unwrap()).unwrap_err();
        assert!(e.contains("LoadLibraryExW") || e.contains("Windows"), "{e}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(windows)]
    #[test]
    fn mods_dll_sem_exportacoes() {
        // uma DLL de verdade do sistema que não é mod
        let p = Path::new("C:/Windows/System32/version.dll");
        if p.is_file() {
            let e = carregar("x", p).unwrap_err();
            assert!(e.contains("não exporta"), "{e}");
        }
    }

    #[test]
    fn mods_versao_errada_recusada() {
        assert!(yw1_modapi::versao_compativel(YW1_API_VERSAO + 1).unwrap_err().contains("atualize"));
        assert!(yw1_modapi::versao_compativel(0).is_err());
    }
}
