//! Ganchos no código recompilado do jogo: base para etiquetas de draw, mods de arquivo/código e widescreen.
//!
//! **Na entrada de qualquer função** (ligados em tempo de execução, sem regerar nada): o cabeçalho que o
//! `yw1-recomp` gera em toda função (`recomp/src/main.rs`, `emit_func`) é
//!
//! ```text
//! c.func = CHAVE;
//! if pc == ENTRADA {
//!     if YW1_HOOKED[IDX] != 0 && yw1_hook_entry(c, IDX, ret) { return; }   // gancho executou a função
//! } else if pc == ENTRADA | HOOK_ORIG {
//!     pc = ENTRADA;                                                        // "chamar a original": pula o gancho
//! }
//! rt::poll(c);  …corpo…
//! ```
//!
//! O byte `YW1_HOOKED[IDX]` (crate `dispatch`) é ligado aqui ao registrar e desligado ao remover o último gancho da
//! função; desligado, o custo é uma comparação e uma leitura relaxada por chamada. O gancho roda antes do `rt::poll`:
//! nenhuma troca de thread guest entre ele e a função. Entradas extras (saltos para o meio da função, pc ≠ entrada)
//! não passam pelo gancho.
//!
//! Tipos (vários "antes"/"depois" por função, em ordem crescente de `prioridade`, empate = ordem de registro):
//! - [`Tipo::Antes`] `Fn(&mut Cpu)`: roda antes do corpo e pode mudar os argumentos (r0–r3, pilha);
//! - [`Tipo::Substituir`] `Fn(&mut Cpu, &Original)`: ocupa o lugar da função (exclusivo: um por função);
//!   [`Original::chamar`] executa a função original (as chamadas recursivas dela voltam a passar pelos ganchos);
//! - [`Tipo::Depois`] `Fn(&mut Cpu, &Entrada)`: roda quando a função (ou a substituta) volta; os registradores já
//!   têm o retorno e [`Entrada`] tem a cópia de r0–r3, sp e lr da entrada (depois dos "antes").
//!
//! **Em instrução** (`recomp/hooks.toml`): o gerador põe `yw1_hook_at(c, va)` antes da instrução (inclusive
//! condicional — dispara mesmo se a condição for falsa); [`registrar_em`] só aceita esses endereços.
//!
//! Caminho quente: o registro fica atrás de um `Mutex`, mas callbacks e a função original rodam sem ele (clone de
//! `Arc` da lista e solta): o código do jogo pode preemptar, trocar de thread e reentrar aqui.
//!
//! `YW_HOOKS=0` registra só os ganchos essenciais (modo mono…); `YW_HOOKS_LOG=1` imprime, no fim da rodada, quantas
//! vezes cada gancho rodou ([`resumo`], no `game.log`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use yw1_rt::{Cpu, GuestFn, HookTables, HOOK_ORIG};

extern "Rust" {
    /// Tabelas geradas no crate `dispatch` (falsas nos testes da biblioteca, abaixo).
    fn yw1_hook_tables() -> HookTables;
}

fn tabelas() -> HookTables {
    // SAFETY: função gerada (ou falsa nos testes) que só devolve referências a estáticos
    unsafe { yw1_hook_tables() }
}

/// Callback de cada tipo de gancho de entrada (o tipo leva o callback porque cada um tem uma assinatura).
pub enum Tipo {
    Antes(Box<dyn Fn(&mut Cpu) + Send + Sync>),
    Substituir(Box<dyn Fn(&mut Cpu, &Original) + Send + Sync>),
    Depois(Box<dyn Fn(&mut Cpu, &Entrada) + Send + Sync>),
}

impl Tipo {
    pub fn antes(f: impl Fn(&mut Cpu) + Send + Sync + 'static) -> Tipo {
        Tipo::Antes(Box::new(f))
    }
    pub fn substituir(f: impl Fn(&mut Cpu, &Original) + Send + Sync + 'static) -> Tipo {
        Tipo::Substituir(Box::new(f))
    }
    pub fn depois(f: impl Fn(&mut Cpu, &Entrada) + Send + Sync + 'static) -> Tipo {
        Tipo::Depois(Box::new(f))
    }
}

