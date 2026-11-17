//! Mods do jogo recompilado (fase G2).
//!
//! - Pasta: `%APPDATA%/yw1/mods/<nome>/` (`YW_MODS_DIR` troca a pasta; `YW_MODS=0` desliga tudo), cada mod com um
//!   `mod.toml` (nome, versao, autor, prioridade, descricao; `[codigo] dll = "…"` para mod de código).
//! - **Arquivos**: `romfs/<caminho do .fa>` substitui o arquivo do jogo com aquele caminho ([`arquivos`]).
//! - **Código**: a DLL (ABI C do crate `modapi/`) registra ganchos em funções do jogo ([`carregador`]). Desligado por
//!   padrão: é código nativo com confiança total, e só vale depois de ligado no menu.
//! - Configuração (ordem, desligados, código ligado) em `settings.toml` `[mods]`, pela aba "Mods" do menu; vale ao
//!   reiniciar. [`iniciar`] roda no `boot`, antes da primeira instrução guest.

pub mod arquivos;
pub mod carregador;
pub mod indice;

use super::settings;
use indice::{codigo_ligado, ligado, ordem_efetiva, Indice, ModDesc};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// Pasta dos mods: `YW_MODS_DIR`, senão `%APPDATA%/yw1/mods` (sem APPDATA: `./mods`).
pub fn pasta() -> PathBuf {
    if let Some(p) = std::env::var_os("YW_MODS_DIR") {
        return PathBuf::from(p);
    }
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("yw1")).unwrap_or_else(|| PathBuf::from(".")).join("mods")
}

/// `YW_MODS=0`: nenhum mod (nem de arquivos nem de código).
pub fn desligados_pelo_ambiente() -> bool {
    std::env::var("YW_MODS").is_ok_and(|v| v == "0")
}

/// Situação de um mod nesta execução (para o menu).
#[derive(Clone, Debug)]
pub struct Situacao {
    pub desc: ModDesc,
    /// arquivos que valem (os que nenhum mod antes dele já trocou)
    pub arquivos_valendo: usize,
    /// código: None = não pedido; Some(Ok(n ganchos)) / Some(Err(motivo))
    pub codigo: Option<Result<usize, String>>,
}

/// O que o boot encontrou e fez (a aba "Mods" mostra; mudanças no menu valem ao reiniciar).
#[derive(Clone, Debug, Default)]
pub struct Estado {
    pub pasta: PathBuf,
    pub desligado_pelo_ambiente: bool,
    pub mods: Vec<Situacao>,
    /// arquivos trocados no total
    pub arquivos: usize,
}

static ESTADO: OnceLock<Mutex<Estado>> = OnceLock::new();

/// Cópia do estado do boot (vazio antes do boot).
pub fn estado() -> Estado {
    ESTADO.get().map(|e| e.lock().unwrap_or_else(|x| x.into_inner()).clone()).unwrap_or_default()
}

/// Lê de novo a pasta (botão "procurar de novo" do menu): só a lista; nada passa a valer antes de reiniciar.
pub fn descobrir_de_novo() -> Vec<ModDesc> {
    indice::descobrir(&pasta())
}

