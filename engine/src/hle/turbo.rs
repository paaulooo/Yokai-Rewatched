//! Turbo (G6): enquanto uma tecla é SEGURADA (padrão Tab; um botão do controle; `turbo` no roteiro de entrada), o
//! VBlank chega `emulacao.turbo` vezes mais rápido (2–4×, padrão 3×). O jogo é por quadro (a lógica anda por VBlank,
//! não por deltatime), então acelerar = gerar VBlanks mais depressa; o relógio do sistema (`GetSystemTick`), os
//! timeouts do kernel e o relógio do DSP NÃO mudam.
//!
//! Durante o turbo:
//! - a thread de VBlank (`hle::boot`) troca a taxa e rebaseia o prazo absoluto na troca: ao soltar, o próximo VBlank
//!   vem 1/59,83 s depois do último (volta ao normal em ≤ 1 quadro);
//! - o som fica mudo: o DSP continua gerando e publicando os quadros no mesmo ritmo (contrapressão e consumo da fila
//!   intactos), só a saída vai em silêncio (`services::dsp::audio_frame`);
//! - a interpolação de quadros pausa (`render::set_interp(0)`; o valor volta ao soltar);
//! - a resolução interna cai para a nativa e o FXAA de cena desliga (`render::set_scale(1)`/`set_fxaa(false)`; voltam
//!   ao soltar). Medido na janela com 6× + FXAA: sem isso a thread da GPU não dá conta de 3× os quadros e o jogo fica
//!   em ≈31 quadros/s com o VBlank a 179 Hz (trocas adiadas esperando a GPU). `YW_TURBO_HIRES=1` mantém a resolução.
//! - o vigia mede pelo relógio de VBlank em segundos de parede (`hle::relogio_vblank_s`), não por VBlanks.
//!
//! `YW_TURBO=0` desliga o turbo; `YW_TURBO=2..4` escolhe o multiplicador (vence o arquivo).

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering::Relaxed};
use std::sync::Mutex;

/// Quem está segurando o turbo (bits).
pub const JANELA: u8 = 1;
pub const ROTEIRO: u8 = 2;

static FONTES: AtomicU8 = AtomicU8::new(0);
static MULT: AtomicU32 = AtomicU32::new(3);
static ATIVO: AtomicBool = AtomicBool::new(false);
/// interpolação pedida antes do turbo (ou pela janela durante ele); `u32::MAX` = nada guardado
static INTERP_SALVO: AtomicU32 = AtomicU32::new(u32::MAX);
/// resolução interna e FXAA de antes do turbo (`u32::MAX` / 2 = nada guardado)
static ESCALA_SALVA: AtomicU32 = AtomicU32::new(u32::MAX);
static FXAA_SALVO: AtomicU8 = AtomicU8::new(2);
/// serializa as transições (janela e roteiro podem apertar ao mesmo tempo)
static TRANS: Mutex<()> = Mutex::new(());
/// VBlanks gerados com o turbo ligado e quantas vezes ele foi ligado (diagnóstico)
pub static VBLANKS_TURBO: AtomicU64 = AtomicU64::new(0);
pub static ACIONAMENTOS: AtomicU64 = AtomicU64::new(0);
/// instante da última troca pedida (para a thread de VBlank medir a latência da troca de ritmo)
static PEDIDO_EM: Mutex<Option<std::time::Instant>> = Mutex::new(None);

/// Tempo desde a última troca pedida (e o esquece): a thread de VBlank chama quando aplica o ritmo novo.
pub fn tomar_pedido() -> Option<std::time::Duration> {
    PEDIDO_EM.lock().unwrap_or_else(|e| e.into_inner()).take().map(|t| t.elapsed())
}

fn env() -> Option<u32> {
    static E: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
    *E.get_or_init(|| std::env::var("YW_TURBO").ok().and_then(|v| v.trim().parse::<u32>().ok()))
}

/// Turbo permitido (`YW_TURBO=0` desliga).
pub fn permitido() -> bool {
    env() != Some(0)
}

/// Multiplicador das configurações (`emulacao.turbo`); `YW_TURBO=N` vence.
pub fn set_mult(k: u32) {
    MULT.store(k.clamp(2, 4), Relaxed);
}

/// Multiplicador em uso (2–4).
pub fn mult() -> u32 {
    match env() {
        Some(k @ 2..=4) => k,
        _ => MULT.load(Relaxed),
    }
}

