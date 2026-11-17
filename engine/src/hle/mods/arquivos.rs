//! Substituição de arquivos do `yw1_a.fa` por ganchos nas funções do jogo que abrem um arquivo pelo caminho.
//!
//! Assinaturas (lidas em `data/ghidra/code.bin.c` e conferidas com ganchos de log: `tools/mod_acessos.py`):
//! - `FUN_0047359c(ger, caminho, u32 *buf, u32 *tam, u8 *achou, heap)` → bool: abre o arquivo inteiro. `ger` =
//!   gerenciador (0x5fe028); `caminho` sem prefixo ("data/menu/win_en.xa"); `achou` e `heap` vêm na pilha
//!   ([sp], [sp+4]). Procura nos arquivos montados (`FUN_001db8c0`, que põe "rom:/" na frente e resolve por CRC-32);
//!   se não achou e `*achou == 0`, cai no arquivo solto (`FUN_0047397c`). O buffer vem de
//!   `FUN_0030f4e0(heap, (tam + 4) & ~3, 2)` (alinhamento 1 << 2 = 4) e o resto até o fim é zerado
//!   (`FUN_002fbaf0`); quem pediu libera pelo mesmo heap (`FUN_0048efc0(heap, buf)`).
//! - `FUN_0047365c`: mesma assinatura, com descompressão Level-5 (`FUN_001dbaa8` → `FUN_0031cd60`/`FUN_00167574`):
//!   aloca `(tam_descomprimido + 0xb) & ~3` no mesmo `heap` e devolve o tamanho DESCOMPRIMIDO. O arquivo do mod é
//!   entregue como está (o mod já vem sem compressão).
//! - `FUN_00473728(ger, caminho)` → 1 se o arquivo existe (nos montados ou solto).
//!
//! O gancho `substituir` aloca com o alocador do PRÓPRIO jogo (chamando `FUN_0030f4e0` na mesma thread guest, ver
//! [`chamar_jogo`]), com o mesmo heap, tamanho e alinhamento que a original usaria, copia o arquivo, zera a sobra e
//! preenche `*buf`, `*tam`, `*achou = 1` e o retorno como a original. Sem mod para o caminho: chama a original.

use super::indice::Indice;
use crate::hle::hooks::{self, Original, Tipo};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use yw1_rt::Cpu;

/// Abre um arquivo inteiro pelo caminho.
pub const ABRIR: u32 = 0x47359c;
/// Abre e descomprime.
pub const ABRIR_DESCOMPRIMIR: u32 = 0x47365c;
/// "Existe?"
pub const EXISTE: u32 = 0x473728;
/// `FUN_0030f4e0(heap, tamanho, log2 do alinhamento)` → ponteiro ou 0: alocador por descritor de heap.
pub const ALOCAR: u32 = 0x30f4e0;

/// lr de retorno das chamadas que o motor faz ao jogo: o epílogo gerado volta quando o lr salvo é igual a este
/// (`if t == ret { return; }`); nenhum código do jogo fica perto daqui.
const RET_MOTOR: u32 = 0xFFFF_FFF0;

struct Fonte {
    /// índice dos mods de arquivos (montado no boot)
    indice: Indice,
    /// nome (pasta) de cada mod do índice
    nomes: Vec<String>,
}

static FONTE: RwLock<Option<Fonte>> = RwLock::new(None);
/// arquivos dados pela API dos mods de código (`substituir_arquivo`): vencem os de pasta
static MEMORIA: Mutex<Option<HashMap<String, (String, Arc<Vec<u8>>)>>> = Mutex::new(None);
static INSTALADO: AtomicBool = AtomicBool::new(false);
/// trocas feitas (para o resumo)
pub static TROCAS: AtomicU64 = AtomicU64::new(0);
/// linhas de log por caminho (as primeiras de cada um)
static VISTOS: Mutex<Option<HashMap<String, u32>>> = Mutex::new(None);

/// Guarda o índice dos mods de arquivos e liga os ganchos se houver algo para trocar.
pub fn instalar(indice: Indice, nomes: Vec<String>) {
    let vazio = indice.is_empty();
    *FONTE.write().unwrap_or_else(|e| e.into_inner()) = Some(Fonte { indice, nomes });
    if !vazio {
        ligar_ganchos();
    }
}

