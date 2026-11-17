//! Descoberta dos mods na pasta, leitura do `mod.toml`, ordem efetiva e índice "caminho do .fa → arquivo do mod".
//! Tudo aqui é puro (só lê arquivos do PC): os testes rodam sem o jogo.

use crate::hle::settings;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

/// Um mod achado na pasta de mods (uma subpasta com `mod.toml`).
#[derive(Clone, Debug, Default)]
pub struct ModDesc {
    /// nome da pasta: é o que identifica o mod nas configurações
    pub id: String,
    pub dir: PathBuf,
    pub nome: String,
    pub versao: String,
    pub autor: String,
    pub descricao: String,
    /// maior = vence (quando o menu não escolheu uma ordem)
    pub prioridade: i32,
    /// (caminho normalizado do .fa, arquivo no PC) de tudo que está em `romfs/`
    pub arquivos: Vec<(String, PathBuf)>,
    /// DLL do mod de código (absoluta, dentro de `dir`)
    pub dll: Option<PathBuf>,
    /// problema que impede o mod de valer (mod.toml ausente/inválido, DLL fora da pasta…)
    pub erro: Option<String>,
    /// avisos que não impedem (arquivos com caminho inválido ignorados…)
    pub avisos: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct ModToml {
    nome: Option<String>,
    versao: Option<String>,
    autor: Option<String>,
    descricao: Option<String>,
    prioridade: Option<i32>,
    codigo: Option<Codigo>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Codigo {
    dll: String,
}

/// Caminho do jeito que o índice guarda: barras normais, sem `rom:/` nem `/` no início, sem `.`/vazios, minúsculas
/// (o .fa não tem dois arquivos que só diferem na caixa — conferido nos 20 835 de `data/romfs/yw1_a_files.csv`, e o
/// Windows não distingue). `..` ou caminho vazio → `None`.
pub fn normalizar(p: &str) -> Option<String> {
    let mut s = p.trim().replace('\\', "/");
    if s.len() >= 5 && s[..5].eq_ignore_ascii_case("rom:/") {
        s.drain(..5);
    }
    let mut partes = Vec::new();
    for c in s.split('/') {
        match c {
            "" | "." => {}
            ".." => return None,
            c => partes.push(c.to_ascii_lowercase()),
        }
    }
    (!partes.is_empty()).then(|| partes.join("/"))
}

/// Lê uma pasta de mod. Nunca falha: problemas vão para `erro`/`avisos`.
pub fn ler_mod(dir: &Path) -> ModDesc {
    let id = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut m = ModDesc { id: id.clone(), dir: dir.to_path_buf(), nome: id, ..Default::default() };
    let toml_p = dir.join("mod.toml");
    if dir.join("exefs").exists() {
        m.avisos.push("exefs/ (patches do code.bin) não vale aqui: o código do jogo é recompilado; use um mod de código".into());
    }
    let texto = match std::fs::read_to_string(&toml_p) {
        Ok(t) => t,
        // mod no formato LayeredFS (Luma3DS/Azahar: só `romfs/`, sem mod.toml): vale com o nome da pasta
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && dir.join("romfs").is_dir() => {
            m.avisos.push("sem mod.toml: formato LayeredFS, nome = pasta".into());
            String::new()
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            m.erro = Some("sem mod.toml (o mod é ignorado)".into());
            return m;
        }
        Err(e) => {
            m.erro = Some(format!("não consegui ler o mod.toml: {e}"));
            return m;
        }
    };
    let t: ModToml = match toml::from_str(&texto) {
        Ok(t) => t,
        Err(e) => {
            m.erro = Some(format!("mod.toml inválido: {}", e.to_string().trim()));
            return m;
        }
    };
    if let Some(n) = t.nome.filter(|n| !n.trim().is_empty()) {
        m.nome = n;
    }
    m.versao = t.versao.unwrap_or_default();
    m.autor = t.autor.unwrap_or_default();
    m.descricao = t.descricao.unwrap_or_default();
    m.prioridade = t.prioridade.unwrap_or(0);
    if let Some(c) = t.codigo {
        // a DLL tem de estar DENTRO da pasta do mod (nada de caminho absoluto ou `..`)
        let rel = Path::new(&c.dll);
        if c.dll.trim().is_empty() || rel.components().any(|k| !matches!(k, Component::Normal(_))) {
            m.erro = Some(format!("[codigo] dll = \"{}\": tem de ser um arquivo dentro da pasta do mod", c.dll));
        } else {
            let p = dir.join(rel);
            m.dll = Some(std::path::absolute(&p).unwrap_or(p));
        }
    }
    let romfs = dir.join("romfs");
    if romfs.is_dir() {
        let mut pilha = vec![romfs.clone()];
        while let Some(d) = pilha.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    pilha.push(p);
                    continue;
                }
                let rel = p.strip_prefix(&romfs).unwrap_or(&p).to_string_lossy().into_owned();
                match normalizar(&rel) {
                    Some(n) => m.arquivos.push((n, p)),
                    None => m.avisos.push(format!("caminho ignorado: {rel}")),
                }
            }
        }
        m.arquivos.sort();
    }
    m
}