/// Uma fonte apertou (`on`) ou soltou o turbo.
pub fn segurar(fonte: u8, on: bool) {
    // caminho rápido (a janela chama a cada ≈1 ms): nada mudou para esta fonte
    if (FONTES.load(Relaxed) & fonte != 0) == on {
        return;
    }
    let _g = TRANS.lock().unwrap_or_else(|e| e.into_inner());
    let antes = FONTES.load(Relaxed);
    let depois = if on { antes | fonte } else { antes & !fonte };
    if antes == depois {
        return;
    }
    FONTES.store(depois, Relaxed);
    let ativo = depois != 0 && permitido();
    if ativo == ATIVO.load(Relaxed) {
        return;
    }
    let q = super::VBLANKS.load(Relaxed);
    *PEDIDO_EM.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now());
    if ativo {
        INTERP_SALVO.store(crate::render::interp_target(), Relaxed);
        crate::render::set_interp(0);
        if !mantem_hires() {
            ESCALA_SALVA.store(crate::render::scale(), Relaxed);
            FXAA_SALVO.store(crate::render::fxaa() as u8, Relaxed);
            crate::render::set_scale(1);
            crate::render::set_fxaa(false);
        }
        ACIONAMENTOS.fetch_add(1, Relaxed);
        ATIVO.store(true, Relaxed);
        eprintln!("[turbo] ligado {}× (q={q})", mult());
    } else {
        ATIVO.store(false, Relaxed);
        let s = INTERP_SALVO.swap(u32::MAX, Relaxed);
        if s != u32::MAX {
            crate::render::set_interp(s);
        }
        let e = ESCALA_SALVA.swap(u32::MAX, Relaxed);
        if e != u32::MAX {
            crate::render::set_scale(e);
        }
        match FXAA_SALVO.swap(2, Relaxed) {
            2 => {}
            f => crate::render::set_fxaa(f != 0),
        }
        eprintln!("[turbo] desligado (q={q})");
    }
}

/// `YW_TURBO_HIRES=1`: o turbo não mexe na resolução interna nem no FXAA.
fn mantem_hires() -> bool {
    std::env::var("YW_TURBO_HIRES").is_ok_and(|v| v != "0")
}

/// O turbo está ligado (alguém segurando e permitido).
pub fn ativo() -> bool {
    ATIVO.load(Relaxed)
}

/// Fator aplicado ao VBlank agora (1 sem turbo).
pub fn fator() -> f64 {
    if ativo() {
        mult() as f64
    } else {
        1.0
    }
}

/// A janela quer mudar a interpolação (menu, outro monitor): com o turbo ligado só guarda o pedido para quando ele for
/// solto; sem turbo aplica na hora.
pub fn pedir_interp(hz: u32) {
    let _g = TRANS.lock().unwrap_or_else(|e| e.into_inner());
    if ATIVO.load(Relaxed) {
        INTERP_SALVO.store(hz, Relaxed);
    } else {
        crate::render::set_interp(hz);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turbo_liga_pausa_interp_e_devolve() {
        crate::render::set_interp(120);
        crate::render::set_scale(4);
        crate::render::set_fxaa(true);
        set_mult(3);
        assert_eq!(fator(), 1.0);
        segurar(JANELA, true);
        assert!(ativo());
        assert_eq!(fator(), 3.0);
        assert_eq!(crate::render::interp_target(), 0, "interpolação pausa no turbo");
        assert_eq!((crate::render::scale(), crate::render::fxaa()), (1, false), "resolução nativa no turbo");
        // a janela pede outra interpolação durante o turbo: só vale ao soltar
        pedir_interp(90);
        assert_eq!(crate::render::interp_target(), 0);
        segurar(ROTEIRO, true);
        segurar(JANELA, false);
        assert!(ativo(), "o roteiro ainda segura");
        segurar(ROTEIRO, false);
        assert!(!ativo());
        assert_eq!(fator(), 1.0);
        assert_eq!(crate::render::interp_target(), 90);
        assert_eq!((crate::render::scale(), crate::render::fxaa()), (4, true));
        crate::render::set_scale(1);
        crate::render::set_fxaa(false);
        set_mult(9);
        assert_eq!(MULT.load(Relaxed), 4);
        set_mult(3);
    }
}
