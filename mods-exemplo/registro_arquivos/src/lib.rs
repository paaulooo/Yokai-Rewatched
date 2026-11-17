//! Mod de exemplo: um gancho "antes" na função do jogo que abre um arquivo do `.fa` pelo caminho
//! (`FUN_0047359c(gerenciador, caminho, &buffer, &tamanho, &achou, heap)`) e outro na variante que descomprime
//! (`FUN_0047365c`, mesma assinatura). Cada abertura vira uma linha `[mod registro_arquivos] …` no log.

use std::sync::atomic::{AtomicU64, Ordering};
use yw1_modapi::{Api, Tipo};

static N: AtomicU64 = AtomicU64::new(0);

fn iniciar(api: &Api) -> bool {
    for (va, qual) in [(0x47359c, "abre"), (0x47365c, "abre+descomprime")] {
        let r = api.registrar(va, Tipo::Antes, 100, qual, move |ctx| {
            let n = N.fetch_add(1, Ordering::Relaxed) + 1;
            let caminho = ctx.ler_cstr(ctx.reg(1), 200);
            ctx.api().log(&format!("#{n} {qual}: {caminho} (VBlank {})", ctx.api().vblank()));
        });
        if r.is_err() {
            api.log(&format!("não consegui registrar o gancho em {va:#x}"));
            return false;
        }
    }
    api.log("registro de arquivos ligado");
    true
}

yw1_modapi::yw1_mod!(iniciar);
