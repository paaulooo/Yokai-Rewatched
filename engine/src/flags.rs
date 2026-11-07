//! Flags de progresso e variáveis globais do jogo (o que os scripts de mapa, fase e evento gravam e consultam).
//!
//! **No jogo original** (lido no export do Ghidra e nos dados):
//! - As flags "principais" (tipo 0) são um mapa de **2.560 bits** (`0xA00`) no objeto de save: `op14000(chave, v = 1)` liga ou
//!   desliga (`FUN_003ea038` → `FUN_002ae46c`, que acha a entrada da chave pelo hash em `FUN_0031a1b8` e mexe no bit);
//!   `op14001(chave)` lê (`FUN_003e8754` → `FUN_002ca4d8`, a mesma que testa `time_lock`).
//! - A tabela vem de `data/res/sys/flag_config_0.01.cfg.bin`: seções `FLAG_INFO_BEGIN(tipo, n)` e registros
//!   `FLAG_INFO(bit, hash do nome)`. **Cada tipo tem a sua numeração de bits** (recomeça em 0).
//! - Tipo de cada opcode, medido nos scripts (versão mais recente): `14000`/`14001` → tipo 0 (971 e 116 chamadas);
//!   `14011`/`14012` → tipo 6 (`14012` devolve um **byte**); `14003` → tipos 7 e 8; `14005`/`14006` → tipo 8.
//! - `flag_config.cfg.bin` (sem versão) é OUTRA tabela: dos 85 hashes que aparecem nas duas, 76 mudam de bit, e 651 flags só
//!   da antiga caem em células já usadas por outras na nova. Elas não se fundem; por isso o port não usa bitmap.
//!
//! **No port**: tudo é guardado **por hash** (sem sobreposição possível). A tabela só informa o tipo: tipo 0 = liga/desliga
//! (grava 0 ou 1), tipo 6 = byte (`& 0xff`), o resto = inteiro. Chave fora da tabela = variável inteira. [H] os opcodes
//! `14003`, `14004`, `14005`, `14006`, `14011` e `14012` como gravar/ler genéricos: o tipo vem dos dados, mas o código
//! deles não foi lido.
//!
//! O estado é global (como o relógio em `clock.rs`): menus, eventos e mapas compartilham as mesmas flags.

use crate::assets::arc0::Archive;
use crate::assets::cfgbin::{self, Param};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, OnceLock};

static STATE: Mutex<BTreeMap<i32, i32>> = Mutex::new(BTreeMap::new());

/// hash do nome → tipo (a mais nova, `_0.01`, tem prioridade sobre a antiga).
static TYPES: OnceLock<HashMap<i32, i32>> = OnceLock::new();

/// Lê as tabelas de flags (idempotente). Sem elas, toda chave vira variável inteira.
pub fn init(fa: &Archive) {
    TYPES.get_or_init(|| {
        let mut t: HashMap<i32, i32> = HashMap::new();
        // a antiga primeiro; a nova sobrescreve
        for path in [
            "data/res/sys/flag_config.cfg.bin",
            "data/res/sys/flag_config_0.01.cfg.bin",
        ] {
            let Ok(b) = fa.read(path) else { continue };
            let mut typ = 0;
            for e in cfgbin::parse(&b).unwrap_or_default() {
                match (e.name.as_str(), e.params.first(), e.params.get(1)) {
                    ("FLAG_INFO_BEGIN", Some(Param::Int(ty)), _) => typ = *ty,
                    ("FLAG_INFO", _, Some(Param::Int(hash))) => {
                        t.insert(*hash, typ);
                    }
                    _ => {}
                }
            }
        }
        t
    });
}

/// Tipo da flag segundo as tabelas do jogo (None = não definida).
pub fn kind_of(key: i32) -> Option<i32> {
    TYPES.get().and_then(|t| t.get(&key)).copied()
}

pub fn set(key: i32, v: i32) {
    let v = match kind_of(key) {
        Some(0) => i32::from(v != 0),
        Some(6) => v & 0xff,
        _ => v,
    };
    let mut st = STATE.lock().unwrap();
    if v == 0 {
        st.remove(&key);
    } else {
        st.insert(key, v);
    }
}

/// Valor da flag (0/1 no tipo 0) ou da variável.
pub fn get(key: i32) -> i32 {
    STATE.lock().unwrap().get(&key).copied().unwrap_or(0)
}

/// Novo jogo: tudo desligado.
pub fn reset() {
    STATE.lock().unwrap().clear();
}

/// Estado para o save: só o que não é zero.
pub fn snapshot() -> Vec<(i32, i32)> {
    STATE
        .lock()
        .unwrap()
        .iter()
        .map(|(k, v)| (*k, *v))
        .collect()
}

pub fn restore(items: &[(i32, i32)]) {
    let mut st = STATE.lock().unwrap();
    st.clear();
    st.extend(items.iter().copied().filter(|(_, v)| *v != 0));
}

#[cfg(test)]
pub fn count_set() -> usize {
    STATE.lock().unwrap().len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::crc32;

    fn key(n: &str) -> i32 {
        crc32(n.as_bytes()) as i32
    }

    #[test]
    fn flags_follow_the_game_tables() {
        let romfs = crate::romfs::locate(None).expect("RomFS");
        let fa = Archive::open(&romfs.join("yw1_a.fa")).unwrap();
        init(&fa);
        reset();
        // `time_lock` e `hlp_050` estão na tabela nova (tipo 0); `dis_areamap` só na antiga
        assert_eq!(kind_of(key("time_lock")), Some(0));
        assert_eq!(kind_of(key("hlp_050")), Some(0));
        assert!(kind_of(key("dis_areamap")).is_some());
        // tipo 0 só guarda 0 ou 1
        set(key("hlp_050"), 7);
        assert_eq!(get(key("hlp_050")), 1);
        assert_eq!(get(key("time_lock")), 0, "uma flag não mexe em outra");
        set(key("hlp_050"), 0);
        assert_eq!(get(key("hlp_050")), 0);
        // chave fora das tabelas = variável inteira
        let v = key("flags_test_variavel_inexistente");
        assert_eq!(kind_of(v), None);
        set(v, 42);
        assert_eq!(get(v), 42);
        // snapshot e restore
        set(key("time_lock"), 1);
        let snap = snapshot();
        reset();
        assert_eq!((get(key("time_lock")), get(v)), (0, 0));
        restore(&snap);
        assert_eq!((get(key("time_lock")), get(v)), (1, 42));
        assert_eq!(count_set(), 2);
        reset();
    }

    /// As duas tabelas existem e a nova tem os tipos esperados (medido: 0, 1, 6, 7, 8).
    #[test]
    fn tables_have_the_measured_kinds() {
        let romfs = crate::romfs::locate(None).expect("RomFS");
        let fa = Archive::open(&romfs.join("yw1_a.fa")).unwrap();
        init(&fa);
        let t = TYPES.get().unwrap();
        let kinds: std::collections::BTreeSet<i32> = t.values().copied().collect();
        for k in [0, 1, 6, 7, 8] {
            assert!(kinds.contains(&k), "tipo {k} ausente: {kinds:?}");
        }
        assert!(t.len() > 3000, "flags lidas: {}", t.len());
    }
}