/// Identificador devolvido por [`registrar`]/[`registrar_em`] (para [`remover`] e [`contagem`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct IdGancho(u64);

/// A função original, para um gancho [`Tipo::Substituir`].
pub struct Original {
    f: GuestFn,
    pc: u32,
    ret: u32,
    lr: u32,
}

impl Original {
    /// Executa a função original com os registradores atuais (argumentos em r0–r3/pilha, como o callback deixou).
    /// O lr volta ao da entrada (o retorno da original é reconhecido por ele). Só esta chamada pula o gancho.
    pub fn chamar(&self, c: &mut Cpu) {
        c.r[14] = self.lr;
        // SAFETY: função recompilada da tabela gerada, chamada com o próprio pc de entrada (+ HOOK_ORIG)
        unsafe { (self.f)(c, self.pc | HOOK_ORIG, self.ret) }
    }

    /// Endereço da função (sem o bit Thumb).
    pub fn va(&self) -> u32 {
        self.pc
    }
}

/// Estado da entrada da função, para um gancho [`Tipo::Depois`] (cópia feita depois dos ganchos "antes").
#[derive(Clone, Copy, Debug)]
pub struct Entrada {
    pub r: [u32; 4],
    pub sp: u32,
    pub lr: u32,
}

enum Cb {
    Antes(Box<dyn Fn(&mut Cpu) + Send + Sync>),
    Substituir(Box<dyn Fn(&mut Cpu, &Original) + Send + Sync>),
    Depois(Box<dyn Fn(&mut Cpu, &Entrada) + Send + Sync>),
    Em(Box<dyn Fn(&mut Cpu) + Send + Sync>),
}

struct Gancho {
    id: u64,
    va: u32,
    nome: &'static str,
    prioridade: i32,
    essencial: bool,
    n: AtomicU64,
    ativo: AtomicBool,
    cb: Cb,
}

impl Gancho {
    fn tipo(&self) -> &'static str {
        match self.cb {
            Cb::Antes(_) => "antes",
            Cb::Substituir(_) => "substituir",
            Cb::Depois(_) => "depois",
            Cb::Em(_) => "em instrução",
        }
    }
}

/// Ganchos de uma função, já ordenados (trocada inteira a cada registro/remoção; quem está rodando segura a antiga).
#[derive(Default)]
struct Lista {
    antes: Vec<Arc<Gancho>>,
    substituir: Option<Arc<Gancho>>,
    depois: Vec<Arc<Gancho>>,
}

#[derive(Default)]
struct Registro {
    prox_id: u64,
    /// todos os ganchos já registrados (inclusive removidos), para o resumo
    todos: Vec<Arc<Gancho>>,
    /// índice da função → ganchos de entrada
    entrada: HashMap<u32, Arc<Lista>>,
    /// va da instrução → ganchos em instrução (ordenados)
    em: HashMap<u32, Arc<Vec<Arc<Gancho>>>>,
    /// registros ignorados por `YW_HOOKS=0`
    ignorados: usize,
}

fn reg() -> MutexGuard<'static, Registro> {
    static R: OnceLock<Mutex<Registro>> = OnceLock::new();
    // um callback nunca roda com o lock, então envenenamento só viria de um bug aqui: segue com o estado que houver
    R.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// Quantos ganchos em instrução estão registrados (atalho do `yw1_hook_at` quando não há nenhum).
static EM_ATIVOS: AtomicUsize = AtomicUsize::new(0);

/// `YW_HOOKS=0`: só os essenciais.
fn so_essenciais() -> bool {
    static S: OnceLock<bool> = OnceLock::new();
    *S.get_or_init(|| std::env::var("YW_HOOKS").is_ok_and(|v| v == "0"))
}

/// Índice da função cuja entrada é `va` (com ou sem o bit Thumb; com ele, a função tem de ser Thumb).
fn indice(t: &HookTables, va: u32) -> Result<usize, String> {
    let a = va & !1;
    let i = t.funcs.partition_point(|&(k, _)| k & !1 < a);
    match t.funcs.get(i) {
        Some(&(k, _)) if k & !1 == a => {
            if va & 1 == 1 && k & 1 == 0 {
                Err(format!("{va:#x}: a função em {a:#x} é ARM, não Thumb"))
            } else {
                Ok(i)
            }
        }
        _ => Err(format!(
            "{va:#x} não é entrada de nenhuma função recompilada (para o meio de uma função, use um gancho em instrução: recomp/hooks.toml + registrar_em)"
        )),
    }
}