/// No boot: descobre os mods, monta o índice de arquivos e liga os ganchos, carrega as DLLs ligadas.
pub fn iniciar() {
    let cfg = settings::load().mods;
    let pasta = pasta();
    let mut est = Estado { pasta: pasta.clone(), desligado_pelo_ambiente: desligados_pelo_ambiente(), ..Default::default() };
    if est.desligado_pelo_ambiente {
        eprintln!("[mods] desligados (YW_MODS=0)");
        let _ = ESTADO.set(Mutex::new(est));
        return;
    }
    let mods = indice::descobrir(&pasta);
    if mods.is_empty() {
        eprintln!("[mods] nenhum mod em {}", pasta.display());
        let _ = ESTADO.set(Mutex::new(est));
        return;
    }
    // `romfs/<caminho>` que existe na RomFS do jogo (yw1_a.fa, mov/…, snd/…) troca o arquivo INTEIRO (LayeredFS, RomFS
    // virtual em services/romfs.rs); o resto é um arquivo de dentro do .fa (gancho de abertura, `arquivos.rs`)
    let na_romfs: std::collections::HashSet<String> = match crate::hle::services::fs::listar_romfs() {
        Ok(v) => v.into_iter().map(|e| e.caminho).collect(),
        Err(e) => {
            eprintln!("[mods] não consegui ler a RomFS do jogo ({e}): só arquivos de dentro do .fa");
            Default::default()
        }
    };
    let so = |de_romfs: bool| -> Vec<ModDesc> {
        mods.iter()
            .map(|m| {
                let mut m = m.clone();
                m.arquivos.retain(|(c, _)| na_romfs.contains(c) == de_romfs);
                m
            })
            .collect()
    };
    let ix_rom = Indice::construir(&so(true), &cfg);
    let ix = Indice::construir(&so(false), &cfg);
    for (c, venceu, perdeu) in ix.conflitos.iter().chain(&ix_rom.conflitos) {
        eprintln!("[mods] {c}: vale o de \"{venceu}\" (\"{perdeu}\" também troca)");
    }
    let mut valendo = vec![0usize; mods.len()];
    for (i, _) in ix.mapa.values().chain(ix_rom.mapa.values()) {
        valendo[*i] += 1;
    }
    if !ix_rom.is_empty() {
        let mut trocas: Vec<(String, PathBuf)> = ix_rom.mapa.iter().map(|(c, (_, p))| (c.clone(), p.clone())).collect();
        trocas.sort();
        match crate::hle::services::fs::preparar_camadas(&trocas) {
            Ok(t) => {
                for (c, antes, depois) in t {
                    eprintln!("[mods] RomFS: {c} trocado ({antes} → {depois} bytes)");
                }
            }
            Err(e) => eprintln!("[mods] RomFS virtual falhou ({e}): arquivos inteiros da RomFS NÃO trocados"),
        }
    }
    est.arquivos = ix.len() + ix_rom.len();
    let ordem = ordem_efetiva(&mods, &cfg);
    let nomes: Vec<String> = mods.iter().map(|m| m.nome.clone()).collect();
    arquivos::instalar(ix, nomes);
    let mut codigo: Vec<Option<Result<usize, String>>> = vec![None; mods.len()];
    for &i in &ordem {
        let m = &mods[i];
        let estado = if let Some(e) = &m.erro {
            format!("ignorado: {e}")
        } else if !ligado(m, &cfg) {
            "desligado".into()
        } else {
            format!("ligado, {} de {} arquivos valendo", valendo[i], m.arquivos.len())
        };
        eprintln!("[mods] {} \"{}\" {} ({}): {estado}", m.id, m.nome, m.versao, m.dir.display());
        for a in &m.avisos {
            eprintln!("[mods]   aviso: {a}");
        }
        if codigo_ligado(m, &cfg) {
            let dll = m.dll.clone().unwrap_or_default();
            let r = carregador::carregar(&m.id, &dll);
            match &r {
                Ok(n) => eprintln!("[mods]   código carregado: {} ({n} ganchos)", dll.display()),
                Err(e) => eprintln!("[mods]   código RECUSADO: {e}"),
            }
            codigo[i] = Some(r);
        } else if m.dll.is_some() && ligado(m, &cfg) {
            eprintln!("[mods]   tem código (DLL), desligado: ligue no menu (Esc/F1 → Configurações → Mods)");
        }
    }
    est.mods = ordem
        .iter()
        .map(|&i| Situacao { desc: mods[i].clone(), arquivos_valendo: valendo[i], codigo: codigo[i].clone() })
        .collect();
    let _ = ESTADO.set(Mutex::new(est));
}

/// Resumo do fim da rodada (`game.log`).
pub fn log_resumo() {
    let n = arquivos::TROCAS.load(std::sync::atomic::Ordering::Relaxed);
    if n > 0 {
        eprintln!("[mods] {n} aberturas de arquivo atendidas por mods");
    }
}
