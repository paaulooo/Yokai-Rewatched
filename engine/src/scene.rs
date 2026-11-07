//! Cena dirigida por script: um `.xq` de menu rodando na VM (`xq.rs`) com o `MenuHost`.
//! Tabela id de cena → script (deduzida do comportamento dos scripts — work/notes/xq32_start_sequence.md):
//! 2111 SaveLoadMenu (escolher slot), 2060 CharaMakeMenu (menino/menina), 2070 NameMenu (nome do jogador).

use crate::assets::arc0::Archive;
use crate::assets::Result;
use crate::input::Input;
use crate::menu_host::{MenuHost, SceneRequest};
use crate::screen::Screens;
use crate::xq::{self, Value, Vm};
use std::path::PathBuf;
use std::rc::Rc;

pub fn script_for(id: i32) -> Option<&'static str> {
    Some(match id {
        2110..=2113 => "seq/menu/SaveLoadMenu_001e.xq",
        2060 => "seq/menu/CharaMakeMenu_001e.xq",
        2070..=2073 => "seq/menu/NameMenu_001e.xq",
        // Menu do campo (X). O id do próprio MainMenu não aparece nos scripts: 2030 é do port.
        2030 => "seq/menu/MainMenu_003a.xq",
        2040 => "seq/menu/ItemMenu_001c.xq",
        2050 => "seq/menu/DictionaryMenu_001e.xq",
        2160 => "seq/menu/InfoMenu.xq",
        2180 => "seq/menu/ConfigMenu_001e.xq",
        2190 => "seq/menu/QuestMenu_001e.xq",
        _ => return None,
    })
}

pub struct ScriptScene {
    pub vm: Vm,
    pub host: MenuHost,
    pub id: i32,
    /// Nome digitado na tela 2070 (teclado do PC).
    pub typed_name: String,
}

impl ScriptScene {
    /// `args` = argumentos do `op2104(id, volta, extras...)`; a cena os lê com `op2212(n)`
    /// ([H] índice 3 = 1º extra; os scripts também leem `$0` = id).
    #[cfg(test)]
    pub fn open(fa: Rc<Archive>, romfs: PathBuf, id: i32, args: Vec<Value>) -> Result<ScriptScene> {
        Self::open_as(fa, romfs, id, args, 1)
    }

    /// `player_type`: 1 menino, 2 menina (vem da CharaMake).
    pub fn open_as(
        fa: Rc<Archive>,
        romfs: PathBuf,
        id: i32,
        args: Vec<Value>,
        player_type: i32,
    ) -> Result<ScriptScene> {
        let path = script_for(id).ok_or_else(|| format!("cena {id} sem script conhecido"))?;
        let scripts = xq::load_scene(&fa, path)?;
        let mut scene_args = vec![Value::Int(id)];
        scene_args.extend(args.into_iter().skip(1));
        // índices 0 = id, 1 = volta, 2 = (reservado), 3.. = extras
        let extras: Vec<Value> = scene_args.iter().skip(2).cloned().collect();
        let mut indexed = vec![
            Value::Int(id),
            scene_args.get(1).cloned().unwrap_or(Value::Int(-1)),
            Value::Int(0),
        ];
        indexed.extend(extras);
        let mut vm = Vm::new(scripts, indexed.clone());
        if !vm.start_scene() {
            return Err(format!("{path}: função de entrada não encontrada"));
        }
        eprintln!(
            "[yw1] cena {id}: {path} (args {:?})",
            indexed.iter().map(Value::text).collect::<Vec<_>>()
        );
        let mut host = MenuHost::new(fa, romfs, indexed);
        host.load_fonts();
        host.player_type = player_type;
        Ok(ScriptScene {
            vm,
            host,
            id,
            typed_name: String::new(),
        })
    }