/// Arquivo vindo de um mod de código (vence os de pasta). Liga os ganchos se ainda não estavam.
pub fn substituir_em_memoria(caminho: &str, dados: Vec<u8>, origem: &str) -> bool {
    let Some(n) = super::indice::normalizar(caminho) else { return false };
    MEMORIA
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(n, (origem.to_string(), Arc::new(dados)));
    ligar_ganchos();
    true
}

/// Registra os ganchos de arquivo (uma vez).
fn ligar_ganchos() {
    if INSTALADO.swap(true, Ordering::SeqCst) {
        return;
    }
    let r = [
        hooks::registrar(ABRIR, Tipo::substituir(|c, o| abrir(c, o, false)), "mods: abrir arquivo (FUN_0047359c)", 0, false),
        hooks::registrar(
            ABRIR_DESCOMPRIMIR,
            Tipo::substituir(|c, o| abrir(c, o, true)),
            "mods: abrir e descomprimir (FUN_0047365c)",
            0,
            false,
        ),
        hooks::registrar(EXISTE, Tipo::substituir(existe), "mods: arquivo existe? (FUN_00473728)", 0, false),
    ];
    for e in r.into_iter().filter_map(Result::err) {
        eprintln!("[mods] {e}");
    }
}

/// Bytes do mod para o caminho pedido pelo jogo: (dados, nome do mod). Lido do disco a cada abertura (o jogo libera
/// o buffer dele; nada fica preso no motor).
fn procurar(caminho: &str) -> Option<(Arc<Vec<u8>>, String)> {
    let n = super::indice::normalizar(caminho)?;
    if let Some((o, d)) = MEMORIA.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(&n)) {
        return Some((d.clone(), o.clone()));
    }
    let g = FONTE.read().unwrap_or_else(|e| e.into_inner());
    let f = g.as_ref()?;
    let (i, p) = f.indice.mapa.get(&n)?;
    let nome = f.nomes.get(*i).cloned().unwrap_or_default();
    match std::fs::read(p) {
        Ok(d) => Some((Arc::new(d), nome)),
        Err(e) => {
            eprintln!("[mods] {}: não consegui ler {} ({e}); fica o arquivo do jogo", nome, p.display());
            None
        }
    }
}

fn existe_no_mod(caminho: &str) -> bool {
    let Some(n) = super::indice::normalizar(caminho) else { return false };
    if MEMORIA.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|m| m.contains_key(&n)) {
        return true;
    }
    FONTE.read().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|f| f.indice.mapa.contains_key(&n))
}

/// Toda a faixa [a, a + n) está mapeada?
pub fn mapeado(a: u32, n: u32) -> bool {
    if n == 0 {
        return crate::hle::mem::is_mapped(a);
    }
    let Some(fim) = a.checked_add(n - 1) else { return false };
    let mut p = a & !0xfff;
    loop {
        if !crate::hle::mem::is_mapped(p) {
            return false;
        }
        if p >= fim & !0xfff {
            return true;
        }
        p += 0x1000;
    }
}

/// Texto C na memória do jogo (até `max` bytes; `None` se sai da memória mapeada ou passa do máximo).
pub fn ler_cstr(mut a: u32, max: usize) -> Option<String> {
    let mut v = Vec::new();
    while v.len() < max {
        if !crate::hle::mem::is_mapped(a) {
            return None;
        }
        let b = yw1_rt::r8(a) as u8;
        if b == 0 {
            return Some(String::from_utf8_lossy(&v).into_owned());
        }
        v.push(b);
        a = a.wrapping_add(1);
    }
    None
}

