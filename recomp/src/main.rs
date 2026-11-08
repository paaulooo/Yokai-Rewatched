//! `yw1-recomp`: recompila estaticamente o `code.bin` (ARM11) do Yo-kai Watch 1 para Rust.
//!
//! Etapas:
//! 1. **Entradas**: funções `FUN_xxxxxxxx` do export do Ghidra + 0x100000 (ponto de entrada).
//! 2. **Descoberta**: percorre cada função seguindo o fluxo (B, BL, tabelas de salto, retornos); alvos de BL viram
//!    funções novas; `B` para o início de outra função = chamada de cauda.
//! 3. **Entradas por endereço**: palavras de `.text` (pools de literais), `.rodata` e `.data` que apontam para uma
//!    instrução já descoberta viram entradas extras (vtables, ponteiros de função, callbacks).
//! 4. **Emissão**: um crate Rust por fatia de funções (`work/recomp/game_NN`), mais o crate `dispatch` com a tabela
//!    endereço → função. As funções se chamam por símbolo (`#[no_mangle]`), então as fatias compilam em paralelo.
//!
//! O código gerado é derivado do jogo do usuário: fica em `work/` e nunca é versionado.
//!
//! Uso: `cargo run --release -- [--code data/ghidra/code.bin] [--ghidra data/ghidra/code.bin.c] [--out work/recomp]
//!       [--crates 24] [--stats]`

mod arm;
mod thumb;

use arm::{translate, Flow, Image, Ins};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const BASE: u32 = 0x0010_0000;
const TEXT_END: u32 = 0x004b_fca0;
const RO_END: u32 = 0x005b_e000;
const RO_START: u32 = 0x004c_0000;
const DATA_END: u32 = 0x005b_e000 + 0x0003_ecd4;

struct Func {
    entry: u32,
    insts: BTreeMap<u32, Ins>,
    leaders: BTreeSet<u32>,
    /// tem algum caminho que retorna (retorno, salto indireto ou tabela)?
    rets: bool,
    /// chamadas de cauda (`B` para outra função)
    tails: Vec<u32>,
    /// endereços de instruções não suportadas
    undef: Vec<u32>,
}

#[derive(Default)]
struct Stats {
    undefined: BTreeMap<u32, u32>,
    /// (chave da função, endereço) de cada instrução não suportada
    undef_sites: Vec<(u32, u32)>,
    tables: usize,
    tail_calls: usize,
    indirect_jumps: usize,
    returns: usize,
}

/// `entry` é a chave da função (endereço | 1 = Thumb). Endereços de instrução/blocos são os reais (pares).
fn explore(
    img: &Image,
    entry: u32,
    seeds: &BTreeSet<u32>,
    noreturn: &BTreeSet<u32>,
    calls: &mut Vec<u32>,
    st: &mut Stats,
) -> Func {
    let mut rets = false;
    let mut tails = Vec::new();
    let mut undef = Vec::new();
    let thumb = entry & 1 == 1;
    let start = entry & !1;
    let mut insts = BTreeMap::new();
    let mut leaders = BTreeSet::new();
    leaders.insert(start);
    let mut work = vec![start];
    while let Some(pc) = work.pop() {
        if insts.contains_key(&pc) {
            continue;
        }
        let i = if thumb {
            thumb::translate(img, pc)
        } else {
            translate(img, pc)
        };
        let len = i.len;
        let cond = i.cond != 14;
        // `add/sub lr, pc, #imm`: endereço de retorno de uma "chamada" manual → bloco a decodificar
        if !thumb {
            if let Some(w) = img.word(pc) {
                let imm = (w & 0xff).rotate_right(((w >> 8) & 0xf) * 2);
                let tgt = match w & 0x0fff_f000 {
                    0x028f_e000 => Some((pc + 8).wrapping_add(imm)),
                    0x024f_e000 => Some((pc + 8).wrapping_sub(imm)),
                    _ => None,
                };
                if let Some(tg) = tgt.filter(|&x| img.key_in_text(x)) {
                    leaders.insert(tg);
                    work.push(tg);
                }
            }
        }
        let mut fall = false;
        match &i.flow {
            Flow::Next | Flow::CallInd => work.push(pc + len),
            Flow::Call(t) => {
                calls.push(*t);
                // depois de chamar uma função que não retorna (assert/abort) vem pool de literais, não código
                // (uma chamada condicional a ela ainda segue adiante)
                if !noreturn.contains(t) || cond {
                    work.push(pc + len);
                }
            }
            Flow::Branch(t) => {
                let t = *t;
                if (seeds.contains(&t) && t != entry) || !img.key_in_text(t) {
                    st.tail_calls += 1;
                    tails.push(t);
                } else {
                    leaders.insert(t & !1);
                    work.push(t & !1);
                }
                fall = cond;
            }
            Flow::Return => {
                st.returns += 1;
                rets = true;
                fall = cond;
            }
            Flow::JumpInd => {
                st.indirect_jumps += 1;
                rets = true;
                fall = cond;
            }
            Flow::Table(ts) => {
                st.tables += 1;
                rets = true;
                for &t in ts {
                    leaders.insert(t);
                    work.push(t);
                }
                fall = cond;
            }
            Flow::Halt => {}
            Flow::Undefined => {
                *st.undefined.entry(img.word(pc).unwrap_or(0)).or_default() += 1;
                st.undef_sites.push((entry, pc));
                undef.push(pc);
            }
        }
        if fall {
            leaders.insert(pc + len);
            work.push(pc + len);
        }
        insts.insert(pc, i);
    }
    Func {
        entry,
        insts,
        leaders,
        rets,
        tails,
        undef,
    }
}

