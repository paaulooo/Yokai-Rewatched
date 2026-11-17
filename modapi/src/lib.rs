//! ABI C dos **mods de código** do Yo-kai Watch recompilado (`yw1r`) e um invólucro Rust seguro por cima dela.
//!
//! Um mod de código é uma DLL (`cdylib`) na pasta do mod (`%APPDATA%/yw1/mods/<nome>/`, apontada pelo `mod.toml`,
//! seção `[codigo] dll = "…"`) que exporta duas funções com ABI C:
//!
//! ```c
//! uint32_t yw1_mod_api_versao(void);                       // a versão da API com que o mod foi compilado
//! bool     yw1_mod_init(const Yw1Api *api, uint32_t versao); // versao = a do motor; false = o mod recusa
//! ```
//!
//! O motor lê `yw1_mod_api_versao` ANTES de chamar `yw1_mod_init` e recusa (com mensagem no log e no menu) um mod de
//! versão maior que a dele ou menor que [`YW1_API_VERSAO_MINIMA`]. A estrutura [`Yw1Api`] só cresce no fim: um mod
//! compilado com uma versão menor vê só os campos que conhece (`tamanho` diz quantos bytes o motor preencheu).
//!
//! **Confiança**: a DLL roda dentro do processo do jogo com os mesmos direitos dele (código nativo, sem caixa de
//! areia). Por isso os mods de código vêm desligados e o menu avisa ao ligar.
//!
//! Ganchos: só na ENTRADA de funções do jogo (endereço virtual do `code.bin`, o `FUN_<va>` do Ghidra), em três tipos
//! — antes (pode mudar argumentos), substituir (decide se chama a original; um por função) e depois (vê o retorno).
//! O callback recebe um contexto opaco válido só durante a chamada; tudo que mexe em registradores ou chama funções
//! do jogo passa por ele, na thread guest que entrou na função.
//!
//! Uso em Rust (ver `mods-exemplo/registro_arquivos`):
//!
//! ```ignore
//! fn iniciar(api: &yw1_modapi::Api) -> bool {
//!     api.registrar(0x47359c, yw1_modapi::Tipo::Antes, 100, "meu gancho", |ctx| {
//!         let caminho = ctx.ler_cstr(ctx.reg(1), 256);
//!         ctx.api().log(&format!("abrindo {caminho}"));
//!     }).is_ok()
//! }
//! yw1_modapi::yw1_mod!(iniciar);
//! ```

use std::ffi::{c_char, c_void, CString};

/// Versão desta API (sobe a cada campo novo no fim de [`Yw1Api`] ou mudança de significado).
pub const YW1_API_VERSAO: u32 = 1;
/// Menor versão de mod que o motor desta versão ainda aceita.
pub const YW1_API_VERSAO_MINIMA: u32 = 1;

/// Tipo do gancho para `registrar_gancho`.
pub const GANCHO_ANTES: u32 = 0;
pub const GANCHO_SUBSTITUIR: u32 = 1;
pub const GANCHO_DEPOIS: u32 = 2;

/// Nível do `log`.
pub const LOG_INFO: u32 = 0;
pub const LOG_AVISO: u32 = 1;
pub const LOG_ERRO: u32 = 2;

/// Contexto de um gancho em execução (opaco; válido só durante o callback).
#[repr(C)]
pub struct Yw1Ctx {
    _privado: [u8; 0],
}

/// Callback de gancho: `ctx` = contexto da chamada, `user` = o ponteiro passado ao registrar.
pub type Yw1Callback = extern "C" fn(ctx: *mut Yw1Ctx, user: *mut c_void);