    /// Um quadro de 1/60 s. Devolve a troca de cena pedida, se houver.
    pub fn tick(&mut self, input: Option<&Input>) -> Result<Option<SceneRequest>> {
        // Tela de nome (2070): digita-se com o teclado do PC (o teclado da tela de toque do jogo é desenhado
        // pelo script, mas o cursor dele depende de callbacks de controle ainda não feitos). Enter/Start confirma
        // como `NameMenu_ToOk` (player_set_name/type → cena 1010); vazio = nome padrão.
        if self.id == 2070 {
            if let Some(i) = input {
                for &c in &i.typed {
                    if c == '\u{8}' {
                        self.typed_name.pop();
                    } else if self.typed_name.chars().count() < 8 {
                        self.typed_name.push(c);
                    }
                }
            }
            // as letras digitadas não navegam o teclado do script
            if !input.is_some_and(|i| i.pressed(crate::input::Button::Start)) {
                self.vm.frame(&mut self.host)?;
                self.host.tick_objects();
                return Ok(None);
            }
        }
        if self.id == 2070 && input.is_some_and(|i| i.pressed(crate::input::Button::Start)) {
            let default = if self.host.player_type == 2 {
                "Katie"
            } else {
                "Nate"
            };
            let typed = self.typed_name.trim();
            let name = if typed.is_empty() {
                default.to_string()
            } else {
                typed.to_string()
            };
            self.host.player_name = Some(name);
            return Ok(Some(SceneRequest {
                id: 1010,
                args: vec![Value::Int(1010)],
            }));
        }
        if let Some(input) = input {
            let spawns = self.host.update_input(input);
            // B sem botão "voltar" reconhecido pelo host: chama a função de volta do script, se existir.
            if input.pressed(crate::input::Button::B) && spawns.is_empty() {
                for f in [
                    "Menu_Back",
                    "LoadMenu_Back",
                    "SaveMenu_Back",
                    "NameMenu_ToBack",
                    "ConfigMenu_Back",
                ] {
                    if self.vm.spawn_value(&Value::Str(f.into()), &[]).is_some() {
                        break;
                    }
                }
            }
            for (f, args) in spawns {
                if self.vm.spawn_value(&f, &args).is_none() {
                    eprintln!("[yw1] callback {} não encontrado", f.text());
                }
            }
        }
        self.vm.frame(&mut self.host)?;
        self.host.tick_objects();
        let mut req = self.host.request.take();
        // Script terminou sem pedir cena: volta para quem abriu (campo). id -1 = "fechar".
        if req.is_none() && self.vm.alive() == 0 {
            req = Some(SceneRequest {
                id: -1,
                args: vec![],
            });
        }
        if self.id == 2060 && req.is_some() {
            // CharaMakeMenu_Select grava $4001 = 1 menino / 2 menina.
            let t = self.vm.global(0, 4001).int();
            if t == 1 || t == 2 {
                self.host.player_type = t;
            }
        }
        Ok(req)
    }

    pub fn draw(&self, screens: &mut Screens) {
        self.host.draw(screens);
        if self.id == 2070 {
            // [H] campo do nome no topo da tela de baixo; cursor "_" piscando.
            let shown = format!("{}_", self.typed_name);
            self.host
                .draw_text(screens, false, (110.0, 14.0), &shown, 0x20_2020);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Button;

    fn open(id: i32, args: Vec<Value>) -> Option<ScriptScene> {
        let romfs = crate::romfs::locate(None)?;
        let fa = Rc::new(Archive::open(&romfs.join("yw1_a.fa")).ok()?);
        Some(ScriptScene::open(fa, romfs, id, args).unwrap())
    }

    fn dump(scene: &ScriptScene, label: &str) {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../work/menus");
        std::fs::create_dir_all(&dir).unwrap();
        let mut screens = Screens::new();
        scene.draw(&mut screens);
        let mut fb = Vec::new();
        screens.compose_scaled(&mut fb, 1, None);
        let raw: Vec<u8> = fb
            .iter()
            .flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, 255])
            .collect();
        std::fs::write(dir.join(format!("{label}_400x480.rgba")), raw).unwrap();
    }

    /// Novo jogo: SaveLoadMenu (2111, extra = 1) → escolher o 1º slot com A → o script pede a cena 2060.
    #[test]
    fn new_game_slot_leads_to_chara_make() {
        let Some(mut s) = open(2111, vec![Value::Int(2111), Value::Int(-1), Value::Int(1)]) else {
            return;
        };
        let mut input = Input::headless();
        let mut req = None;
        for f in 0..900 {
            input.set(Button::A, f == 200);
            if let Some(r) = s.tick(Some(&input)).unwrap() {
                req = Some(r);
                break;
            }
            if f == 150 {
                dump(&s, "saveload_f150");
            }
        }
        println!(
            "objetos: {}, nativos desconhecidos: {:?}",
            s.host.object_count(),
            s.host.unknown
        );
        println!("corrotinas: {:?}", s.vm.thread_names());
        let req = req.expect("nenhuma troca de cena");
        assert_eq!(req.id, 2060, "{req:?}");
    }
}

#[cfg(test)]
mod chain {
    use super::*;
    use crate::input::Button;

    /// Menu principal (X no campo) sem janela: grava a tela em work/menus/.
    #[test]
    #[ignore]
    fn main_menu_builds() {
        let romfs = crate::romfs::locate(None).unwrap();
        let fa = Rc::new(Archive::open(&romfs.join("yw1_a.fa")).unwrap());
        let mut s = ScriptScene::open(fa, romfs, 2030, vec![Value::Int(2030)]).unwrap();
        let mut input = Input::headless();
        for f in 0..240 {
            input.set(crate::input::Button::Down, f == 150);
            if let Some(r) = s.tick(Some(&input)).unwrap() {
                println!("pediu cena {r:?}");
                break;
            }
        }
        println!(
            "objetos {} desconhecidos {:?}",
            s.host.object_count(),
            s.host.unknown
        );
        println!("corrotinas {:?}", s.vm.thread_names());
        super::tests_dump(&s, "mainmenu_f240");
    }