fn reconstruir(r: &Registro, idx: u32) -> Option<Arc<Lista>> {
    let mut l = Lista::default();
    let mut doidx: Vec<&Arc<Gancho>> = r
        .todos
        .iter()
        .filter(|g| g.ativo.load(Ordering::Relaxed) && !matches!(g.cb, Cb::Em(_)))
        .filter(|g| indice(&tabelas(), g.va).ok() == Some(idx as usize))
        .collect();
    // estável: empate fica na ordem de registro
    doidx.sort_by_key(|g| g.prioridade);
    for g in doidx {
        match g.cb {
            Cb::Antes(_) => l.antes.push(g.clone()),
            Cb::Substituir(_) => l.substituir = Some(g.clone()),
            Cb::Depois(_) => l.depois.push(g.clone()),
            Cb::Em(_) => {}
        }
    }
    (!l.antes.is_empty() || l.substituir.is_some() || !l.depois.is_empty()).then(|| Arc::new(l))
}

fn reconstruir_em(r: &Registro, va: u32) -> Option<Arc<Vec<Arc<Gancho>>>> {
    let mut v: Vec<Arc<Gancho>> = r
        .todos
        .iter()
        .filter(|g| g.ativo.load(Ordering::Relaxed) && matches!(g.cb, Cb::Em(_)) && g.va == va)
        .cloned()
        .collect();
    v.sort_by_key(|g| g.prioridade);
    (!v.is_empty()).then(|| Arc::new(v))
}

/// Registra um gancho na ENTRADA da função `va` (endereço da função; o bit Thumb é opcional). `prioridade`: menor
/// roda primeiro. `essencial`: vale mesmo com `YW_HOOKS=0` (sem ele, com `YW_HOOKS=0` o registro é aceito e ignorado).
/// Erro se `va` não é entrada de função ou se já há um [`Tipo::Substituir`] nela.
pub fn registrar(va: u32, tipo: Tipo, nome: &'static str, prioridade: i32, essencial: bool) -> Result<IdGancho, String> {
    let t = tabelas();
    let idx = indice(&t, va).map_err(|e| format!("gancho `{nome}`: {e}"))?;
    let key = t.funcs[idx].0;
    let cb = match tipo {
        Tipo::Antes(f) => Cb::Antes(f),
        Tipo::Substituir(f) => Cb::Substituir(f),
        Tipo::Depois(f) => Cb::Depois(f),
    };
    let mut r = reg();
    r.prox_id += 1;
    let id = r.prox_id;
    if !essencial && so_essenciais() {
        r.ignorados += 1;
        return Ok(IdGancho(id));
    }
    if matches!(cb, Cb::Substituir(_)) {
        if let Some(s) = r.entrada.get(&(idx as u32)).and_then(|l| l.substituir.clone()) {
            return Err(format!("gancho `{nome}`: a função {key:#x} já é substituída por `{}`", s.nome));
        }
    }
    r.todos.push(Arc::new(Gancho {
        id,
        va: key,
        nome,
        prioridade,
        essencial,
        n: AtomicU64::new(0),
        ativo: AtomicBool::new(true),
        cb,
    }));
    if let Some(l) = reconstruir(&r, idx as u32) {
        r.entrada.insert(idx as u32, l);
    }
    // liga depois de a lista estar no lugar
    t.hooked[idx].store(1, Ordering::SeqCst);
    Ok(IdGancho(id))
}

