//! Cobertura de formatos: tenta abrir CADA arquivo do RomFS com os decodificadores do motor Rust e grava
//! `work/notes/format_coverage.json` (lido por `tools/progress_metrics.py` para a página de progresso).
//! Só roda como teste ignorado: `cargo test --release format_coverage -- --ignored --nocapture`.

#[cfg(test)]
mod tests {
    use crate::assets::arc0::Archive;
    use crate::assets::xpck::Pack;
    use crate::assets::{cfgbin, imgc, xmpr};
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Tally {
        files: u64,
        bytes: u64,
        ok_files: u64,
        ok_bytes: u64,
        decoder: &'static str,
    }

    /// Decodifica um arquivo interno de um XPCK pela extensão. None = não há decodificador para ele.
    fn inner(name: &str, d: &[u8]) -> Option<bool> {
        let ext = name.rsplit('.').next().unwrap_or("");
        Some(match ext {
            "prm" => xmpr::parse(d).is_ok(),
            "xi" => imgc::decode(d).is_ok(),
            "bin" if name == "RES.bin" => crate::assets::chrc::Manifest::parse(d).is_ok(),
            // nós de esqueleto (168 B), animação e informação de clipe: lidos por character.rs
            "mbn" => d.len() >= 0x48,
            "mtn2" => d.starts_with(b"XMTN"),
            "mtninf" => d.len() >= 0x58,
            "cmr2" => true,
            _ => return None,
        })
    }