/// Tabela de funções que o motor passa ao `yw1_mod_init`. Fica válida até o fim do processo.
#[repr(C)]
pub struct Yw1Api {
    /// [`YW1_API_VERSAO`] do motor.
    pub versao: u32,
    /// `size_of::<Yw1Api>()` do motor (campos além disso não existem nele).
    pub tamanho: u32,
    /// Registra um gancho na entrada da função `va` (bit Thumb opcional). `tipo` = `GANCHO_*`; `prioridade`: menor
    /// roda primeiro; `nome`: texto C (copiado). Devolve o id (> 0) ou 0 se recusado (motivo no log): `va` não é
    /// entrada de função, a função já tem um "substituir", tipo inválido.
    pub registrar_gancho:
        extern "C" fn(va: u32, tipo: u32, prioridade: i32, nome: *const c_char, cb: Yw1Callback, user: *mut c_void) -> u64,
    /// Remove um gancho registrado por este mod. false = id desconhecido/já removido.
    pub remover_gancho: extern "C" fn(id: u64) -> bool,
    /// Só num gancho "substituir": executa a função original com os registradores atuais. false fora dele.
    pub chamar_original: extern "C" fn(ctx: *mut Yw1Ctx) -> bool,
    /// Registrador atual: 0–14 = r0–r14 (sp = 13, lr = 14); 15 = endereço da função do gancho.
    pub ler_reg: extern "C" fn(ctx: *mut Yw1Ctx, i: u32) -> u32,
    /// Escreve r0–r14 (15 é ignorado).
    pub escrever_reg: extern "C" fn(ctx: *mut Yw1Ctx, i: u32, v: u32),
    /// Num gancho "depois": valores da ENTRADA (0–3 = r0–r3, 4 = sp, 5 = lr). Nos outros tipos, os atuais.
    pub ler_entrada: extern "C" fn(ctx: *mut Yw1Ctx, i: u32) -> u32,
    /// Copia `len` bytes da memória do jogo. false (nada copiado) se algum byte não está mapeado.
    pub ler_mem: extern "C" fn(va: u32, dst: *mut u8, len: usize) -> bool,
    /// Copia `len` bytes para a memória do jogo. false (nada escrito) se algum byte não está mapeado.
    pub escrever_mem: extern "C" fn(va: u32, src: *const u8, len: usize) -> bool,
    /// Só dentro de um gancho: chama a função do jogo `va` com `n` argumentos (r0–r3, o resto na pilha, AAPCS) e
    /// devolve r0 | r1 << 32. Os registradores do contexto voltam ao que eram (só o retorno muda algo).
    pub chamar_funcao: extern "C" fn(ctx: *mut Yw1Ctx, va: u32, args: *const u32, n: usize) -> u64,
    /// Substitui o arquivo do `.fa` de caminho `caminho` (ex.: "data/menu/win_en.xa") por estes bytes (copiados)
    /// a partir da próxima abertura. Vence os mods de arquivos. false = caminho vazio/inválido.
    pub substituir_arquivo: extern "C" fn(caminho: *const c_char, dados: *const u8, len: usize) -> bool,
    /// Uma linha no log do jogo (`game.log`), com o nome do mod. `nivel` = `LOG_*`.
    pub log: extern "C" fn(nivel: u32, msg: *const c_char),
    /// Quantos VBlanks (≈ 1/59,83 s de console) já passaram.
    pub vblank: extern "C" fn() -> u64,
}

/// Assinatura de `yw1_mod_init`.
pub type Yw1ModInit = unsafe extern "C" fn(api: *const Yw1Api, versao: u32) -> bool;
/// Assinatura de `yw1_mod_api_versao`.
pub type Yw1ModApiVersao = unsafe extern "C" fn() -> u32;

/// Uma versão de mod `v` é aceita por um motor desta versão?
pub fn versao_compativel(v: u32) -> Result<(), String> {
    if v > YW1_API_VERSAO {
        Err(format!("o mod pede a API {v}, mas este jogo tem a {YW1_API_VERSAO} (atualize o jogo)"))
    } else if v < YW1_API_VERSAO_MINIMA {
        Err(format!("o mod usa a API {v}, que este jogo não aceita mais (mínima {YW1_API_VERSAO_MINIMA}; recompile o mod)"))
    } else {
        Ok(())
    }
}

// ------------------------------------------------------------------------------------------------ invólucro Rust

/// Tipo de gancho no invólucro.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tipo {
    Antes,
    Substituir,
    Depois,
}

/// A API vista pelo mod (cópia do ponteiro que o motor passou; vale até o fim do processo).
#[derive(Clone, Copy)]
pub struct Api {
    raw: &'static Yw1Api,
}