/// Registra um gancho ANTES da instrução `va` (só endereços listados em `recomp/hooks.toml`, que o gerador
/// instrumentou). Dispara mesmo se a instrução for condicional e não executar.
pub fn registrar_em(
    va: u32,
    nome: &'static str,
    prioridade: i32,
    essencial: bool,
    cb: impl Fn(&mut Cpu) + Send + Sync + 'static,
) -> Result<IdGancho, String> {
    if tabelas().at.binary_search(&va).is_err() {
        return Err(format!(
            "gancho `{nome}`: a instrução {va:#x} não está em recomp/hooks.toml (adicione lá e regere o código)"
        ));
    }
    let mut r = reg();
    r.prox_id += 1;
    let id = r.prox_id;
    if !essencial && so_essenciais() {
        r.ignorados += 1;
        return Ok(IdGancho(id));
    }
    r.todos.push(Arc::new(Gancho {
        id,
        va,
        nome,
        prioridade,
        essencial,
        n: AtomicU64::new(0),
        ativo: AtomicBool::new(true),
        cb: Cb::Em(Box::new(cb)),
    }));
    if let Some(l) = reconstruir_em(&r, va) {
        r.em.insert(va, l);
    }
    EM_ATIVOS.fetch_add(1, Ordering::SeqCst);
    Ok(IdGancho(id))
}

/// Remove um gancho (devolve false se o id não existe ou já foi removido). O último gancho de entrada de uma função
/// desliga o byte dela. Uma chamada já em andamento termina com a lista antiga.
pub fn remover(id: IdGancho) -> bool {
    let mut r = reg();
    let Some(g) = r.todos.iter().find(|g| g.id == id.0 && g.ativo.load(Ordering::Relaxed)).cloned() else {
        return false;
    };
    g.ativo.store(false, Ordering::Relaxed);
    if let Cb::Em(_) = g.cb {
        match reconstruir_em(&r, g.va) {
            Some(l) => r.em.insert(g.va, l),
            None => r.em.remove(&g.va),
        };
        EM_ATIVOS.fetch_sub(1, Ordering::SeqCst);
        return true;
    }
    let t = tabelas();
    let Ok(idx) = indice(&t, g.va) else { return true };
    match reconstruir(&r, idx as u32) {
        Some(l) => {
            r.entrada.insert(idx as u32, l);
        }
        None => {
            // desliga antes de tirar a lista (quem já leu o byte ligado acha a lista vazia e segue o corpo)
            t.hooked[idx].store(0, Ordering::SeqCst);
            r.entrada.remove(&(idx as u32));
        }
    }
    true
}

/// Quantas vezes o gancho rodou.
pub fn contagem(id: IdGancho) -> u64 {
    reg().todos.iter().find(|g| g.id == id.0).map_or(0, |g| g.n.load(Ordering::Relaxed))
}

/// Chamado pelo cabeçalho de uma função com o byte ligado (pc = entrada). Devolve true se já executou a função
/// (substituta, ou original + "depois"); false = o cabeçalho segue para o corpo (só havia ganchos "antes").
#[no_mangle]
pub fn yw1_hook_entry(c: &mut Cpu, idx: u32, ret: u32) -> bool {
    let Some(l) = reg().entrada.get(&idx).cloned() else {
        return false;
    };
    for g in &l.antes {
        g.n.fetch_add(1, Ordering::Relaxed);
        if let Cb::Antes(f) = &g.cb {
            f(c);
        }
    }
    if l.substituir.is_none() && l.depois.is_empty() {
        return false;
    }
    let (key, f) = tabelas().funcs[idx as usize];
    let orig = Original { f, pc: key & !1, ret, lr: c.r[14] };
    let entrada = Entrada { r: [c.r[0], c.r[1], c.r[2], c.r[3]], sp: c.r[13], lr: c.r[14] };
    match &l.substituir {
        Some(g) => {
            g.n.fetch_add(1, Ordering::Relaxed);
            if let Cb::Substituir(f) = &g.cb {
                f(c, &orig);
            }
        }
        None => orig.chamar(c),
    }
    for g in &l.depois {
        g.n.fetch_add(1, Ordering::Relaxed);
        if let Cb::Depois(f) = &g.cb {
            f(c, &entrada);
        }
    }
    true
}

/// Chamado pelo código gerado antes de uma instrução listada em `recomp/hooks.toml`.
#[no_mangle]
pub fn yw1_hook_at(c: &mut Cpu, va: u32) {
    if EM_ATIVOS.load(Ordering::Relaxed) == 0 {
        return;
    }
    let Some(l) = reg().em.get(&va).cloned() else {
        return;
    };
    for g in l.iter() {
        g.n.fetch_add(1, Ordering::Relaxed);
        if let Cb::Em(f) = &g.cb {
            f(c);
        }
    }
}