/// Descobre todas as funções a partir das entradas, até não aparecer nenhuma nova.
fn discover(
    img: &Image,
    initial: &BTreeSet<u32>,
    noreturn: &BTreeSet<u32>,
) -> (BTreeMap<u32, Func>, Stats, BTreeSet<u32>) {
    let mut seeds = initial.clone();
    let mut st = Stats::default();
    let mut funcs: BTreeMap<u32, Func> = BTreeMap::new();
    let mut called = BTreeSet::new();
    let mut queue: Vec<u32> = seeds.iter().copied().collect();
    while let Some(e) = queue.pop() {
        if funcs.contains_key(&e) || !img.key_in_text(e) {
            continue;
        }
        let mut calls = Vec::new();
        let f = explore(img, e, &seeds, noreturn, &mut calls, &mut st);
        funcs.insert(e, f);
        for c in calls {
            called.insert(c);
            if img.key_in_text(c) && seeds.insert(c) {
                queue.push(c);
            }
        }
    }
    (funcs, st, called)
}

/// Funções definidas no export do Ghidra (`tipo FUN_xxxxxxxx(…)` numa linha sem `;`).
fn ghidra_functions(path: &Path) -> BTreeSet<u32> {
    let mut out = BTreeSet::new();
    let Ok(src) = std::fs::read_to_string(path) else {
        eprintln!("[recomp] aviso: {} não lido; só a entrada 0x100000", path.display());
        return out;
    };
    for line in src.lines() {
        if line.starts_with(char::is_whitespace) || line.trim_end().ends_with(';') {
            continue;
        }
        let Some(i) = line.find("FUN_") else {
            continue;
        };
        if i > 0 && {
            let b = line.as_bytes()[i - 1];
            b.is_ascii_alphanumeric() || b == b'_'
        } {
            continue;
        }
        let hex = &line[i + 4..];
        if hex.len() > 8 && hex.as_bytes()[8] == b'(' {
            if let Ok(a) = u32::from_str_radix(&hex[..8], 16) {
                out.insert(a);
            }
        }
    }
    out
}

/// Tabelas de construtores estáticos do RVCT (`__cpp_initialize__aeabi_`, em Thumb):
/// `ldr rA, [pc, #i]; add rA, pc; ldr rB, [pc, #j]; add rB, pc; …; ldr r0, [rA]; adds r0, r0, rA; blx r0`.
/// A tabela `[rA, rB)` guarda deslocamentos relativos à própria entrada: alvo = entrada + valor (bit 0 = Thumb).
fn init_tables(img: &Image) -> Vec<u32> {
    let half = |va: u32| -> Option<u32> {
        let w = img.word(va & !3)?;
        Some(if va & 2 != 0 { w >> 16 } else { w & 0xffff })
    };
    // valor de `ldr rX, [pc, #imm8*4]` + `add rX, pc` (pc = endereço do add + 4)
    let pc_rel = |a: u32| -> Option<(u32, u32)> {
        let h1 = half(a)?;
        let h2 = half(a + 2)?;
        if h1 & 0xf800 != 0x4800 {
            return None;
        }
        let r = (h1 >> 8) & 7;
        if h2 != 0x4478 | r {
            return None;
        }
        let lit = img.word(((a + 4) & !3) + (h1 & 0xff) * 4)?;
        Some((r, lit.wrapping_add(a + 2 + 4)))
    };
    let mut out = Vec::new();
    let mut a = BASE;
    while a + 24 < TEXT_END {
        if let (Some((ra, start)), Some((rb, end))) = (pc_rel(a), pc_rel(a + 4)) {
            // procura o `ldr r0, [rA]` + `adds r0, r0, rA` + `blx r0` logo adiante
            let body = (a + 8..a + 24).step_by(2).any(|b| {
                half(b) == Some(0x6800 | ra << 3)
                    && half(b + 2) == Some(0x1800 | ra << 6)
                    && half(b + 4) == Some(0x4780)
            });
            if body && ra != rb && start < end && end - start < 0x10000 && (end - start) % 4 == 0 {
                for e in (start..end).step_by(4) {
                    if let Some(v) = img.word(e) {
                        let t = e.wrapping_add(v);
                        if img.key_in_text(t) {
                            out.push(t);
                        }
                    }
                }
            }
        }
        a += 2;
    }
    out
}