static API: std::sync::OnceLock<Api> = std::sync::OnceLock::new();

impl Api {
    /// # Safety
    /// `p` tem de ser o ponteiro recebido em `yw1_mod_init` (válido até o fim do processo).
    pub unsafe fn de_ponteiro(p: *const Yw1Api) -> Option<Api> {
        // SAFETY: garantido pelo chamador
        let raw = unsafe { p.as_ref()? };
        Some(Api { raw })
    }

    /// A API registrada pelo [`yw1_mod!`] (para callbacks que não recebem um [`Ctx`]).
    pub fn global() -> Option<Api> {
        API.get().copied()
    }

    pub fn crua(&self) -> &'static Yw1Api {
        self.raw
    }

    pub fn log(&self, msg: &str) {
        self.log_nivel(LOG_INFO, msg)
    }

    pub fn log_nivel(&self, nivel: u32, msg: &str) {
        let c = CString::new(msg.replace('\0', " ")).unwrap_or_default();
        (self.raw.log)(nivel, c.as_ptr())
    }

    pub fn vblank(&self) -> u64 {
        (self.raw.vblank)()
    }

    /// Registra um gancho na entrada de `va`. O callback roda na thread guest; um pânico dentro dele é contido
    /// (vira uma linha de erro no log). Devolve o id.
    pub fn registrar<F>(&self, va: u32, tipo: Tipo, prioridade: i32, nome: &str, f: F) -> Result<u64, ()>
    where
        F: Fn(&mut Ctx) + Send + Sync + 'static,
    {
        let t = match tipo {
            Tipo::Antes => GANCHO_ANTES,
            Tipo::Substituir => GANCHO_SUBSTITUIR,
            Tipo::Depois => GANCHO_DEPOIS,
        };
        let caixa: Box<Box<dyn Fn(&mut Ctx) + Send + Sync>> = Box::new(Box::new(f));
        let user = Box::into_raw(caixa) as *mut c_void;
        let n = CString::new(nome.replace('\0', " ")).unwrap_or_default();
        let id = (self.raw.registrar_gancho)(va, t, prioridade, n.as_ptr(), trampolim, user);
        if id == 0 {
            // SAFETY: o motor recusou; ninguém mais tem o ponteiro
            drop(unsafe { Box::from_raw(user as *mut Box<dyn Fn(&mut Ctx) + Send + Sync>) });
            return Err(());
        }
        // (o callback fica vivo enquanto o processo durar, mesmo depois de remover: uma chamada em andamento pode
        // ainda estar usando)
        Ok(id)
    }

    pub fn remover(&self, id: u64) -> bool {
        (self.raw.remover_gancho)(id)
    }

    /// Lê `buf.len()` bytes do jogo; false se a região não está toda mapeada.
    pub fn ler_mem(&self, va: u32, buf: &mut [u8]) -> bool {
        (self.raw.ler_mem)(va, buf.as_mut_ptr(), buf.len())
    }

    pub fn escrever_mem(&self, va: u32, dados: &[u8]) -> bool {
        (self.raw.escrever_mem)(va, dados.as_ptr(), dados.len())
    }

    pub fn ler_u32(&self, va: u32) -> Option<u32> {
        let mut b = [0u8; 4];
        self.ler_mem(va, &mut b).then(|| u32::from_le_bytes(b))
    }

    /// Texto C (até `max` bytes) na memória do jogo; bytes fora de ASCII imprimível viram '.'.
    pub fn ler_cstr(&self, mut va: u32, max: usize) -> String {
        let mut s = String::new();
        let mut b = [0u8; 1];
        while s.len() < max && self.ler_mem(va, &mut b) && b[0] != 0 {
            s.push(if (0x20..0x7f).contains(&b[0]) { b[0] as char } else { '.' });
            va = va.wrapping_add(1);
        }
        s
    }

    pub fn substituir_arquivo(&self, caminho: &str, dados: &[u8]) -> bool {
        let Ok(c) = CString::new(caminho) else { return false };
        (self.raw.substituir_arquivo)(c.as_ptr(), dados.as_ptr(), dados.len())
    }
}