    #[test]
    #[ignore]
    fn format_coverage() {
        let romfs = crate::romfs::locate(None).expect("RomFS");
        let fa = Archive::open(&romfs.join("yw1_a.fa")).unwrap();
        let index = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../data/romfs/yw1_a_files.csv"),
        )
        .unwrap();
        let mut by_ext: BTreeMap<String, Tally> = BTreeMap::new();
        let mut inner_t: BTreeMap<String, Tally> = BTreeMap::new();
        for line in index.lines().skip(1) {
            let path = line.split(',').next().unwrap();
            let file = path.rsplit('/').next().unwrap();
            let mut ext = file.split_once('.').map_or("(sem)", |x| x.1).to_string();
            if ext.ends_with("cfg.bin") {
                ext = "cfg.bin".into();
            } else if ext.ends_with(".xq") {
                ext = "xq".into();
            }
            let Ok(d) = fa.read(path) else { continue };
            let (ok, decoder): (bool, &'static str) = match ext.as_str() {
                "cfg.bin" => (cfgbin::parse(&d).is_ok(), "cfgbin.rs"),
                "xq" => (crate::xq::Script::parse(path, &d).is_ok(), "xq.rs (VM)"),
                "xi" => (imgc::decode(&d).is_ok(), "imgc.rs"),
                "xa" => (crate::layout::Layout::load(&d).is_ok(), "layout.rs"),
                "xf" => (crate::assets::font::Font::parse(&d).is_ok(), "font.rs"),
                "bin" | "mapenv" => (cfgbin::parse(&d).is_ok(), "cfgbin.rs"),
                "xc" | "xr" | "xv" => match Pack::parse(&d) {
                    Ok(p) => {
                        let mut all = true;
                        for (n, b) in &p.files {
                            let iext = n.rsplit('.').next().unwrap_or("").to_string();
                            let t = inner_t.entry(iext).or_default();
                            t.files += 1;
                            t.bytes += b.len() as u64;
                            match inner(n, b) {
                                Some(true) => {
                                    t.ok_files += 1;
                                    t.ok_bytes += b.len() as u64;
                                }
                                Some(false) => all = false,
                                None => {}
                            }
                        }
                        (all, "xpck.rs + internos")
                    }
                    Err(_) => (false, "xpck.rs"),
                },
                _ => (false, "—"),
            };
            let t = by_ext.entry(ext).or_default();
            t.files += 1;
            t.bytes += d.len() as u64;
            t.decoder = decoder;
            if ok {
                t.ok_files += 1;
                t.ok_bytes += d.len() as u64;
            }
        }
        // fora do .fa: músicas (BCSTM), banco de efeitos (BCSAR) e vídeos (Moflex, via ffmpeg)
        for (dir, ext) in [("snd", "bcstm"), ("snd", "bcsar"), ("mov", "moflex")] {
            let mut stack = vec![romfs.join(dir)];
            while let Some(p) = stack.pop() {
                for e in std::fs::read_dir(&p).into_iter().flatten().flatten() {
                    let path = e.path();
                    if path.is_dir() {
                        stack.push(path);
                        continue;
                    }
                    if path.extension().and_then(|x| x.to_str()) != Some(ext) {
                        continue;
                    }
                    let len = e.metadata().map_or(0, |m| m.len());
                    let (ok, decoder) = match ext {
                        "bcstm" => (
                            std::fs::read(&path)
                                .is_ok_and(|d| crate::audio::bcstm::decode(&d).is_ok()),
                            "audio/bcstm.rs",
                        ),
                        "moflex" => (true, "ffmpeg (externo)"),
                        _ => (false, "—"),
                    };
                    let t = by_ext.entry(ext.to_string()).or_default();
                    t.files += 1;
                    t.bytes += len;
                    t.decoder = decoder;
                    if ok {
                        t.ok_files += 1;
                        t.ok_bytes += len;
                    }
                }
            }
        }
        let tot = |m: &BTreeMap<String, Tally>| {
            m.values().fold((0u64, 0u64, 0u64, 0u64), |a, t| {
                (
                    a.0 + t.files,
                    a.1 + t.ok_files,
                    a.2 + t.bytes,
                    a.3 + t.ok_bytes,
                )
            })
        };
        let (f, fo, b, bo) = tot(&by_ext);
        let (inf, info, inb, inbo) = tot(&inner_t);
        let row = |k: &String, t: &Tally| {
            format!(
                "    {{\"ext\": \"{k}\", \"files\": {}, \"ok_files\": {}, \"bytes\": {}, \"ok_bytes\": {}, \"decoder\": \"{}\"}}",
                t.files, t.ok_files, t.bytes, t.ok_bytes, t.decoder
            )
        };
        let json = format!(
            "{{\n  \"summary\": {{\"files\": {f}, \"ok_files\": {fo}, \"bytes\": {b}, \"ok_bytes\": {bo}, \"inner_files\": {inf}, \"inner_ok\": {info}, \"inner_bytes\": {inb}, \"inner_ok_bytes\": {inbo}}},\n  \"by_ext\": [\n{}\n  ],\n  \"inner\": [\n{}\n  ]\n}}\n",
            by_ext.iter().map(|(k, t)| row(k, t)).collect::<Vec<_>>().join(",\n"),
            inner_t.iter().map(|(k, t)| row(k, t)).collect::<Vec<_>>().join(",\n"),
        );
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../work/notes/format_coverage.json");
        std::fs::write(&out, &json).unwrap();
        println!("{json}");
    }

    /// Roda TODOS os scripts de evento (`seq/event/*.xq`) na VM com o host de eventos, sem janela, apertando A nas
    /// falas, e conta quantos terminam. Grava `work/notes/event_coverage.json`.
    #[test]
    #[ignore]
    fn event_coverage() {
        use crate::event_host::EventScene;
        use crate::input::{Button, Input};
        use std::rc::Rc;
        let romfs = crate::romfs::locate(None).expect("RomFS");
        let fa = Rc::new(Archive::open(&romfs.join("yw1_a.fa")).unwrap());
        let index = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../data/romfs/yw1_a_files.csv"),
        )
        .unwrap();
        let mut input = Input::headless();
        let mut per: BTreeMap<String, (u32, u32, u32)> = BTreeMap::new(); // grupo → (total, abre, termina)
        let (mut total, mut opens, mut done) = (0u32, 0u32, 0u32);
        let mut stuck = Vec::new();
        for line in index.lines().skip(1) {
            let path = line.split(',').next().unwrap();
            let Some(name) = path
                .strip_prefix("seq/event/")
                .and_then(|n| n.strip_suffix(".xq"))
            else {
                continue;
            };
            let group = name[..4.min(name.len())].to_string();
            let e = per.entry(group).or_default();
            e.0 += 1;
            total += 1;
            let Ok(mut ev) = EventScene::open(fa.clone(), name, None, 1) else {
                continue;
            };
            e.1 += 1;
            opens += 1;
            let mut t = 0u32;
            while t < 20000 && !ev.finished {
                input.set(Button::A, t % 20 == 19);
                if ev.tick(Some(&input)).is_err() {
                    break;
                }
                t += 1;
            }
            if ev.finished {
                e.2 += 1;
                done += 1;
            } else {
                stuck.push(name.to_string());
            }
        }
        let groups: Vec<String> = per
            .iter()
            .map(|(g, (t, o, d))| {
                format!(
                    "    {{\"group\": \"{g}\", \"total\": {t}, \"opens\": {o}, \"finishes\": {d}}}"
                )
            })
            .collect();
        let json = format!(
            "{{
  \"total\": {total}, \"opens\": {opens}, \"finishes\": {done},
  \"groups\": [
{}
  ],
  \"stuck\": {:?}
}}
",
            groups.join(
                ",
"
            ),
            stuck.iter().take(60).collect::<Vec<_>>()
        );
        std::fs::write(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../work/notes/event_coverage.json"),
            &json,
        )
        .unwrap();
        println!("{json}");
    }

    /// Tenta carregar cada mapa (`data/map/<id>/`) com `Room::load` e conta quantos abrem.
    #[test]
    #[ignore]
    fn map_coverage() {
        let romfs = crate::romfs::locate(None).expect("RomFS");
        let fa = Archive::open(&romfs.join("yw1_a.fa")).unwrap();
        let index = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../data/romfs/yw1_a_files.csv"),
        )
        .unwrap();
        let mut ids = std::collections::BTreeSet::new();
        for line in index.lines().skip(1) {
            let path = line.split(',').next().unwrap();
            if let Some(rest) = path
                .strip_prefix("data/res/map/")
                .or_else(|| path.strip_prefix("data/map/"))
            {
                if let Some(id) = rest.split('/').next() {
                    // id de mapa de verdade: letra + 3 dígitos + letra + 2 dígitos (t101i02, e102g001 tem 3)
                    let b = id.as_bytes();
                    let real = b.len() >= 7
                        && b[0].is_ascii_lowercase()
                        && b[1..4].iter().all(u8::is_ascii_digit)
                        && b[4].is_ascii_lowercase()
                        && b[5..].iter().all(u8::is_ascii_digit);
                    if real {
                        ids.insert(id.to_string());
                    }
                }
            }
        }
        let (mut ok, mut tris) = (0u32, 0usize);
        let mut bad = Vec::new();
        let mut kinds: BTreeMap<char, (u32, u32)> = BTreeMap::new();
        for id in &ids {
            let k = kinds.entry(id.chars().nth(4).unwrap_or('?')).or_default();
            k.0 += 1;
            match crate::room::Room::load(&fa, id) {
                Ok(r) => {
                    ok += 1;
                    k.1 += 1;
                    tris += r.triangles();
                }
                Err(e) => bad.push(format!("{id}: {e}")),
            }
        }
        let json = format!(
            "{{
  \"maps\": {}, \"load\": {ok}, \"triangles\": {tris},
  \"kinds\": {:?},
  \"failed\": {:?}
}}
",
            ids.len(),
            format!(
                "{{{}}}",
                kinds
                    .iter()
                    .map(|(k, (t, o))| format!("\"{k}\": [{t}, {o}]"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            bad.iter().take(40).collect::<Vec<_>>()
        );
        std::fs::write(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../work/notes/map_coverage.json"),
            &json,
        )
        .unwrap();
        println!("{json}");
    }

    /// Distribuição de formatos de textura (campo `fmt`/`bd` do IMGC) e de tamanhos, de todas as `.xi` do jogo.
    #[test]
    #[ignore]
    fn texture_stats() {
        let romfs = crate::romfs::locate(None).expect("RomFS");
        let fa = Archive::open(&romfs.join("yw1_a.fa")).unwrap();
        let index = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../data/romfs/yw1_a_files.csv"),
        )
        .unwrap();
        let mut fmts: BTreeMap<(u8, u8), (u32, u64)> = BTreeMap::new();
        let mut sizes: BTreeMap<usize, u32> = BTreeMap::new();
        let mut add = |d: &[u8]| {
            if d.len() > 20 && &d[0..4] == b"IMGC" {
                let e = fmts.entry((d[10], d[13])).or_default();
                e.0 += 1;
                if let Ok(img) = imgc::decode(d) {
                    e.1 += (img.width * img.height) as u64;
                    *sizes.entry(img.width.max(img.height)).or_default() += 1;
                }
            }
        };
        for line in index.lines().skip(1) {
            let path = line.split(',').next().unwrap();
            let Ok(d) = fa.read(path) else { continue };
            if path.ends_with(".xi") {
                add(&d);
            } else if path.ends_with(".xc") || path.ends_with(".xr") || path.ends_with(".xa") {
                if let Ok(p) = Pack::parse(&d) {
                    for (n, b) in &p.files {
                        if n.ends_with(".xi") {
                            add(b);
                        }
                    }
                }
            }
        }
        let total: u32 = fmts.values().map(|v| v.0).sum();
        println!("texturas {total}");
        for ((f, bd), (n, px)) in &fmts {
            println!("fmt 0x{f:02x} bd {bd}: {n} texturas, {px} pixels");
        }
        println!("maior lado: {sizes:?}");
    }
}