/// Ganchos registrados e quantas vezes cada um rodou (para o `game.log` no fim da rodada, com `YW_HOOKS_LOG=1`).
pub fn resumo() -> String {
    let r = reg();
    let mut s = format!(
        "[ganchos] {} registrados ({} ativos){}",
        r.todos.len(),
        r.todos.iter().filter(|g| g.ativo.load(Ordering::Relaxed)).count(),
        if r.ignorados > 0 { format!(" · {} ignorados por YW_HOOKS=0", r.ignorados) } else { String::new() }
    );
    for g in &r.todos {
        s.push_str(&format!(
            "\n  {:#010x} {:<12} {:<32} prioridade {:>4}{}{} · {} chamadas",
            g.va,
            g.tipo(),
            g.nome,
            g.prioridade,
            if g.essencial { " · essencial" } else { "" },
            if g.ativo.load(Ordering::Relaxed) { "" } else { " · removido" },
            g.n.load(Ordering::Relaxed)
        ));
    }
    s
}

/// `YW_HOOKS_LOG=1`: imprime o [`resumo`] (fim da rodada, `hle::main`/`run_headless`).
pub fn log_resumo() {
    if std::env::var("YW_HOOKS_LOG").is_ok_and(|v| v != "0") {
        eprintln!("{}", resumo());
    }
}

#[cfg(test)]
mod tests {
    //! Funções "guest" de mentira em Rust com EXATAMENTE o cabeçalho que o gerador emite, e tabelas falsas no lugar
    //! das do crate `dispatch`.
    use super::*;
    use std::sync::atomic::{AtomicU32, AtomicU8};
    use std::sync::Mutex;
    use std::sync::atomic::Ordering::Relaxed;

    const ZERO: AtomicU8 = AtomicU8::new(0);
    static HOOKED: [AtomicU8; 3] = [ZERO; 3];
    static FUNCS: [(u32, GuestFn); 3] = [(0x1000, f_00001000), (0x2001, f_00002001), (0x3000, f_00003000)];
    static AT: [u32; 1] = [0x3004];

    #[no_mangle]
    fn yw1_hook_tables() -> HookTables {
        HookTables { hooked: &HOOKED, funcs: &FUNCS, at: &AT }
    }

    /// Os testes daqui mexem no mesmo registro global: um de cada vez.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Quantas vezes o CORPO de cada função rodou.
    static CORPO: [AtomicU32; 3] = [const { AtomicU32::new(0) }; 3];

    /// ARM, índice 0: r0 = r0 + r1 (soma).
    fn f_00001000(c: &mut Cpu, mut pc: u32, ret: u32) {
        c.func = 0x1000;
        if pc == 0x1000 {
            if HOOKED[0].load(Relaxed) != 0 && unsafe { yw1_hook_entry_ref(c, 0, ret) } {
                return;
            }
        } else if pc == 0x80001000 {
            pc = 0x1000;
        }
        assert_eq!(pc, 0x1000);
        CORPO[0].fetch_add(1, Relaxed);
        c.r[0] = c.r[0].wrapping_add(c.r[1]);
    }

    /// Thumb (chave 0x2001, pc 0x2000), índice 1: fatorial recursivo de r0 (chama a si mesma pelo cabeçalho).
    fn f_00002001(c: &mut Cpu, mut pc: u32, ret: u32) {
        c.func = 0x2001;
        if pc == 0x2000 {
            if HOOKED[1].load(Relaxed) != 0 && unsafe { yw1_hook_entry_ref(c, 1, ret) } {
                return;
            }
        } else if pc == 0x80002000 {
            pc = 0x2000;
        }
        assert_eq!(pc, 0x2000);
        CORPO[1].fetch_add(1, Relaxed);
        let n = c.r[0];
        if n <= 1 {
            c.r[0] = 1;
            return;
        }
        c.r[0] = n - 1;
        c.r[14] = 0x2011;
        f_00002001(c, 0x2000, 0x2011);
        c.r[0] *= n;
    }