/// Valor do literal carregado pela instrução em `a` (`ldr rX, [pc, #imm]` em ARM ou Thumb), se houver.
fn literal_at(img: &Image, a: u32, thumb: bool) -> Option<u32> {
    if thumb {
        let w = img.word(a & !3)?;
        let h = if a & 2 != 0 { w >> 16 } else { w & 0xffff };
        if h & 0xf800 == 0x4800 {
            let lit = img.const_word(((a + 4) & !3) + (h & 0xff) * 4)?;
            // `ldr rX, [pc, #i]` + `add rX, pc` (endereço independente de posição): valor = literal + pc do add
            // (o add pode vir até 4 instruções depois; o compilador intercala outras)
            let rd = (h >> 8) & 7;
            for k in 1..=4u32 {
                let b = a + 2 * k;
                let w2 = img.word(b & !3)?;
                let h2 = if b & 2 != 0 { w2 >> 16 } else { w2 & 0xffff };
                if h2 == 0x4478 | rd {
                    return Some(lit.wrapping_add(b + 4));
                }
                // parou se rd foi reescrito (formatos com Rd nos bits 2–0 ou 10–8)
                if h2 & 7 == rd && h2 >> 13 != 0b101 && h2 >> 12 != 0b1001 || (h2 >> 11 & 0x1f) >= 4 && (h2 >> 11) <= 7 && (h2 >> 8) & 7 == rd {
                    break;
                }
            }
            return Some(lit);
        }
        // ADR (add rX, pc, #imm8*4): sempre alinhado; em código Thumb o alvo de código é Thumb (o chamador soma 1)
        if h & 0xf800 == 0xa000 {
            let v = ((a + 4) & !3) + (h & 0xff) * 4;
            return Some(if img.key_in_text(v) { v | 1 } else { v });
        }
        return None;
    }
    let w = img.word(a)?;
    // ADR: add/sub rX, pc, #imm (rotacionado); o RVCT põe o bit 0 quando o rótulo é Thumb
    if w >> 28 != 15 && w & 0x0fff_0000 == 0x028f_0000 {
        return Some((a + 8).wrapping_add((w & 0xff).rotate_right(((w >> 8) & 0xf) * 2)));
    }
    if w >> 28 != 15 && w & 0x0fff_0000 == 0x024f_0000 {
        return Some((a + 8).wrapping_sub((w & 0xff).rotate_right(((w >> 8) & 0xf) * 2)));
    }
    // LDR (literal): cond 0101 U001 1111 Rt imm12
    if w & 0x0f7f_0000 == 0x051f_0000 {
        let imm = w & 0xfff;
        let t = if w & (1 << 23) != 0 { a + 8 + imm } else { a + 8 - imm };
        let lit = img.const_word(t)?;
        // `ldr rX, [pc, #i]` + `add rX, pc, rX`: valor = literal + pc do add (+8)
        let rd = (w >> 12) & 15;
        for k in 1..=3u32 {
            let b = a + 4 * k;
            match img.word(b) {
                Some(n) if n & 0x0fff_ffff == 0x008f_0000 | rd << 12 | rd => return Some(lit.wrapping_add(b + 8)),
                // outra instrução escrevendo em rd: para
                Some(n) if (n >> 12) & 15 == rd => break,
                _ => {}
            }
        }
        return Some(lit);
    }
    None
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let code = arg(&args, "--code")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("YW_CODEBIN").map(PathBuf::from))
        .unwrap_or_else(|| root.join("data/ghidra/code.bin"));
    let ghidra = arg(&args, "--ghidra")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("data/ghidra/code.bin.c"));
    let out = arg(&args, "--out")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("YW_RECOMP").map(PathBuf::from))
        .unwrap_or_else(|| root.join("work/recomp"));
    let ncrates: usize = arg(&args, "--crates").and_then(|s| s.parse().ok()).unwrap_or(24);
    let only_stats = args.iter().any(|a| a == "--stats");

    let bytes = std::fs::read(&code).unwrap_or_else(|e| panic!("{}: {e}", code.display()));
    let img = Image {
        bytes,
        base: BASE,
        text_end: TEXT_END,
        ro_end: RO_END,
    };
    let t0 = std::time::Instant::now();

    // 1–2. descoberta, repetida até estabilizar:
    //  - entrada do Ghidra que decodifica lixo como ARM logo no começo, nunca chamada em modo ARM e que decodifica
    //    limpo como Thumb → é Thumb (o Ghidra marcou como ARM);
    //  - função sem nenhum caminho de retorno (só chamadas de cauda para outras assim) → "não retorna"; depois de
    //    chamá-la não se decodifica mais nada.
    let mut seeds = ghidra_functions(&ghidra);
    let ghidra_n = seeds.len();
    seeds.insert(BASE);
    let ctors = init_tables(&img);
    let nctors = ctors.len();
    seeds.extend(ctors);
    let mut noreturn = BTreeSet::new();
    let mut thumb_fixed = 0usize;
    let mut by_pointer = 0usize;
    let mut rounds = 0;
    let (funcs, st) = loop {
        rounds += 1;
        let (funcs, st, called) = discover(&img, &seeds, &noreturn);
        let mut changed = false;
        let fixes: Vec<u32> = funcs
            .values()
            .filter(|f| {
                f.entry & 1 == 0
                    && seeds.contains(&f.entry)
                    && !called.contains(&f.entry)
                    && f.undef.iter().any(|&u| u - f.entry < 0x40)
            })
            .map(|f| f.entry)
            .filter(|&e| {
                let mut c = Vec::new();
                let mut s2 = Stats::default();
                let t = explore(&img, e | 1, &seeds, &noreturn, &mut c, &mut s2);
                t.undef.iter().all(|&u| u - e >= 0x40)
            })
            .collect();
        for e in fixes {
            seeds.remove(&e);
            seeds.insert(e | 1);
            thumb_fixed += 1;
            changed = true;
        }
        let mut nr: BTreeSet<u32> = noreturn.clone();
        loop {
            let before = nr.len();
            for f in funcs.values() {
                if !f.rets && f.undef.is_empty() && f.tails.iter().all(|t| nr.contains(t)) {
                    nr.insert(f.entry);
                }
            }
            if nr.len() == before {
                break;
            }
        }
        if nr != noreturn {
            noreturn = nr;
            changed = true;
        }
        // candidatos por endereço: literais carregados pelo código (`ldr rX, =func`, callbacks, destrutores do
        // atexit) e palavras de .rodata/.data (vtables) que apontam para código ainda sem dono. Só entram se
        // decodificarem limpo (nenhuma instrução não suportada).
        if !changed {
            let mut owned: HashMap<u32, u32> = HashMap::new();
            for f in funcs.values() {
                for &a in f.insts.keys() {
                    owned.entry(a).or_insert(f.entry);
                }
            }
            let mut cands: BTreeSet<u32> = BTreeSet::new();
            for f in funcs.values() {
                for &a in f.insts.keys() {
                    if let Some(v) = literal_at(&img, a, f.entry & 1 == 1) {
                        cands.insert(v);
                    }
                }
            }
            // palavras de dados: .rodata/.data inteiros e, na .text, as que não são instruções executadas
            let mut a = BASE;
            while a + 4 <= DATA_END.min(BASE + img.bytes.len() as u32) {
                if a >= RO_START || !owned.contains_key(&a) {
                    cands.insert(img.word(a).unwrap_or(0));
                }
                a += 4;
            }
            let mut added = 0;
            for v in cands {
                if !img.key_in_text(v) || seeds.contains(&v) || funcs.contains_key(&v) || owned.contains_key(&(v & !1)) {
                    continue;
                }
                let mut c = Vec::new();
                let mut s2 = Stats::default();
                let t = explore(&img, v, &seeds, &noreturn, &mut c, &mut s2);
                // funções de uma instrução: `bx lr` (vazias), `bx r0` (trampolins) ou `b alvo` (veneers)
                let tiny_ret = t.insts.len() == 1
                    && t.insts.values().all(|i| i.cond == 14 && matches!(i.flow, Flow::Return | Flow::JumpInd | Flow::Branch(_)));
                if t.undef.is_empty() && (t.insts.len() >= 2 || tiny_ret) {
                    seeds.insert(v);
                    added += 1;
                }
            }
            if added > 0 {
                by_pointer += added;
                changed = true;
            }
        }
        if !changed || rounds >= 12 {
            break (funcs, st);
        }
    };
    let mut funcs = funcs;
    let from_calls = funcs.len() - ghidra_n.min(funcs.len());

    // 2b. regiões densas: funções ARM com salto computado por processamento de dados (`add pc, rA, rB`,
    // `mov pc, rX`, `movle pc, r4`) — assembly escrito à mão (o decodificador de vídeo Mobiclip). A região inteira é
    // decodificada em linha e toda instrução vira bloco e entrada: qualquer destino calculado cai no lugar.
    let mut dense: Vec<(u32, u32)> = funcs
        .values()
        .filter(|f| f.entry & 1 == 0)
        .filter(|f| {
            f.insts.iter().any(|(&a, i)| {
                i.flow == Flow::JumpInd
                    && img.word(a).is_some_and(|w| w >> 26 & 3 == 0 && (w >> 12) & 15 == 15 && w & 0x0ff0_00f0 != 0x0120_0010)
            })
        })
        .map(|f| (f.entry, f.insts.keys().next_back().map_or(f.entry, |&a| a) + 4))
        .collect();
    dense.sort();
    let mut regions: Vec<(u32, u32)> = Vec::new();
    for (a, b) in dense {
        match regions.last_mut() {
            Some(r) if a <= r.1 + 0x400 => r.1 = r.1.max(b),
            _ => regions.push((a, b)),
        }
    }
    // absorve as funções que começam dentro de cada região (e estende até o fim delas)
    for r in regions.iter_mut() {
        loop {
            let end = funcs
                .range(r.0..r.1)
                .filter(|(k, _)| *k & 1 == 0)
                .filter_map(|(_, f)| f.insts.keys().next_back().map(|&a| a + 4))
                .max()
                .unwrap_or(r.1)
                .max(r.1);
            if end == r.1 {
                break;
            }
            r.1 = end;
        }
    }
    // junta de novo depois de crescer (buracos pequenos entre regiões também são o mesmo bloco de assembly)
    loop {
        let before = regions.len();
        let mut merged: Vec<(u32, u32)> = Vec::new();
        for r in regions.drain(..) {
            match merged.last_mut() {
                Some(m) if r.0 <= m.1 + 0x1000 => m.1 = m.1.max(r.1),
                _ => merged.push(r),
            }
        }
        regions = merged;
        if regions.len() == before {
            break;
        }
    }
    let mut dense_insts = 0usize;
    let mut region_entries: Vec<(u32, u32)> = Vec::new();
    for &(a, b) in &regions {
        let inner: Vec<u32> = funcs.range(a..b).map(|(k, _)| *k).filter(|k| k & 1 == 0).collect();
        for k in inner {
            funcs.remove(&k);
        }
        let mut insts = BTreeMap::new();
        let mut leaders = BTreeSet::new();
        let mut pc = a;
        while pc < b {
            let i = translate(&img, pc);
            leaders.insert(pc);
            region_entries.push((pc, a));
            insts.insert(pc, i);
            pc += 4;
        }
        dense_insts += insts.len();
        funcs.insert(
            a,
            Func {
                entry: a,
                insts,
                leaders,
                rets: true,
                tails: Vec::new(),
                undef: Vec::new(),
            },
        );
    }

    // 3. entradas por endereço (ponteiros para instruções já descobertas)
    let mut owner: HashMap<u32, u32> = HashMap::new();
    for f in funcs.values() {
        for &a in f.insts.keys() {
            owner.entry(a).or_insert(f.entry);
        }
    }
    let mut extra = 0usize;
    let mut extra_entries: BTreeMap<u32, u32> = BTreeMap::new();
    for &(pc, a) in &region_entries {
        if pc != a {
            extra_entries.insert(pc, a);
        }
    }
    let mut ptrs: BTreeSet<u32> = BTreeSet::new();
    // saltos diretos (`b alvo`) que saem de uma região densa: o assembly à mão pula para o meio de outras funções
    // (ex.: 0x269ee8 → 0x266bcc). Na descoberta normal esses blocos eram da função original; decodificada em linha,
    // a região perde-os — o alvo vira entrada extra da função que contém a instrução.
    let mut dense_out: Vec<(u32, u32)> = Vec::new();
    for &(a, b) in &regions {
        for (&pc, i) in &funcs[&a].insts {
            if let Flow::Branch(t) | Flow::Call(t) = i.flow {
                if (t & !1) < a || (t & !1) >= b {
                    dense_out.push((pc, t));
                }
            }
        }
    }
    for &(pc, t) in &dense_out {
        if funcs.contains_key(&t) || extra_entries.contains_key(&t) {
            continue;
        }
        match owner.get(&(t & !1)) {
            Some(&fe) if fe & 1 == t & 1 => {
                extra_entries.insert(t, fe);
                funcs.get_mut(&fe).unwrap().leaders.insert(t & !1);
                extra += 1;
            }
            // fora do .text = dado no meio do assembly lido como instrução (tabelas do decodificador)
            _ if !img.key_in_text(t) => {}
            _ => eprintln!("[aviso] região densa: salto em {pc:#x} para {t:#x}, que não está em nenhuma função"),
        }
    }
    for f in funcs.values() {
        for &a in f.insts.keys() {
            if let Some(v) = literal_at(&img, a, f.entry & 1 == 1) {
                ptrs.insert(v);
            }
        }
    }
    let mut a = BASE;
    while a + 4 <= DATA_END.min(BASE + img.bytes.len() as u32) {
        if a >= RO_START || !owner.contains_key(&a) {
            ptrs.insert(img.word(a).unwrap_or(0));
        }
        a += 4;
    }
    for v in ptrs {
        if !img.key_in_text(v) || funcs.contains_key(&v) || extra_entries.contains_key(&v) {
            continue;
        }
        if let Some(&fe) = owner.get(&(v & !1)) {
            // mesmo modo (bit 0)
            if fe & 1 != v & 1 {
                continue;
            }
            extra_entries.insert(v, fe);
            funcs.get_mut(&fe).unwrap().leaders.insert(v & !1);
            extra += 1;
        }
    }

    let ninst: usize = funcs.values().map(|f| f.insts.len()).sum();
    let nblocks: usize = funcs.values().map(|f| f.leaders.len()).sum();
    let nundef: u32 = st.undefined.values().sum();
    let mut report = String::new();
    writeln!(report, "code.bin: {}", code.display()).unwrap();
    writeln!(
        report,
        "funções: {} (Ghidra {ghidra_n}, +{from_calls} por BL/tabelas/ponteiros) · construtores estáticos: {nctors} · funções achadas por literal/ponteiro: {by_pointer} · entradas extras por ponteiro: {extra}",
        funcs.len()
    )
    .unwrap();
    writeln!(
        report,
        "instruções traduzidas: {ninst} · blocos: {nblocks} · tabelas de salto: {} · chamadas de cauda: {} · saltos indiretos: {} · retornos: {}",
        st.tables, st.tail_calls, st.indirect_jumps, st.returns
    )
    .unwrap();
    writeln!(
        report,
        "não suportadas: {nundef} ({} codificações distintas) · funções que não retornam: {} · entradas do Ghidra corrigidas para Thumb: {thumb_fixed} · rodadas: {rounds}",
        st.undefined.len(),
        noreturn.len()
    )
    .unwrap();
    writeln!(
        report,
        "regiões densas (assembly com saltos computados): {} · {dense_insts} instruções · {:?}",
        regions.len(),
        regions.iter().map(|(a, b)| format!("{a:#x}-{b:#x}")).collect::<Vec<_>>()
    )
    .unwrap();
    let mut top: Vec<_> = st.undefined.iter().collect();
    top.sort_by(|a, b| b.1.cmp(a.1));
    for (w, n) in top.iter().take(40) {
        writeln!(report, "  {w:#010x} × {n}").unwrap();
    }
    for (e, pc) in st.undef_sites.iter().take(60) {
        writeln!(report, "  em {pc:#010x} (função {e:#010x} +{:#x})", pc - (e & !1)).unwrap();
    }
    writeln!(report, "descoberta em {:.1}s", t0.elapsed().as_secs_f64()).unwrap();
    print!("{report}");
    if only_stats {
        return;
    }

    // 4. emissão
    std::fs::create_dir_all(&out).unwrap();
    write_if_changed(out.join("stats.txt"), &report).unwrap();
    // as regiões densas (assembly com milhares de blocos numa função) vão num crate só delas, compilado com menos
    // otimização: o LLVM leva muito tempo nelas em opt-level 2
    let dense_keys: BTreeSet<u32> = regions.iter().map(|r| r.0).collect();
    let list: Vec<&Func> = funcs.values().filter(|f| !dense_keys.contains(&f.entry)).collect();
    let per = ninst.div_ceil(ncrates.max(1));
    let mut slices: Vec<Vec<&Func>> = vec![Vec::new()];
    let mut acc = 0;
    for f in list {
        if acc >= per && slices.len() < ncrates {
            slices.push(Vec::new());
            acc = 0;
        }
        acc += f.insts.len();
        slices.last_mut().unwrap().push(f);
    }
    let dense_slice = slices.len();
    slices.push(funcs.values().filter(|f| dense_keys.contains(&f.entry)).collect());
    let crate_of: HashMap<u32, usize> = slices
        .iter()
        .enumerate()
        .flat_map(|(i, s)| s.iter().map(move |f| (f.entry, i)))
        .collect();
    let rt_path = std::fs::canonicalize(root.join("rt")).unwrap();
    let rt_dep = format!(
        "yw1-rt = {{ path = {:?} }}",
        rt_path.display().to_string().trim_start_matches(r"\\?\")
    );
    let mut members = Vec::new();
    for (i, s) in slices.iter().enumerate() {
        let name = format!("game_{i:02}");
        let dir = out.join(&name).join("src");
        std::fs::create_dir_all(&dir).unwrap();
        write_if_changed(
            out.join(&name).join("Cargo.toml"),
            format!(
                "[package]\nname = \"yw1-{}\"\nversion = \"0.1.0\"\nedition = \"2021\"\npublish = false\n\n[dependencies]\n{rt_dep}\n",
                name.replace('_', "-")
            ),
        )
        .unwrap();
        let mut src = String::with_capacity(per * 120);
        src.push_str("//! Gerado por yw1-recomp a partir do code.bin do usuário — não editar, não versionar.\n#![allow(unused, unreachable_code, unused_unsafe, non_snake_case, clippy::all)]\nuse yw1_rt::{self as rt, Cpu};\n\n/// pc devolvido pelos pedaços de uma função gigante quando ela retorna\nconst DONE: u32 = u32::MAX;\n\n");
        let mut ext = BTreeSet::new();
        let mut body = String::new();
        for f in s {
            emit_func(f, &funcs, &crate_of, i, &mut ext, &mut body);
        }
        if !ext.is_empty() {
            src.push_str("extern \"Rust\" {\n");
            for e in &ext {
                writeln!(src, "    fn f_{e:08x}(c: &mut Cpu, pc: u32, ret: u32);").unwrap();
            }
            src.push_str("}\n\n");
        }
        src.push_str(&body);
        write_if_changed(dir.join("lib.rs"), src).unwrap();
        members.push(name);
    }
    // despacho
    let dir = out.join("dispatch").join("src");
    std::fs::create_dir_all(&dir).unwrap();
    write_if_changed(
        out.join("dispatch").join("Cargo.toml"),
        format!("[package]\nname = \"yw1-dispatch\"\nversion = \"0.1.0\"\nedition = \"2021\"\npublish = false\n\n[dependencies]\n{rt_dep}\n"),
    )
    .unwrap();
    let mut d = String::from("//! Gerado por yw1-recomp — tabela endereço guest → função recompilada.\n#![allow(unused, clippy::all)]\nuse yw1_rt::Cpu;\n\nextern \"Rust\" {\n");
    for e in funcs.keys() {
        writeln!(d, "    fn f_{e:08x}(c: &mut Cpu, pc: u32, ret: u32);").unwrap();
    }
    d.push_str("}\n\n/// Quantas entradas a tabela tem (funções + entradas extras).\npub const ENTRIES: usize = ");
    writeln!(d, "{};\n", funcs.len() + extra_entries.len()).unwrap();
    d.push_str("#[no_mangle]\npub fn yw1_dispatch(c: &mut Cpu, va: u32, ret: u32) {\n    unsafe {\n        match va {\n");
    for e in funcs.keys() {
        writeln!(d, "            {e:#x} => f_{e:08x}(c, {:#x}, ret),", e & !1).unwrap();
    }
    for (v, fe) in &extra_entries {
        writeln!(d, "            {v:#x} => f_{fe:08x}(c, {:#x}, ret),", v & !1).unwrap();
    }
    d.push_str("            _ => unknown(c, va),\n        }\n    }\n}\n\n#[cold]\n#[inline(never)]\nfn unknown(c: &mut Cpu, va: u32) {\n    panic!(\"[recomp] salto/chamada indireta para {va:#010x} sem entrada recompilada (lr = {:#010x}, na função {:#010x})\", c.r[14], c.func);\n}\n\n/// Lista das entradas (endereço da função, endereço de entrada), para o motor e para depuração.\npub fn entries() -> &'static [(u32, u32)] {\n    &[\n");
    for e in funcs.keys() {
        writeln!(d, "        ({e:#x}, {e:#x}),").unwrap();
    }
    for (v, fe) in &extra_entries {
        writeln!(d, "        ({fe:#x}, {v:#x}),").unwrap();
    }
    d.push_str("    ]\n}\n");
    write_if_changed(dir.join("lib.rs"), d).unwrap();
    members.push("dispatch".into());

    // executável: motor (HLE, renderizador) + todas as fatias + despacho
    let engine_path = std::fs::canonicalize(root.join("engine")).unwrap();
    let engine_dep = format!(
        "yw1 = {{ path = {:?} }}",
        engine_path.display().to_string().trim_start_matches(r"\\?\")
    );
    let app = out.join("app").join("src");
    std::fs::create_dir_all(&app).unwrap();
    let mut deps = format!("{engine_dep}\n{rt_dep}\nyw1-dispatch = {{ path = \"../dispatch\" }}\n");
    let mut uses = String::new();
    for m in members.iter().filter(|m| m.starts_with("game_")) {
        let pkg = format!("yw1-{}", m.replace('_', "-"));
        writeln!(deps, "{pkg} = {{ path = \"../{m}\" }}").unwrap();
        writeln!(uses, "extern crate {};", pkg.replace('-', "_")).unwrap();
    }
    write_if_changed(
        out.join("app").join("Cargo.toml"),
        format!("[package]\nname = \"yw1-recomp-app\"\nversion = \"0.1.0\"\nedition = \"2021\"\npublish = false\n\n[[bin]]\nname = \"yw1r\"\npath = \"src/main.rs\"\n\n[dependencies]\n{deps}"),
    )
    .unwrap();
    write_if_changed(
        app.join("main.rs"),
        format!(
            "//! Gerado por yw1-recomp: executável do jogo recompilado (não versionar).\n{uses}extern crate yw1_dispatch;\n\nfn main() {{\n    yw1::hle::main();\n}}\n"
        ),
    )
    .unwrap();
    members.push("app".into());

    let mut ws = String::from("# Gerado por yw1-recomp. Workspace do código recompilado (não versionar).\n[workspace]\nresolver = \"2\"\nmembers = [\n");
    for m in &members {
        writeln!(ws, "    \"{m}\",").unwrap();
    }
    ws.push_str("]\n\n[profile.release]\nopt-level = 2\ncodegen-units = 16\ndebug = false\nincremental = false\n\n[profile.dev]\nopt-level = 1\n");
    // as regiões densas ficam no último crate (`dense_slice`), partidas em pedaços: compilam no opt-level normal
    let _ = dense_slice;
    write_if_changed(out.join("Cargo.toml"), ws).unwrap();
    println!(
        "gerado em {} ({} crates) em {:.1}s",
        out.display(),
        members.len(),
        t0.elapsed().as_secs_f64()
    );
}

/// Só grava se o conteúdo mudou (mantém o build incremental do código gerado).
fn write_if_changed(path: impl AsRef<Path>, data: impl AsRef<[u8]>) -> std::io::Result<()> {
    let (path, data) = (path.as_ref(), data.as_ref());
    if std::fs::read(path).is_ok_and(|old| old == data) {
        return Ok(());
    }
    std::fs::write(path, data)
}

/// Chamada a `t` com o endereço de retorno `ret` (constante numa chamada, `ret` da própria função numa chamada de
/// cauda).
fn call_expr(
    t: u32,
    ret: &str,
    funcs: &BTreeMap<u32, Func>,
    crate_of: &HashMap<u32, usize>,
    me: usize,
    ext: &mut BTreeSet<u32>,
) -> String {
    if funcs.contains_key(&t) {
        if crate_of.get(&t) == Some(&me) {
            format!("f_{t:08x}(c, {:#x}, {ret});", t & !1)
        } else {
            ext.insert(t);
            format!("unsafe {{ f_{t:08x}(c, {:#x}, {ret}) }};", t & !1)
        }
    } else {
        format!("rt::jump(c, {t:#x}, {ret});")
    }
}

fn emit_func(
    f: &Func,
    funcs: &BTreeMap<u32, Func>,
    crate_of: &HashMap<u32, usize>,
    me: usize,
    ext: &mut BTreeSet<u32>,
    out: &mut String,
) {
    let mut blocks: Vec<(u32, String)> = Vec::new();
    // região densa (todo endereço é bloco: assembly do decodificador de vídeo): sem checagem de preempção nos laços.
    // Com ela, o LLVM levava >15 min só nessa função; o decodificador roda pouco por chamada e cede na entrada.
    let dense = f.insts.len() > 256 && f.leaders.len() == f.insts.len();
    for &l in &f.leaders {
        if !f.insts.contains_key(&l) {
            continue;
        }
        let out = &mut String::new();
        writeln!(out, "            {l:#x} => {{").unwrap();
        let mut pc = l;
        loop {
            let Some(i) = f.insts.get(&pc) else {
                break;
            };
            let uncond = i.cond == 14;
            let s = match &i.flow {
                Flow::CallInd => format!("{} c.func = {:#x};", i.body, f.entry),
                Flow::Next => i.body.clone(),
                Flow::Undefined => {
                    writeln!(out, "                {}", i.body).unwrap();
                    break;
                }
                Flow::Halt => {
                    writeln!(out, "                {{ {} rt::halted({pc:#x}); }}", i.body).unwrap();
                    break;
                }
                Flow::Branch(t) => {
                    let t = *t;
                    let ta = t & !1;
                    let intra = f.insts.contains_key(&ta)
                        && f.leaders.contains(&ta)
                        && (t == f.entry || !funcs.contains_key(&t));
                    if intra && ta <= pc && !dense {
                        // salto para trás (laço): ponto de checagem de preempção
                        format!("rt::poll(c); pc = {ta:#x}; continue;")
                    } else if intra {
                        format!("pc = {ta:#x}; continue;")
                    } else {
                        format!("{} return;", call_expr(t, "ret", funcs, crate_of, me, ext))
                    }
                }
                Flow::Call(t) => {
                    let r = (pc + i.len) | (f.entry & 1);
                    format!(
                        "c.r[14] = {r:#x}; {} c.func = {:#x};",
                        call_expr(*t, &format!("{r:#x}"), funcs, crate_of, me, ext),
                        f.entry
                    )
                }
                Flow::Return => format!("{} if t == ret {{ return; }} pc = t & !1; continue;", i.body),
                // salto indireto para o próprio endereço de retorno (`bx r3` com o lr salvo em r3, `ldm rX, {pc}`) =
                // retorno; senão segue dentro da função ou sai pelo despacho (braço `_`)
                Flow::JumpInd => format!("{} if t == ret {{ return; }} pc = t; continue;", i.body),
                Flow::Table(_) => format!("{} pc = t; continue;", i.body),
            };
            if uncond {
                writeln!(out, "                {{ {s} }}").unwrap();
            } else {
                writeln!(out, "                if {} {{ {s} }}", arm::cond_expr(i.cond)).unwrap();
            }
            let ends = uncond
                && matches!(
                    i.flow,
                    Flow::Branch(_) | Flow::Return | Flow::JumpInd | Flow::Table(_)
                );
            if ends {
                break;
            }
            let next = pc + i.len;
            if f.leaders.contains(&next) || !f.insts.contains_key(&next) {
                writeln!(out, "                pc = {next:#x};").unwrap();
                break;
            }
            pc = next;
        }
        out.push_str("            }\n");
        blocks.push((l, std::mem::take(out)));
    }
    let head = format!(
        "#[no_mangle]\npub fn f_{:08x}(c: &mut Cpu, mut pc: u32, ret: u32) {{\n    c.func = {:#x};\n    rt::poll(c);\n    loop {{\n",
        f.entry, f.entry
    );
    if blocks.len() <= CHUNK_MAX {
        out.push_str(&head);
        out.push_str("        match pc {\n");
        for (_, b) in &blocks {
            out.push_str(b);
        }
        out.push_str("            _ => { rt::jump(c, pc, ret); return; }\n        }\n    }\n}\n\n");
        return;
    }
    // Função gigante (o decodificador de vídeo em assembly tem ~19 mil blocos): um único `match` deixa o rustc
    // superlinear (>25 min até em opt-level 0). Parte em pedaços de CHUNK blocos, cada um uma função com o próprio
    // laço; um salto para fora do pedaço devolve o pc ao laço de cima, que escolhe o pedaço pela faixa de endereços.
    // Retorno da função = DONE (ímpar e fora do .text: nunca é bloco).
    let chunks: Vec<&[(u32, String)]> = blocks.chunks(CHUNK).collect();
    let range = |k: usize| {
        let lo = chunks[k][0].0;
        let hi = chunks.get(k + 1).map(|n| n[0].0 - 1).unwrap_or(u32::MAX - 1);
        (lo, hi)
    };
    out.push_str(&head);
    out.push_str("        pc = match pc {\n");
    for k in 0..chunks.len() {
        let (lo, hi) = range(k);
        writeln!(out, "            {lo:#x}..={hi:#x} => f_{:08x}_k{k}(c, pc, ret),", f.entry).unwrap();
    }
    out.push_str("            _ => { rt::jump(c, pc, ret); return; }\n        };\n        if pc == DONE { return; }\n    }\n}\n\n");
    for (k, ch) in chunks.iter().enumerate() {
        let (lo, hi) = range(k);
        writeln!(
            out,
            "#[inline(never)]\nfn f_{:08x}_k{k}(c: &mut Cpu, mut pc: u32, ret: u32) -> u32 {{\n    loop {{\n        match pc {{",
            f.entry
        )
        .unwrap();
        for (_, b) in ch.iter() {
            out.push_str(&b.replace("return;", "return DONE;"));
        }
        // pc dentro da faixa mas que não é bloco: mesmo tratamento do braço `_` de uma função normal
        writeln!(
            out,
            "            {lo:#x}..={hi:#x} => {{ rt::jump(c, pc, ret); return DONE; }}\n            _ => return pc,\n        }}\n    }}\n}}\n"
        )
        .unwrap();
    }
}

/// Funções com mais blocos que isto são partidas em pedaços de `CHUNK` blocos (ver `emit_func`).
const CHUNK_MAX: usize = 1024;
const CHUNK: usize = 384;
