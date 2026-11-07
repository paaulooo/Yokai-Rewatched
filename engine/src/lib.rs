//! Biblioteca do runtime nativo por recompilação estática: HLE do 3DS (`hle`) e front-end do PICA (`pica`).
//! O executável final é gerado em `work/recomp/app` (liga esta biblioteca com o código recompilado do usuário).
//! O executável `yw1` deste crate (src/main.rs) é o motor manual antigo, congelado.

pub mod hle;
pub mod pica;
pub mod render;

/// Nos testes da biblioteca não há código recompilado: o despacho do runtime só precisa existir para linkar.
#[cfg(test)]
#[no_mangle]
pub fn yw1_dispatch(_c: &mut yw1_rt::Cpu, va: u32, _ret: u32) {
    panic!("[teste] chamada guest {va:#x} sem código recompilado")
}
