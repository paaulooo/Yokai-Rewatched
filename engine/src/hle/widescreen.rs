//! Widescreen da tela de cima (fase G3): o lado do JOGO. O renderizador (`render/hires.rs`, `render/vulkan.rs`) desenha a
//! sombra N× mais larga; aqui o jogo passa a calcular a projeção e o culling com o frustum largo, para a cena sair sem
//! esticar e nada sumir nas bordas.
//!
//! Confirmado no jogo (passo 0: `YW_HOOK_LOG` em 0x12953c/0x2e11ec/0x16efc4 na rota até a rua, `route_probe4`):
//! - `FUN_0016efc4` (perspectiva da câmera, chamada por `FUN_002cc0bc`) monta uma projeção QUADRADA: esquerda = −topo,
//!   direita = topo (m[0] = m[5] = 1/tan(fovy/2), ex. 4,2193). O aspecto 5:3 NÃO entra nela.
//! - `FUN_0012953c(ctx)` (envio da câmera por quadro → `FUN_00129d28` escreve c0..c7 do shader de vértice) copia a
//!   projeção de `*(ctx+0xd80)+0x140` para ctx+0xc7c e, na perspectiva (m[11] ≠ 0), multiplica m[0] e m[8] (escala e
//!   deslocamento horizontais) por `*(ctx+0xd90) / *(ctx+0xd8c)` = 240/400 (altura/largura da tela; 320×240 na tela de
//!   baixo) e TRANSPÕE com giro: c0 = linha y do jogo, c1 = linha x × 0,6 (c1.x = 2,5316 = 4,2193 × 0,6). Ou seja, a
//!   horizontal da tela = eixo y do clip do PICA = linhas do alvo de 400 (a tela de cima é desenhada deitada).
//!   ctx+0xb48 = (0, 1, 0, 1) nos dois modos [H: deslocamento/escala do viewport; não usado aqui].
//! - O culling por objeto é `FUN_002e11ec(câm, &(L, A), obj)` → `FUN_001aae7c` com s0 = L/A: o chamador passa (400, 240)
//!   (constantes na pilha) e o teste horizontal usa tan(fovy/2)·L/A.
//!
//! Ganchos ("substituir", com a original chamada no meio): com widescreen em efeito (`render::wide_game_w`), e só quando
//! o valor visto é o da tela de cima (400×240 + perspectiva), troca 400 por W' = 240·proporção durante a chamada e
//! devolve 400 depois. Assim a projeção (m[0] e m[8]) sai com 240/W' e o culling usa W'/240 — o jogo enxerga o
//! frustum largo; a imagem NATIVA (memória guest, 400 px) sai comprimida na horizontal, mas não é exibida (a sombra
//! larga a estica de volta). A tela de baixo (320×240) e as ortográficas (2D) não mudam. Ao vivo: cada chamada lê a
//! proporção atual.

/// HUD ancorada (G5): fica aqui dentro para não mexer na lista de módulos de `hle/mod.rs`.
#[path = "hud.rs"]
pub mod hud;

use super::hooks::{self, Tipo};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use yw1_rt::{r32, w32};

static OK: AtomicBool = AtomicBool::new(false);
/// chamadas em que a largura foi trocada: [projeção, culling]
static TROCAS: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];

/// Os ganchos do jogo estão registrados (sem eles o renderizador não alarga nada: a cena sairia esticada).
pub fn ganchos_ok() -> bool {
    OK.load(Ordering::Relaxed)
}

/// (projeções alargadas, testes de culling alargados) desde o boot.
pub fn trocas() -> (u64, u64) {
    (TROCAS[0].load(Ordering::Relaxed), TROCAS[1].load(Ordering::Relaxed))
}

fn rf(a: u32) -> f32 {
    f32::from_bits(r32(a))
}

/// Põe `w` em `a` durante a chamada original e volta 400 (se ninguém mexeu no meio).
fn trocar_largura(c: &mut yw1_rt::Cpu, orig: &hooks::Original, a: u32, w: f32) {
    w32(a, w.to_bits());
    orig.chamar(c);
    if r32(a) == w.to_bits() {
        w32(a, 400f32.to_bits());
    }
}

/// Registra os ganchos (no `boot`, antes da primeira instrução guest). Não essenciais: `YW_HOOKS=0` os desliga, e
/// então o widescreen fica desligado também no renderizador.
pub fn registrar() {
    let proj = hooks::registrar(
        0x12953c,
        Tipo::substituir(|c, orig| {
            let ctx = c.r[0];
            let Some(w) = crate::render::wide_game_w() else {
                orig.chamar(c);
                return;
            };
            let env = r32(ctx.wrapping_add(0xd80));
            let persp = env != 0 && super::mem::is_mapped(env.wrapping_add(0x16c)) && r32(env.wrapping_add(0x140 + 0x2c)) != 0;
            if persp && rf(ctx.wrapping_add(0xd8c)) == 400.0 && rf(ctx.wrapping_add(0xd90)) == 240.0 {
                TROCAS[0].fetch_add(1, Ordering::Relaxed);
                trocar_largura(c, orig, ctx.wrapping_add(0xd8c), w);
            } else {
                orig.chamar(c);
            }
        }),
        "widescreen: projeção (FUN_0012953c)",
        0,
        false,
    );
    // `YW_WIDE_CULL=0`: o culling fica com o 5:3 (só para a prova A/B de que o gancho do culling é necessário)
    let cull_on = std::env::var("YW_WIDE_CULL").map_or(true, |v| v != "0");
    let cull = hooks::registrar(
        0x2e11ec,
        Tipo::substituir(move |c, orig| {
            let par = c.r[1];
            let Some(w) = crate::render::wide_game_w().filter(|_| cull_on) else {
                orig.chamar(c);
                return;
            };
            if super::mem::is_mapped(par) && super::mem::is_mapped(par.wrapping_add(7)) && rf(par) == 400.0 && rf(par.wrapping_add(4)) == 240.0 {
                TROCAS[1].fetch_add(1, Ordering::Relaxed);
                trocar_largura(c, orig, par, w);
            } else {
                orig.chamar(c);
            }
        }),
        "widescreen: culling (FUN_002e11ec)",
        0,
        false,
    );
    // com YW_HOOKS=0 o registro dos não essenciais é aceito e ignorado (`hooks::registrar`)
    let ignorados = std::env::var("YW_HOOKS").is_ok_and(|v| v == "0");
    match (proj, cull) {
        (Ok(_), Ok(_)) if !ignorados => OK.store(true, Ordering::Relaxed),
        (Ok(_), Ok(_)) => eprintln!("[widescreen] YW_HOOKS=0: widescreen desligado"),
        (p, q) => {
            for e in [p.err(), q.err()].into_iter().flatten() {
                eprintln!("[widescreen] gancho não registrado ({e}): widescreen desligado");
            }
        }
    }
}

/// Linha `[widescreen]` no fim da rodada (se esteve ligado): trocas do lado do jogo e draws por classe no renderizador.
pub fn log_resumo() {
    let (p, c) = trocas();
    let n: Vec<u64> = crate::render::LARGA_CONT.iter().map(|x| x.load(Ordering::Relaxed)).collect();
    if p + c + n.iter().sum::<u64>() == 0 {
        return;
    }
    eprintln!(
        "[widescreen] projeções alargadas {p} · testes de culling alargados {c} · draws da tela de cima: cena {} · 2D central {} · 2D de tela cheia {} · fora {}",
        n[1], n[2], n[3], n[0]
    );
}