#[cfg(test)]
mod stuck_probe {
    /// Para cada evento que não termina: onde a corrotina principal está parada e que comandos nativos ele ignorou.
    #[test]
    #[ignore]
    fn stuck_events() {
        use crate::assets::arc0::Archive;
        use crate::event_host::EventScene;
        use crate::input::{Button, Input};
        use std::rc::Rc;
        let romfs = crate::romfs::locate(None).expect("RomFS");
        let fa = Rc::new(Archive::open(&romfs.join("yw1_a.fa")).unwrap());
        let json = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../work/notes/event_coverage.json"),
        )
        .unwrap();
        let stuck: Vec<String> = json
            .split("\"stuck\": [")
            .nth(1)
            .unwrap()
            .split(']')
            .next()
            .unwrap()
            .split(',')
            .map(|x| x.trim().trim_matches('"').to_string())
            .filter(|x| !x.is_empty())
            .collect();
        let mut input = Input::headless();
        for name in stuck {
            let Ok(mut ev) = EventScene::open(fa.clone(), &name, None, 1) else {
                continue;
            };
            for t in 0..6000 {
                input.set(Button::A, t % 20 == 19);
                if ev.tick(Some(&input)).is_err() || ev.finished {
                    break;
                }
            }
            println!(
                "{name}: {:?} | ignorados {:?}",
                ev.threads(),
                ev.host.unknown
            );
        }
    }
}