    /// Menu principal: B fecha (o script termina → pedido -1); Settings (2180) monta e grava tela.
    #[test]
    #[ignore]
    fn main_menu_closes_and_settings_build() {
        let romfs = crate::romfs::locate(None).unwrap();
        let fa = Rc::new(Archive::open(&romfs.join("yw1_a.fa")).unwrap());
        let mut s =
            ScriptScene::open(fa.clone(), romfs.clone(), 2030, vec![Value::Int(2030)]).unwrap();
        let mut input = Input::headless();
        let mut closed = None;
        for f in 0..600 {
            input.set(crate::input::Button::B, f == 120);
            if let Some(r) = s.tick(Some(&input)).unwrap() {
                closed = Some(r.id);
                break;
            }
        }
        assert_eq!(closed, Some(-1), "B não fechou o menu");
        let mut c = ScriptScene::open(fa, romfs, 2180, vec![Value::Int(2180)]).unwrap();
        for _ in 0..240 {
            if c.tick(Some(&input)).unwrap().is_some() {
                break;
            }
        }
        println!(
            "config: objetos {} desconhecidos {:?}",
            c.host.object_count(),
            c.host.unknown
        );
        super::tests_dump(&c, "config_f240");
    }

    /// Novo jogo inteiro sem janela, apertando A a cada 90 quadros: 2111 → 2060 → 2070 → 1010.
    /// Renderiza uma cena de menu sem janela e grava as duas telas em `work/scene/`:
    /// `YW_SCENE=2060 YW_TICKS=120 cargo test --release scene_snapshot -- --ignored`
    #[test]
    #[ignore]
    fn scene_snapshot() {
        let id: i32 = std::env::var("YW_SCENE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2060);
        let ticks: usize = std::env::var("YW_TICKS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(120);
        let romfs = crate::romfs::locate(None).unwrap();
        let fa = Rc::new(Archive::open(&romfs.join("yw1_a.fa")).unwrap());
        let mut s = ScriptScene::open_as(
            fa,
            romfs,
            id,
            vec![Value::Int(id), Value::Int(-1), Value::Int(1)],
            1,
        )
        .unwrap();
        let input = Input::headless();
        for _ in 0..ticks {
            let _ = s.tick(Some(&input));
        }
        println!(
            "objetos {} | corrotinas {:?} | ignorados {:?}",
            s.host.object_count(),
            s.vm.thread_names(),
            s.host.unknown
        );
        let mut screens = crate::screen::Screens::new();
        s.draw(&mut screens);
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../work/scene");
        std::fs::create_dir_all(&dir).unwrap();
        for (name, px) in [("top", &screens.top), ("bottom", &screens.bottom)] {
            let raw: Vec<u8> = px
                .iter()
                .flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, 255])
                .collect();
            std::fs::write(dir.join(format!("{id}_{name}.rgba")), raw).unwrap();
        }
    }

    /// `cargo test --release -- --ignored --nocapture new_game_chain`
    #[test]
    #[ignore]
    fn new_game_chain() {
        let romfs = crate::romfs::locate(None).unwrap();
        let fa = Rc::new(Archive::open(&romfs.join("yw1_a.fa")).unwrap());
        let mut id = 2111;
        let mut args = vec![Value::Int(2111), Value::Int(-1), Value::Int(1)];
        let mut input = Input::headless();
        let mut ptype = 1;
        for _ in 0..4 {
            let Some(_) = script_for(id) else {
                println!("fim da cadeia em {id}");
                break;
            };
            let mut s =
                ScriptScene::open_as(fa.clone(), romfs.clone(), id, args.clone(), ptype).unwrap();
            let mut next = None;
            for f in 0..3000 {
                if id == 2070 {
                    input.set(Button::Start, f == 300);
                } else {
                    input.set(Button::A, f % 90 == 89);
                }
                match s.tick(Some(&input)) {
                    Ok(Some(r)) => {
                        next = Some(r);
                        break;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        println!("ERRO {e}");
                        break;
                    }
                }
                if f == 120 {
                    super::tests_dump(&s, &format!("chain_{id}_f120"));
                }
            }
            println!("cena {id}: desconhecidos {:?}", s.host.unknown);
            println!(
                "  corrotinas {:?} nome {:?} tipo {}",
                s.vm.thread_names(),
                s.host.player_name,
                s.host.player_type
            );
            let Some(r) = next else {
                println!("  cena {id} não pediu troca");
                break;
            };
            println!("  → {:?}", r);
            ptype = s.host.player_type;
            id = r.id;
            args = r.args;
        }
    }
}

#[cfg(test)]
pub(crate) fn tests_dump(scene: &ScriptScene, label: &str) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../work/menus");
    std::fs::create_dir_all(&dir).unwrap();
    let mut screens = Screens::new();
    scene.draw(&mut screens);
    let mut fb = Vec::new();
    screens.compose_scaled(&mut fb, 1, None);
    let raw: Vec<u8> = fb
        .iter()
        .flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, 255])
        .collect();
    std::fs::write(dir.join(format!("{label}_400x480.rgba")), raw).unwrap();
}
