//! Vigia de travamento: roda em qualquer modo (janela ou sem janela). Se o jogo para de apresentar quadros por mais de
//! 3 s do console (sem estar pausado pelo menu), escreve no console as linhas `[vigia]` de sempre e, na primeira
//! detecção, um relatório completo em `stall.log` (pasta de logs da execução, ver `hle::logs`):
//! threads, registradores da thread em execução, memória apontada por eles, análise de ciclo em listas ligadas
//! (o travamento já visto: `FUN_001420a8` girando numa fila de buffers de áudio com ciclo) e o histórico recente do
//! DSP.

use super::{kernel, logs, services, vblank_hz, VBLANKS};
use std::sync::atomic::Ordering;
use std::time::Duration;

/// Liga o vigia (uma vez, depois do boot).
pub fn start() {
    std::thread::Builder::new()
        .name("vigia".into())
        .spawn(|| {
            let (mut last_swaps, mut last_t, mut dumps) = (0u64, 0.0f64, 0u32);
            loop {
                std::thread::sleep(Duration::from_millis(250));
                if super::paused() {
                    last_t = VBLANKS.load(Ordering::Relaxed) as f64 / vblank_hz();
                    continue;
                }
                let t = VBLANKS.load(Ordering::Relaxed) as f64 / vblank_hz();
                let sw = services::gsp::SWAPS[0].load(Ordering::Relaxed);
                if sw != last_swaps {
                    (last_swaps, last_t, dumps) = (sw, t, 0);
                } else if t - last_t > 3.0 + dumps as f64 * 0.5 && dumps < 12 {
                    dumps += 1;
                    eprintln!("[vigia] {t:.1}s sem quadro novo: {}", kernel::running_snapshot());
                    if dumps == 1 {
                        let r = stall_report(t);
                        logs::append("stall.log", &r);
                        match logs::dir() {
                            Some(d) => eprintln!("[vigia] relatório completo: {}", d.join("stall.log").display()),
                            None => eprintln!("{r}"),
                        }
                    }
                    if dumps == 12 {
                        eprintln!("[vigia] threads:\n{}", kernel::threads_report());
                    }
                }
            }
        })
        .ok();
}

fn mapped(a: u32, len: u32) -> bool {
    a >= 0x1000 && super::mem::is_mapped(a) && super::mem::is_mapped(a.wrapping_add(len.saturating_sub(1)))
}

fn rd32(a: u32) -> Option<u32> {
    if !mapped(a, 4) {
        return None;
    }
    let mut b = [0u8; 4];
    yw1_rt::read_bytes(a, &mut b);
    Some(u32::from_le_bytes(b))
}

fn rd16(a: u32) -> Option<u16> {
    if !mapped(a, 2) {
        return None;
    }
    let mut b = [0u8; 2];
    yw1_rt::read_bytes(a, &mut b);
    Some(u16::from_le_bytes(b))
}

fn rd8(a: u32) -> Option<u8> {
    if !mapped(a, 1) {
        return None;
    }
    let mut b = [0u8; 1];
    yw1_rt::read_bytes(a, &mut b);
    Some(b[0])
}

/// Despeja `len` bytes a partir de `a`, em palavras de 32 bits, 8 por linha.
fn hexdump(a: u32, len: u32) -> String {
    let mut s = String::new();
    let mut o = 0;
    while o < len {
        s.push_str(&format!("    {:08x}:", a + o));
        for k in 0..8 {
            match rd32(a + o + 4 * k) {
                Some(w) => s.push_str(&format!(" {w:08x}")),
                None => s.push_str(" ········"),
            }
        }
        s.push('\n');
        o += 32;
    }
    s
}

/// Nó da fila de buffers de áudio do SDK (`FUN_001420a8`): próximo em +0x14, estado em +0x11 (1 na fila, 2 tocando,
/// 3 livre, 4 concluído pelo DSP), número de sequência em +0x12. Segue a lista a partir de `p` e acusa ciclo.
fn walk_list(p: u32) -> Option<String> {
    let mut s = String::new();
    let mut seen = Vec::new();
    let mut n = p;
    for _ in 0..24 {
        let (Some(next), Some(state), Some(id)) = (rd32(n.wrapping_add(0x14)), rd8(n.wrapping_add(0x11)), rd16(n.wrapping_add(0x12)))
        else {
            return None;
        };
        if seen.contains(&n) {
            s.push_str(&format!("      → {n:08x} (JÁ VISTO: ciclo)\n"));
            return Some(s);
        }
        seen.push(n);
        s.push_str(&format!("      nó {n:08x} estado {state} sequência {id} próximo {next:08x}"));
        s.push_str(if next == n { "  ← APONTA PARA SI MESMO\n" } else { "\n" });
        if next == 0 {
            s.push_str("      (fim da lista)\n");
            return Some(s);
        }
        if next == n {
            return Some(s);
        }
        n = next;
    }
    s.push_str("      … (mais de 24 nós)\n");
    Some(s)
}

/// Relatório completo de travamento (texto).
pub fn stall_report(t: f64) -> String {
    let mut r = String::new();
    r.push_str(&format!(
        "================ TRAVAMENTO a {t:.1} s do console ================\nquadros apresentados: {} · VBlanks: {}\n\n",
        services::gsp::SWAPS[0].load(Ordering::Relaxed),
        VBLANKS.load(Ordering::Relaxed)
    ));
    r.push_str(&format!("thread em execução: {}\n\nthreads:\n{}\n", kernel::running_snapshot(), kernel::threads_report()));
    if let Some((tid, prio, func, regs)) = kernel::running_regs() {
        r.push_str(&format!("registradores de t{tid} (prio {prio}, função {func:#x}):\n"));
        let mut shown: Vec<u32> = Vec::new();
        for (i, &v) in regs.iter().enumerate() {
            r.push_str(&format!("  r{i:<2} = {v:08x}\n"));
            if !(0x100000..0x2000_0000).contains(&v) || !mapped(v, 4) {
                continue;
            }
            // lista ligada com ciclo? (campo "próximo" em +0x14, o padrão da fila de áudio)
            if let Some(w) = walk_list(v) {
                if w.contains("APONTA PARA SI MESMO") || w.contains("ciclo") {
                    r.push_str(&format!("    ** r{i}: lista a partir de {v:08x} tem CICLO:\n{w}"));
                }
            }
            let base = v & !0x1f;
            if shown.iter().any(|&b| b.abs_diff(base) < 0x60) {
                continue;
            }
            shown.push(base);
            r.push_str(&format!("    memória em torno de r{i} ({v:08x}):\n{}", hexdump(base.saturating_sub(0x20), 0x80)));
        }
        r.push_str("\nlista a partir de cada registrador que aponta para memória (nó = +0x14 próximo, +0x11 estado, +0x12 sequência):\n");
        for (i, &v) in regs.iter().enumerate() {
            if (0x100000..0x2000_0000).contains(&v) {
                if let Some(w) = walk_list(v) {
                    r.push_str(&format!("  r{i} = {v:08x}:\n{w}"));
                }
            }
        }
    }
    r.push_str("
últimas chamadas da fila de áudio do jogo (anel do rt, da mais antiga para a mais nova):
");
    r.push_str(&yw1_rt::trace_dump(300));
    let h = services::dsp::history();
    r.push_str(&format!("\nhistórico do estado que o DSP devolveu ao jogo (últimas {} mudanças):\n", h.len().min(120)));
    for l in h.iter().rev().take(120).rev() {
        r.push_str(&format!("  {l}\n"));
    }
    r.push_str("================ fim do relatório ================\n");
    r
}