    /// ARM, índice 2: gancho em instrução em 0x3004 (como o gerador emite), depois r0 = r0 * 2.
    fn f_00003000(c: &mut Cpu, mut pc: u32, ret: u32) {
        c.func = 0x3000;
        if pc == 0x3000 {
            if HOOKED[2].load(Relaxed) != 0 && unsafe { yw1_hook_entry_ref(c, 2, ret) } {
                return;
            }
        } else if pc == 0x80003000 {
            pc = 0x3000;
        }
        assert_eq!(pc, 0x3000);
        CORPO[2].fetch_add(1, Relaxed);
        unsafe { yw1_hook_at_ref(c, 0x3004) };
        c.r[0] = c.r[0].wrapping_mul(2);
    }

    // o código gerado chama por símbolo (`extern "Rust"`); aqui, direto
    unsafe fn yw1_hook_entry_ref(c: &mut Cpu, idx: u32, ret: u32) -> bool {
        yw1_hook_entry(c, idx, ret)
    }
    unsafe fn yw1_hook_at_ref(c: &mut Cpu, va: u32) {
        yw1_hook_at(c, va)
    }

    fn cpu(r0: u32, r1: u32) -> Cpu {
        let mut c = Cpu::default();
        c.r[0] = r0;
        c.r[1] = r1;
        c.r[13] = 0x0ff0_0000;
        c.r[14] = 0xdead;
        c
    }

    #[test]
    fn hooks_antes_muda_argumento() {
        let _s = serial();
        let id = registrar(0x1000, Tipo::antes(|c| c.r[1] = 100), "antes", 0, false).unwrap();
        assert_eq!(HOOKED[0].load(Relaxed), 1);
        let mut c = cpu(2, 3);
        f_00001000(&mut c, 0x1000, 0xdead);
        assert_eq!(c.r[0], 102);
        assert_eq!(contagem(id), 1);
        assert!(remover(id));
    }

    #[test]
    fn hooks_antes_em_ordem_de_prioridade() {
        let _s = serial();
        // r1 = (r1 + 1) * 10 só se o de prioridade -5 rodar antes do de 7
        let a = registrar(0x1000, Tipo::antes(|c| c.r[1] *= 10), "segundo", 7, false).unwrap();
        let b = registrar(0x1000, Tipo::antes(|c| c.r[1] += 1), "primeiro", -5, false).unwrap();
        let mut c = cpu(0, 3);
        f_00001000(&mut c, 0x1000, 0xdead);
        assert_eq!(c.r[0], 40);
        remover(a);
        remover(b);
    }

    #[test]
    fn hooks_substituir_sem_original() {
        let _s = serial();
        let antes = CORPO[0].load(Relaxed);
        let id = registrar(0x1000, Tipo::substituir(|c, _o| c.r[0] = 99), "troca", 0, false).unwrap();
        let mut c = cpu(2, 3);
        f_00001000(&mut c, 0x1000, 0xdead);
        assert_eq!(c.r[0], 99);
        assert_eq!(CORPO[0].load(Relaxed), antes, "o corpo não pode rodar");
        remover(id);
    }

    #[test]
    fn hooks_substituir_chamando_original() {
        let _s = serial();
        let id = registrar(
            0x1000,
            Tipo::substituir(|c, o| {
                assert_eq!(o.va(), 0x1000);
                c.r[1] += 1;
                c.r[14] = 0x1234; // a original volta a ver o lr da entrada
                o.chamar(c);
                assert_eq!(c.r[14], 0xdead);
                c.r[0] += 1000;
            }),
            "envolve",
            0,
            false,
        )
        .unwrap();
        let mut c = cpu(2, 3);
        f_00001000(&mut c, 0x1000, 0xdead);
        assert_eq!(c.r[0], 1006);
        // chamada "original" vinda de fora (pc | HOOK_ORIG) pula o gancho
        let mut c = cpu(2, 3);
        f_00001000(&mut c, 0x1000 | HOOK_ORIG, 0xdead);
        assert_eq!(c.r[0], 5);
        assert_eq!(contagem(id), 1);
        remover(id);
    }