/// Todas as subpastas de `pasta` (em ordem alfabética). Pasta inexistente = nenhum mod.
pub fn descobrir(pasta: &Path) -> Vec<ModDesc> {
    let Ok(rd) = std::fs::read_dir(pasta) else { return Vec::new() };
    let mut dirs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    dirs.iter().map(|d| ler_mod(d)).collect()
}

/// O mod vale (não desligado no menu e sem erro)?
pub fn ligado(m: &ModDesc, cfg: &settings::Mods) -> bool {
    m.erro.is_none() && !cfg.desligados.iter().any(|d| d == &m.id)
}

/// O código (DLL) do mod deve ser carregado? Exige o mod ligado E o código ligado explicitamente.
pub fn codigo_ligado(m: &ModDesc, cfg: &settings::Mods) -> bool {
    ligado(m, cfg) && m.dll.is_some() && cfg.codigo_ligado.iter().any(|d| d == &m.id)
}

/// Índices de `mods` na ordem em que valem (o primeiro vence): os listados em `cfg.ordem`, nessa ordem; depois os
/// outros por prioridade (maior primeiro) e nome da pasta.
pub fn ordem_efetiva(mods: &[ModDesc], cfg: &settings::Mods) -> Vec<usize> {
    let mut v: Vec<usize> = Vec::with_capacity(mods.len());
    for id in &cfg.ordem {
        if let Some(i) = mods.iter().position(|m| &m.id == id) {
            if !v.contains(&i) {
                v.push(i);
            }
        }
    }
    let mut resto: Vec<usize> = (0..mods.len()).filter(|i| !v.contains(i)).collect();
    resto.sort_by(|&a, &b| mods[b].prioridade.cmp(&mods[a].prioridade).then_with(|| mods[a].id.cmp(&mods[b].id)));
    v.extend(resto);
    v
}

/// Caminho normalizado → (índice do mod em `mods`, arquivo no PC).
#[derive(Clone, Debug, Default)]
pub struct Indice {
    pub mapa: HashMap<String, (usize, PathBuf)>,
    /// (caminho, mod que venceu, mod que perdeu) — para o log
    pub conflitos: Vec<(String, String, String)>,
}

impl Indice {
    pub fn construir(mods: &[ModDesc], cfg: &settings::Mods) -> Indice {
        let mut ix = Indice::default();
        for i in ordem_efetiva(mods, cfg) {
            let m = &mods[i];
            if !ligado(m, cfg) {
                continue;
            }
            for (c, p) in &m.arquivos {
                match ix.mapa.get(c) {
                    Some(&(j, _)) => ix.conflitos.push((c.clone(), mods[j].id.clone(), m.id.clone())),
                    None => {
                        ix.mapa.insert(c.clone(), (i, p.clone()));
                    }
                }
            }
        }
        ix
    }

    /// Arquivo do mod para o caminho que o jogo pediu (qualquer forma: com `rom:/`, barras invertidas, caixa…).
    pub fn procurar(&self, caminho: &str) -> Option<&(usize, PathBuf)> {
        self.mapa.get(&normalizar(caminho)?)
    }

    pub fn len(&self) -> usize {
        self.mapa.len()
    }