/// Chama a função do jogo `va` a partir de um gancho, na mesma thread guest: argumentos em r0–r3 e o resto na pilha
/// (AAPCS: pilha alinhada a 8 na chamada), lr = [`RET_MOTOR`]. Devolve r0 | r1 << 32 e restaura TODOS os
/// registradores do `c` (o gancho continua de onde estava). A pilha usada fica abaixo do sp atual (livre na entrada
/// de uma função). A função pode trocar de thread (preempção) e voltar: o `Cpu` é desta thread.
pub fn chamar_jogo(c: &mut Cpu, va: u32, args: &[u32]) -> u64 {
    let salvo = c.clone();
    let extra = args.len().saturating_sub(4) as u32;
    let sp = salvo.r[13].wrapping_sub(4 * extra) & !7;
    for (i, a) in args.iter().skip(4).enumerate() {
        yw1_rt::w32(sp + 4 * i as u32, *a);
    }
    for i in 0..4 {
        c.r[i] = args.get(i).copied().unwrap_or(0);
    }
    c.r[13] = sp;
    c.r[14] = RET_MOTOR;
    yw1_rt::call(c, va);
    let ret = c.r[0] as u64 | (c.r[1] as u64) << 32;
    *c = salvo;
    ret
}

fn log_troca(caminho: &str, nome: &str, n: usize, buf: u32, descomprime: bool) {
    let mut g = VISTOS.lock().unwrap_or_else(|e| e.into_inner());
    let k = g.get_or_insert_with(HashMap::new).entry(caminho.to_string()).or_insert(0);
    *k += 1;
    if *k <= 3 {
        let q = crate::hle::VBLANKS.load(Ordering::Relaxed);
        eprintln!(
            "[mods] {caminho} ← mod \"{nome}\" ({n} bytes em {buf:#x}{}) q={q}{}",
            if descomprime { ", pedido com descompressão" } else { "" },
            if *k == 3 { " (próximas aberturas deste arquivo sem log)" } else { "" }
        );
    }
}

/// Substituta de `FUN_0047359c` / `FUN_0047365c`.
fn abrir(c: &mut Cpu, o: &Original, descomprime: bool) {
    let Some(caminho) = ler_cstr(c.r[1], 512) else { return o.chamar(c) };
    let Some((dados, nome)) = procurar(&caminho) else { return o.chamar(c) };
    let (p_buf, p_tam, sp) = (c.r[2], c.r[3], c.r[13]);
    if !mapeado(p_buf, 4) || !mapeado(p_tam, 4) || !mapeado(sp, 8) {
        eprintln!("[mods] {caminho}: ponteiros de saída fora da memória ({p_buf:#x}, {p_tam:#x}); fica o arquivo do jogo");
        return o.chamar(c);
    }
    let (p_achou, heap) = (yw1_rt::r32(sp), yw1_rt::r32(sp + 4));
    if !mapeado(p_achou, 1) || !mapeado(heap, 12) {
        eprintln!("[mods] {caminho}: argumentos da pilha inesperados (achou {p_achou:#x}, heap {heap:#x}); fica o arquivo do jogo");
        return o.chamar(c);
    }
    let n = dados.len() as u32;
    // mesmo tamanho que a original pediria: (n + 4) & ~3 na leitura direta, (n + 0xb) & ~3 na que descomprime
    let cap = if descomprime { (n + 0xb) & !3 } else { (n + 4) & !3 };
    yw1_rt::w32(p_buf, 0);
    yw1_rt::w32(p_tam, 0);
    let buf = chamar_jogo(c, ALOCAR, &[heap, cap, 2]) as u32;
    // como a original depois de achar o arquivo no .fa: `*achou = 1` mesmo se a alocação falhar
    yw1_rt::w8(p_achou, 1);
    if buf == 0 || !mapeado(buf, cap) {
        eprintln!(
            "[mods] {caminho} ← mod \"{nome}\": o heap do jogo ({heap:#x}, tipo {}) não deu {cap} bytes; o jogo vê a falha como veria a original",
            yw1_rt::r8(heap)
        );
        c.r[0] = 0;
        return;
    }
    yw1_rt::write_bytes(buf, &dados);
    for a in buf + n..buf + cap {
        yw1_rt::w8(a, 0);
    }
    yw1_rt::w32(p_buf, buf);
    yw1_rt::w32(p_tam, n);
    c.r[0] = 1;
    TROCAS.fetch_add(1, Ordering::Relaxed);
    log_troca(&caminho, &nome, dados.len(), buf, descomprime);
}

/// Substituta de `FUN_00473728`.
fn existe(c: &mut Cpu, o: &Original) {
    match ler_cstr(c.r[1], 512) {
        Some(p) if existe_no_mod(&p) => c.r[0] = 1,
        _ => o.chamar(c),
    }
}