/// Contexto de um gancho em execução (só existe dentro do callback).
pub struct Ctx {
    api: Api,
    raw: *mut Yw1Ctx,
}

impl Ctx {
    pub fn api(&self) -> Api {
        self.api
    }

    /// r0–r14; 15 = endereço da função do gancho.
    pub fn reg(&self, i: u32) -> u32 {
        (self.api.raw.ler_reg)(self.raw, i)
    }

    pub fn set_reg(&mut self, i: u32, v: u32) {
        (self.api.raw.escrever_reg)(self.raw, i, v)
    }

    /// Num gancho "depois": r0–r3 (0–3), sp (4) e lr (5) da entrada.
    pub fn entrada(&self, i: u32) -> u32 {
        (self.api.raw.ler_entrada)(self.raw, i)
    }

    /// Só em [`Tipo::Substituir`]: roda a função original.
    pub fn chamar_original(&mut self) -> bool {
        (self.api.raw.chamar_original)(self.raw)
    }

    /// Chama a função do jogo `va` com estes argumentos; devolve (r0, r1).
    pub fn chamar(&mut self, va: u32, args: &[u32]) -> (u32, u32) {
        let v = (self.api.raw.chamar_funcao)(self.raw, va, args.as_ptr(), args.len());
        (v as u32, (v >> 32) as u32)
    }

    pub fn ler_cstr(&self, va: u32, max: usize) -> String {
        self.api.ler_cstr(va, max)
    }
}

extern "C" fn trampolim(ctx: *mut Yw1Ctx, user: *mut c_void) {
    let Some(api) = Api::global() else { return };
    // SAFETY: `user` é o Box criado em `registrar`, que nunca é liberado depois de aceito
    let f = unsafe { &*(user as *const Box<dyn Fn(&mut Ctx) + Send + Sync>) };
    let mut c = Ctx { api, raw: ctx };
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&mut c))).is_err() {
        api.log_nivel(LOG_ERRO, "pânico dentro de um gancho do mod (contido)");
    }
}

/// Usado pelo [`yw1_mod!`]: confere a versão, guarda a API e chama `f` contendo pânicos.
///
/// # Safety
/// `api` é o ponteiro que o motor passou ao `yw1_mod_init`.
pub unsafe fn iniciar(api: *const Yw1Api, versao_motor: u32, f: fn(&Api) -> bool) -> bool {
    // SAFETY: garantido pelo chamador
    let Some(a) = (unsafe { Api::de_ponteiro(api) }) else { return false };
    // o motor tem de ter ao menos os campos que este mod conhece
    if versao_motor < YW1_API_VERSAO || (a.raw.tamanho as usize) < std::mem::size_of::<Yw1Api>() {
        a.log_nivel(LOG_ERRO, &format!("este mod precisa da API {YW1_API_VERSAO}; o jogo tem a {versao_motor}"));
        return false;
    }
    let _ = API.set(a);
    std::panic::catch_unwind(|| f(&a)).unwrap_or_else(|_| {
        a.log_nivel(LOG_ERRO, "pânico no yw1_mod_init (contido)");
        false
    })
}

/// Exporta `yw1_mod_api_versao` e `yw1_mod_init` chamando `fn iniciar(api: &Api) -> bool`.
#[macro_export]
macro_rules! yw1_mod {
    ($init:path) => {
        #[no_mangle]
        pub extern "C" fn yw1_mod_api_versao() -> u32 {
            $crate::YW1_API_VERSAO
        }
        /// # Safety
        /// Chamado só pelo motor, com a tabela da API.
        #[no_mangle]
        pub unsafe extern "C" fn yw1_mod_init(api: *const $crate::Yw1Api, versao: u32) -> bool {
            // SAFETY: o motor passa a própria tabela
            unsafe { $crate::iniciar(api, versao, $init) }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versoes() {
        assert!(versao_compativel(YW1_API_VERSAO).is_ok());
        assert!(versao_compativel(YW1_API_VERSAO + 1).is_err());
        assert!(versao_compativel(0).is_err());
    }
}