    pub fn is_empty(&self) -> bool {
        self.mapa.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(nome: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("yw1_mods_teste_{nome}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn mod_em(raiz: &Path, id: &str, toml: Option<&str>, arquivos: &[(&str, &[u8])]) {
        let d = raiz.join(id);
        std::fs::create_dir_all(&d).unwrap();
        if let Some(t) = toml {
            std::fs::write(d.join("mod.toml"), t).unwrap();
        }
        for (p, b) in arquivos {
            let f = d.join("romfs").join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, b).unwrap();
        }
    }

    #[test]
    fn mods_normalizar_caminho() {
        assert_eq!(normalizar("data/menu/win_en.xa").as_deref(), Some("data/menu/win_en.xa"));
        assert_eq!(normalizar("rom:/data/menu/win_en.xa").as_deref(), Some("data/menu/win_en.xa"));
        assert_eq!(normalizar("ROM:/Seq\\Menu\\TitleMenu.xq").as_deref(), Some("seq/menu/titlemenu.xq"));
        assert_eq!(normalizar("/./data//x.bin").as_deref(), Some("data/x.bin"));
        assert_eq!(normalizar("data/../x"), None);
        assert_eq!(normalizar(""), None);
        assert_eq!(normalizar("rom:/"), None);
    }

    #[test]
    fn mods_prioridade_e_ordem_do_menu() {
        let r = tmp("prio");
        mod_em(&r, "a_baixa", Some("prioridade = 1"), &[("data/x.bin", b"A"), ("data/so_a.bin", b"a")]);
        mod_em(&r, "b_alta", Some("prioridade = 10\nnome = \"Alta\""), &[("Data/X.bin", b"B")]);
        let mods = descobrir(&r);
        assert_eq!(mods.len(), 2);
        assert_eq!(mods[1].nome, "Alta");
        // sem ordem do menu: maior prioridade vence
        let cfg = settings::Mods::default();
        let ix = Indice::construir(&mods, &cfg);
        let (i, p) = ix.procurar("rom:/data/x.bin").unwrap();
        assert_eq!(mods[*i].id, "b_alta");
        assert_eq!(std::fs::read(p).unwrap(), b"B");
        assert_eq!(ix.conflitos.len(), 1);
        assert!(ix.procurar("data/so_a.bin").is_some());
        // ordem escolhida no menu manda sobre a prioridade
        let cfg = settings::Mods { ordem: vec!["a_baixa".into()], ..Default::default() };
        assert_eq!(ordem_efetiva(&mods, &cfg), vec![0, 1]);
        let ix = Indice::construir(&mods, &cfg);
        assert_eq!(mods[ix.procurar("data/x.bin").unwrap().0].id, "a_baixa");
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn mods_desligado_e_com_erro_nao_entram() {
        let r = tmp("deslig");
        mod_em(&r, "bom", Some("nome = \"Bom\""), &[("data/x.bin", b"1")]);
        // sem mod.toml mas com romfs/ = formato LayeredFS (Luma3DS/Azahar): vale, com o nome da pasta
        mod_em(&r, "layeredfs", None, &[("data/y.bin", b"2")]);
        // sem mod.toml e sem romfs/: ignorado
        std::fs::create_dir_all(r.join("vazio_sem_toml")).unwrap();
        mod_em(&r, "toml_ruim", Some("prioridade = \"alta\""), &[("data/z.bin", b"3")]);
        mod_em(&r, "dll_fora", Some("[codigo]\ndll = \"../fora.dll\""), &[]);
        let mods = descobrir(&r);
        let por_id = |id: &str| mods.iter().find(|m| m.id == id).unwrap();
        assert!(por_id("layeredfs").erro.is_none());
        assert_eq!(por_id("layeredfs").nome, "layeredfs");
        assert!(por_id("vazio_sem_toml").erro.is_some());
        assert!(por_id("toml_ruim").erro.is_some());
        assert!(por_id("dll_fora").erro.is_some());
        let ix = Indice::construir(&mods, &settings::Mods::default());
        assert!(ix.procurar("data/x.bin").is_some());
        assert!(ix.procurar("data/y.bin").is_some());
        assert!(ix.procurar("data/z.bin").is_none());
        let cfg = settings::Mods { desligados: vec!["bom".into(), "layeredfs".into()], ..Default::default() };
        assert!(Indice::construir(&mods, &cfg).is_empty());
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn mods_codigo_desligado_por_padrao() {
        let r = tmp("codigo");
        mod_em(&r, "cod", Some("[codigo]\ndll = \"m.dll\""), &[]);
        let mods = descobrir(&r);
        assert!(mods[0].erro.is_none(), "{:?}", mods[0].erro);
        assert!(mods[0].dll.as_ref().unwrap().ends_with("m.dll"));
        assert!(!codigo_ligado(&mods[0], &settings::Mods::default()));
        let cfg = settings::Mods { codigo_ligado: vec!["cod".into()], ..Default::default() };
        assert!(codigo_ligado(&mods[0], &cfg));
        // desligar o mod desliga o código também
        let cfg = settings::Mods { codigo_ligado: vec!["cod".into()], desligados: vec!["cod".into()], ..Default::default() };
        assert!(!codigo_ligado(&mods[0], &cfg));
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn mods_configuracao_no_settings_toml() {
        let s = settings::Settings::from_toml(
            "[mods]
ordem = [\"b\", \"a\"]
desligados = [\"c\"]
codigo_ligado = [\"a\"]
",
        )
        .unwrap();
        assert_eq!(s.mods.ordem, ["b", "a"]);
        assert_eq!(s.mods.desligados, ["c"]);
        assert_eq!(s.mods.codigo_ligado, ["a"]);
        // ida e volta, e arquivo antigo sem [mods] = padrão (nada ligado de código)
        assert_eq!(settings::Settings::from_toml(&s.to_toml()).unwrap().mods, s.mods);
        assert_eq!(settings::Settings::from_toml("[som]
volume = 50
").unwrap().mods, settings::Mods::default());
    }

    #[test]
    fn mods_pasta_inexistente() {
        assert!(descobrir(Path::new("Z:/nao/existe/yw1_mods")).is_empty());
    }
}