    #[test]
    fn hooks_depois_ve_retorno_e_entrada() {
        let _s = serial();
        let visto = Arc::new(Mutex::new(None));
        let v2 = visto.clone();
        let antes = registrar(0x1000, Tipo::antes(|c| c.r[0] = 10), "antes", 0, false).unwrap();
        let depois = registrar(
            0x1000,
            Tipo::depois(move |c, e| {
                *v2.lock().unwrap() = Some((c.r[0], *e));
                c.r[0] += 1;
            }),
            "depois",
            0,
            false,
        )
        .unwrap();
        let mut c = cpu(2, 3);
        f_00001000(&mut c, 0x1000, 0xdead);
        assert_eq!(c.r[0], 14);
        let (ret, e) = visto.lock().unwrap().unwrap();
        assert_eq!(ret, 13);
        // a entrada é a cópia depois dos "antes"
        assert_eq!(e.r, [10, 3, 0, 0]);
        assert_eq!((e.sp, e.lr), (0x0ff0_0000, 0xdead));
        remover(antes);
        remover(depois);
    }

    #[test]
    fn hooks_recursao_passa_pelo_gancho_em_cada_nivel() {
        let _s = serial();
        let niveis = Arc::new(AtomicU32::new(0));
        let n2 = niveis.clone();
        let id = registrar(0x2001, Tipo::antes(move |_| { n2.fetch_add(1, Relaxed); }), "nivel", 0, false).unwrap();
        let mut c = cpu(5, 0);
        f_00002001(&mut c, 0x2000, 0xdead);
        assert_eq!(c.r[0], 120);
        assert_eq!(niveis.load(Relaxed), 5);
        remover(id);
        // substituir chamando a original: só a chamada de fora pula o gancho, as recursivas passam por ele
        let id = registrar(0x2000, Tipo::substituir(|c, o| o.chamar(c)), "envolve", 0, false).unwrap();
        let mut c = cpu(4, 0);
        f_00002001(&mut c, 0x2000, 0xdead);
        assert_eq!(c.r[0], 24);
        assert_eq!(contagem(id), 4);
        remover(id);
    }

    #[test]
    fn hooks_remover_desliga() {
        let _s = serial();
        let a = registrar(0x1000, Tipo::antes(|c| c.r[0] = 0), "a", 0, false).unwrap();
        let b = registrar(0x1000, Tipo::substituir(|c, _| c.r[0] = 7), "b", 0, false).unwrap();
        assert!(remover(b));
        assert_eq!(HOOKED[0].load(Relaxed), 1, "ainda há o \"antes\"");
        assert!(remover(a));
        assert!(!remover(a), "remover duas vezes");
        assert_eq!(HOOKED[0].load(Relaxed), 0);
        let mut c = cpu(2, 3);
        f_00001000(&mut c, 0x1000, 0xdead);
        assert_eq!(c.r[0], 5);
        // um Substituir novo é aceito depois de remover o anterior
        let b = registrar(0x1000, Tipo::substituir(|_, _| {}), "b2", 0, false).unwrap();
        remover(b);
    }

    #[test]
    fn hooks_substituir_duplicado_da_erro() {
        let _s = serial();
        let a = registrar(0x1000, Tipo::substituir(|_, _| {}), "primeiro", 0, false).unwrap();
        let e = registrar(0x1000, Tipo::substituir(|_, _| {}), "segundo", 0, false).unwrap_err();
        assert!(e.contains("primeiro"), "{e}");
        remover(a);
    }

    #[test]
    fn hooks_enderecos_invalidos() {
        let _s = serial();
        // meio de função, função inexistente, bit Thumb numa função ARM
        assert!(registrar(0x1004, Tipo::antes(|_| {}), "x", 0, false).is_err());
        assert!(registrar(0x9000, Tipo::antes(|_| {}), "x", 0, false).is_err());
        assert!(registrar(0x1001, Tipo::antes(|_| {}), "x", 0, false).is_err());
        // instrução não instrumentada por hooks.toml
        assert!(registrar_em(0x3008, "x", 0, false, |_| {}).is_err());
    }

    #[test]
    fn hooks_em_instrucao() {
        let _s = serial();
        let id = registrar_em(0x3004, "dobra", 0, false, |c| c.r[0] += 1).unwrap();
        let mut c = cpu(3, 0);
        f_00003000(&mut c, 0x3000, 0xdead);
        assert_eq!(c.r[0], 8);
        assert_eq!(contagem(id), 1);
        assert!(remover(id));
        let mut c = cpu(3, 0);
        f_00003000(&mut c, 0x3000, 0xdead);
        assert_eq!(c.r[0], 6);
        assert!(resumo().contains("dobra"));
    }
}
