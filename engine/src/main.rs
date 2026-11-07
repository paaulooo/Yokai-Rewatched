//! yw1 — runtime nativo do Yo-kai Watch 1 (3DS) para PC.
//!
//! Estado atual: boot (tela preta → logo Level-5 → abertura com música) → menu de título
//! reconstruído dos layouts reais do jogo. Escolher uma opção do menu ainda não leva a lugar nenhum.
//!
//! Uso: `cargo run --release -- [caminho/do/romfs]` (ou `YW_ROMFS`); `YW_FFMPEG` aponta o ffmpeg;
//! `YW_START=title` pula os vídeos. Controles: ver `input.rs` (A/B/Start/toque pulam vídeos, Esc sai).

mod assets;
mod audio;
mod battle;
mod chapter1;
mod character;
mod clock;
mod coverage;
mod cutscene;
mod dialog;
mod event_host;
mod flags;
mod gfx3d;
mod input;
mod layout;
mod menu3d;
mod menu_host;
mod prologue;
mod render2d;
mod romfs;
mod room;
mod save;
mod scene;
mod screen;
mod settings;
mod title;
mod video;
mod xq;
mod yokai;

use input::{Button, Input};
use minifb::{Key, KeyRepeat, Scale, ScaleMode, Window, WindowOptions};
use screen::{Screens, WIN_H, WIN_W};
use settings::{Settings, MAX_RENDER_SCALE};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use video::Next as VideoNext;

pub fn ffmpeg_path() -> PathBuf {
    std::env::var_os("YW_FFMPEG")
        .map(PathBuf::from)
        .unwrap_or_else(|| "ffmpeg".into())
}

struct MovieSpec {
    name: &'static str,
    file: &'static str,
    fps: f64,
    stereo: bool,
    audio: Option<&'static str>,
}

// fps medidos com ffprobe; estéreo medido por paralaxe entre quadros (docs/FORMATS.md).
// Ordem logo → abertura → menu: a do jogo rodando; no code.bin só foram achados `LogoMenu`,
// `lxpCMovieManager` e a extensão `.moflex` (a ordem exata no código ainda não foi lida).
const BOOT_MOVIES: [MovieSpec; 2] = [
    MovieSpec {
        name: "logo Level-5",
        file: "mov/logo_l5.moflex",
        fps: 60.0,
        stereo: false,
        audio: None,
    },
    MovieSpec {
        name: "abertura",
        file: "mov/yw1_op.moflex",
        fps: 48.0,
        stereo: true,
        audio: Some("snd/product/stream/yw1_op_en.dspadpcm.bcstm"),
    },
];

struct Movie {
    name: &'static str,
    video: video::Video,
    audio: Option<audio::Player>,
    start: Instant,
    shown: u64,
}

impl Movie {
    fn clock(&self) -> f64 {
        match &self.audio {
            Some(a) => a.seconds(),
            None => self.start.elapsed().as_secs_f64(),
        }
    }

    /// Avança até o quadro devido pelo relógio; devolve `true` quando o vídeo acabou.
    fn update(&mut self, top: &mut [u32]) -> bool {
        let due = (self.clock() * self.video.fps) as u64;
        while self.shown <= due {
            match self.video.next() {
                VideoNext::Frame(f) => {
                    top.copy_from_slice(&f);
                    self.shown += 1;
                }
                VideoNext::Pending => break,
                VideoNext::Ended => return true,
            }
        }
        false
    }
}

fn open_movie(romfs: &Path, spec: &MovieSpec, top: &mut [u32]) -> Option<Movie> {
    let file = romfs.join(spec.file);
    let video = match video::Video::open(&ffmpeg_path(), &file, spec.fps, spec.stereo) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "[yw1] {}: não foi possível iniciar o ffmpeg ({e}); defina YW_FFMPEG",
                spec.name
            );
            return None;
        }
    };
    match video.wait(Duration::from_secs(5)) {
        VideoNext::Frame(f) => top.copy_from_slice(&f),
        _ => {
            eprintln!(
                "[yw1] {}: nenhum quadro decodificado de {}",
                spec.name,
                file.display()
            );
            return None;
        }
    }
    let audio = spec
        .audio
        .and_then(|rel| play_stream(romfs, rel, spec.name));
    eprintln!(
        "[yw1] {}: {} ({} fps{}{})",
        spec.name,
        file.display(),
        video.fps,
        if spec.stereo { ", olho esquerdo" } else { "" },
        audio
            .as_ref()
            .map_or(String::new(), |a| format!(", áudio {:.2} s", a.duration())),
    );
    Some(Movie {
        name: spec.name,
        video,
        audio,
        start: Instant::now(),
        shown: 1,
    })
}

fn play_stream(romfs: &Path, rel: &str, what: &str) -> Option<audio::Player> {
    let path = romfs.join(rel);
    let stream = std::fs::read(&path)
        .map_err(|e| e.to_string())
        .and_then(|b| audio::bcstm::decode(&b).map_err(|e| e.to_string()))
        .and_then(|s| audio::Player::play(Arc::new(s)));
    match stream {
        Ok(p) => Some(p),
        Err(e) => {
            eprintln!("[yw1] {what}: sem áudio ({}): {e}", path.display());
            None
        }
    }
}

const TICK: f64 = 1.0 / 60.0;

struct TitleStage {
    scene: title::Title,
    clock: Instant,
    pending: f64,
}

fn load_title(romfs: Option<&Path>) -> Option<TitleStage> {
    let romfs = romfs?;
    let fa_path = romfs.join("yw1_a.fa");
    let loaded = assets::arc0::Archive::open(&fa_path).and_then(|fa| {
        eprintln!("[yw1] {}: {} arquivos", fa_path.display(), fa.len());
        // Ainda não há saves no port: o script então mostra só "New Game".
        title::Title::load(
            &fa,
            romfs,
            title::SaveInfo {
                has_save: save::any(),
                lens_unlocked: false,
            },
        )
    });
    match loaded {
        Ok(scene) => {
            eprintln!("[yw1] menu de título carregado");
            Some(TitleStage {
                scene,
                clock: Instant::now(),
                pending: 0.0,
            })
        }
        Err(e) => {
            eprintln!("[yw1] não foi possível carregar o menu de título: {e}");
            None
        }
    }
}

struct RoomStage {
    room: room::Room,
    clock: Instant,
    pending: f64,
    save: save::Save,
    slot: usize,
    /// Segundos jogados nesta sessão de campo (somados ao save).
    play: f64,
    event: Option<prologue::Prologue>,
    music: Option<audio::Player>,
    /// Menu aberto por cima do campo (MainMenu e as telas que ele abre).
    menu: Option<scene::ScriptScene>,
    /// Capítulo 1 depois do prólogo + jogo livre (eventos, batalhas, busca de Yo-kai).
    story: Option<chapter1::Story>,
    /// Nome do stream tocando (para trocar entre campo e batalha).
    music_name: Option<String>,
}

/// Toca um stream de `snd/product/stream/` pelo nome do script (`M38_keta_house`, `ev01_0020_01`...).
fn play_named(romfs: Option<&Path>, name: &str, volume: f32) -> Option<audio::Player> {
    let romfs = romfs?;
    let dir = romfs.join("snd/product/stream");
    let want = name.to_ascii_lowercase();
    let file = std::fs::read_dir(&dir).ok()?.flatten().find_map(|e| {
        let n = e.file_name().to_string_lossy().to_string();
        (n.to_ascii_lowercase().starts_with(&want) && n.ends_with(".bcstm")).then_some(n)
    })?;
    let p = play_stream(romfs, &format!("snd/product/stream/{file}"), name);
    if let Some(m) = &p {
        m.set_volume(volume);
    }
    p
}

/// HIPÓTESE: os interiores da casa (t101i01/i02) tocam `M38_keta_house` (mesmo campo final no map_config).
/// HIPÓTESE (pelos nomes): rua `M03_sakura_new_town`, morro dos fundos (Mt. Wildwood, *urayama*) `M04urayamab2`.
fn map_music(map: &str) -> Option<&'static str> {
    match map {
        "t101i01" | "t101i02" | "t101i04" => Some("M38_keta_house"),
        "t101g00" => Some("M03_sakura_new_town"),
        "t102g00" => Some("M04urayamab2"),
        _ => None,
    }
}

/// Cria o capítulo 1 / jogo livre a partir do save (depois do prólogo).
fn open_story(romfs: Option<&Path>, sv: &save::Save) -> Option<chapter1::Story> {
    if !sv.prologue_done {
        return None;
    }
    let fa = assets::arc0::Archive::open(&romfs?.join("yw1_a.fa")).ok()?;
    chapter1::Story::new(std::rc::Rc::new(fa), sv.player_type)
        .map_err(|e| eprintln!("[yw1] dados de Yo-kai indisponíveis: {e}"))
        .ok()
}

impl RoomStage {
    /// Estado atual como save (F5 salva; troca de mapa leva adiante).
    fn snapshot(&self) -> save::Save {
        let mut sv = self.save.clone();
        sv.map = self.room.id.clone();
        sv.pos = self.room.player.pos;
        sv.yaw = self.room.player.yaw;
        sv.play_seconds += self.play as u64;
        sv
    }
}

fn load_room(romfs: Option<&Path>, id: &str, player_type: i32) -> Option<RoomStage> {
    let romfs = romfs?;
    let loaded = assets::arc0::Archive::open(&romfs.join("yw1_a.fa")).and_then(|fa| {
        let mut room = room::Room::load(&fa, id)?;
        match room::PlayerModel::load(&fa, player_type) {
            Ok(m) => room.model = Some(m),
            Err(e) => eprintln!("[yw1] modelo do jogador indisponível ({e}); usando a caixa"),
        }
        Ok(room)
    });
    match loaded {
        Ok(room) => {
            eprintln!(
                "[yw1] mapa {id}: {} triângulos, {} segmentos de parede, câmera {:?}",
                room.triangles(),
                room.walls(),
                room.camera
            );
            Some(RoomStage {
                room,
                clock: Instant::now(),
                pending: 0.0,
                save: save::Save::new_game("Nate", 1),
                slot: 1,
                play: 0.0,
                event: None,
                music: None,
                menu: None,
                story: None,
                music_name: None,
            })
        }
        Err(e) => {
            eprintln!("[yw1] não foi possível carregar o mapa {id}: {e}");
            None
        }
    }
}

/// Tela de carregamento do jogo (`data/menu/loading_en.xa`: `load_base` "LOADING..." + olhos de Yo-kai
/// `load_01..06`, origem no centro da tela de cima). Fica no mínimo `LOADING_MIN` quadros, mesmo que o mapa
/// carregue antes (pedido do dono: mostrar o loading sempre).
struct LoadingStage {
    layout: Option<layout::Layout>,
    base: Option<layout::Anim>,
    eyes: Option<layout::Anim>,
    frame: u32,
    map: String,
    pos: [f32; 3],
    yaw: f32,
    save: save::Save,
    slot: usize,
    room: Option<RoomStage>,
}

const LOADING_MIN: u32 = 45;

fn open_loading(
    romfs: Option<&Path>,
    map: String,
    pos: [f32; 3],
    yaw: f32,
    save: save::Save,
    slot: usize,
) -> LoadingStage {
    let layout = romfs
        .and_then(|r| assets::arc0::Archive::open(&r.join("yw1_a.fa")).ok())
        .and_then(|fa| fa.read("data/menu/loading_en.xa").ok())
        .and_then(|b| layout::Layout::load(&b).ok());
    // Qual par de olhos aparece: o jogo sorteia (HIPÓTESE); aqui gira pelo mapa de destino.
    let pick = 1 + (map.bytes().map(|b| b as usize).sum::<usize>() % 6);
    let anim = |name: &str| {
        layout
            .as_ref()
            .and_then(|l| layout::Anim::new(l, name, true).ok())
            .map(|mut a| {
                a.offset = (200.0, 120.0);
                a
            })
    };
    LoadingStage {
        base: anim("load_base"),
        eyes: anim(&format!("load_{pick:02}")),
        layout,
        frame: 0,
        map,
        pos,
        yaw,
        save,
        slot,
        room: None,
    }
}

enum Stage {
    Black {
        until: Instant,
    },
    /// Vídeo; se houver título guardado, volta para ele no fim (abertura repetida pelo título ocioso).
    Movie(Movie, Option<Box<TitleStage>>),
    Title(Box<TitleStage>),
    Room(Box<RoomStage>),
    Loading(Box<LoadingStage>),
    /// Menu dirigido por script (SaveLoad/CharaMake/Name...).
    Script(Box<ScriptStage>),
    Idle,
}

struct ScriptStage {
    scene: scene::ScriptScene,
    music: Option<audio::Player>,
    clock: Instant,
    pending: f64,
}

fn open_script(
    romfs: Option<&Path>,
    id: i32,
    args: Vec<xq::Value>,
    player_type: i32,
) -> Option<ScriptStage> {
    let romfs = romfs?;
    let fa = assets::arc0::Archive::open(&romfs.join("yw1_a.fa")).ok()?;
    match scene::ScriptScene::open_as(
        std::rc::Rc::new(fa),
        romfs.to_path_buf(),
        id,
        args,
        player_type,
    ) {
        Ok(scene) => Some(ScriptStage {
            scene,
            music: None,
            clock: Instant::now(),
            pending: 0.0,
        }),
        Err(e) => {
            eprintln!("[yw1] cena {id}: {e}");
            None
        }
    }
}

/// O que fazer depois de uma cena de script: outro menu, o campo (1010) ou o título.
enum Next {
    Script(i32, Vec<xq::Value>, i32),
    /// Entrar no campo com este save (novo jogo ou Continue) no diário `slot`.
    Field(save::Save, usize),
    Title,
}

fn main() {
    let explicit = std::env::args_os().nth(1).map(PathBuf::from);
    let romfs = romfs::locate(explicit.as_deref());
    match &romfs {
        Some(p) => eprintln!("[yw1] RomFS: {}", p.display()),
        None => eprintln!(
            "[yw1] RomFS não encontrado (passe o caminho ou defina YW_ROMFS); só tela preta."
        ),
    }

    let mut settings = Settings::load();
    let options = WindowOptions {
        resize: true,
        scale: match settings.window_scale {
            1 => Scale::X1,
            3 => Scale::X1, // minifb só tem potências de 2; 3 → ajuste manual da janela
            4 => Scale::X4,
            _ => Scale::X2,
        },
        scale_mode: ScaleMode::AspectRatioStretch,
        ..WindowOptions::default()
    };
    let mut window = Window::new("Yo-kai Watch — port nativo (yw1)", WIN_W, WIN_H, options)
        .expect("criar janela");
    window.set_target_fps(60);

    let mut input = Input::new();
    let mut screens = Screens::new();
    let mut fb: Vec<u32> = Vec::new();
    let start = std::env::var("YW_START").unwrap_or_default();
    let start_title = matches!(
        start.as_str(),
        "title" | "room" | "newgame" | "chapter1" | "wild"
    );
    let mut movies = if start_title {
        BOOT_MOVIES[..0].iter()
    } else {
        BOOT_MOVIES.iter()
    };
    let mut stage = Stage::Black {
        until: Instant::now() + Duration::from_millis(500),
    };

    if start == "newgame" {
        if let Some(st) = open_script(
            romfs.as_deref(),
            2111,
            vec![xq::Value::Int(2111), xq::Value::Int(-1), xq::Value::Int(1)],
            1,
        ) {
            stage = Stage::Script(Box::new(st));
        }
    }
    if start == "room" {
        // passa pela tela de carregamento com um diário novo: o prólogo (monólogo + 1ª cutscene ev01_0005 +
        // rua) toca antes do quarto, como no jogo
        let sv = save::Save::new_game("Nate", 1);
        let (map, pos, yaw) = (sv.map.clone(), sv.pos, sv.yaw);
        stage = Stage::Loading(Box::new(open_loading(
            romfs.as_deref(),
            map,
            pos,
            yaw,
            sv,
            3,
        )));
    }
    // Atalhos de teste (diário 3): `chapter1` = pé da escada com o prólogo visto; `wild` = Mt. Wildwood livre
    // com o Buhu no time (segurar Y parado procura Yo-kai).
    if start == "chapter1" || start == "wild" {
        let mut sv = save::Save::new_game("Nate", 1);
        sv.prologue_done = true;
        if start == "wild" {
            sv.chapter = chapter1::DONE;
            sv.party = vec![(0x8443_d22d_u32 as i32, 3, 0)];
            sv.has_watch = true;
            sv.map = "t102g00".into();
            sv.pos = [693.0, -20.0, -96.0];
        } else {
            sv.map = "t101i01".into();
            sv.pos = [23.5, 0.0, 26.0];
        }
        let (map, pos, yaw) = (sv.map.clone(), sv.pos, sv.yaw);
        stage = Stage::Loading(Box::new(open_loading(
            romfs.as_deref(),
            map,
            pos,
            yaw,
            sv,
            3,
        )));
    }
    let mut hi_top: Option<Vec<u32>> = None;

    while window.is_open() && !window.is_key_down(Key::Escape) {
        input.update(&window);
        for (key, delta) in [(Key::F1, -1isize), (Key::F2, 1)] {
            if window.is_key_pressed(key, KeyRepeat::No) {
                settings.render_scale = (settings.render_scale as isize + delta)
                    .clamp(1, MAX_RENDER_SCALE as isize)
                    as usize;
                eprintln!(
                    "[yw1] resolução interna do 3D: {0}× ({1}×{2})",
                    settings.render_scale,
                    400 * settings.render_scale,
                    240 * settings.render_scale
                );
                settings.save();
            }
        }
        let skip = [Button::A, Button::B, Button::Start]
            .into_iter()
            .any(|b| input.pressed(b))
            || input.touch_released().is_some();
        let mut attract = false;
        let mut next: Option<Next> = None;
        let mut map_jump: Option<room::MapJump> = None;
        let finished = match &mut stage {
            Stage::Black { until } => Instant::now() >= *until,
            Stage::Movie(m, _) => skip || m.update(&mut screens.top),
            Stage::Title(t) => {
                let now = Instant::now();
                t.pending = (t.pending + now.duration_since(t.clock).as_secs_f64()).min(TICK * 4.0);
                t.clock = now;
                let mut first = true;
                while t.pending >= TICK && !attract {
                    t.pending -= TICK;
                    match t.scene.tick(first.then_some(&input)) {
                        // `Menu_TitleToNew` / `Menu_TitleToContinue` → cena 2111 (SaveLoadMenu) com new = 1 / 0.
                        // Novo jogo segue CharaMake (2060) → NameMenu (2070) → prólogo (monólogo ev01_0003 →
                        // 1ª cutscene ev01_0005 → rua ev01_0010 → quarto ev01_0020); Continue carrega o diário.
                        Some(ev @ (title::Event::NewGame | title::Event::Continue)) => {
                            let new = i32::from(ev == title::Event::NewGame);
                            next = Some(Next::Script(
                                2111,
                                vec![
                                    xq::Value::Int(2111),
                                    xq::Value::Int(-1),
                                    xq::Value::Int(new),
                                ],
                                1,
                            ));
                        }
                        Some(title::Event::YokaiCam) => {
                            eprintln!("[yw1] Yo-kai Cam: não suportado (câmera do 3DS)")
                        }
                        Some(title::Event::Attract) => attract = true,
                        None => {}
                    }
                    first = false;
                }
                t.scene.draw(&mut screens);
                false
            }
            Stage::Script(st) => {
                let now = Instant::now();
                st.pending =
                    (st.pending + now.duration_since(st.clock).as_secs_f64()).min(TICK * 4.0);
                st.clock = now;
                let mut first = true;
                while st.pending >= TICK && next.is_none() {
                    st.pending -= TICK;
                    match st.scene.tick(first.then_some(&input)) {
                        Ok(Some(req)) => {
                            let t = st.scene.host.player_type;
                            if let Some(n) = &st.scene.host.player_name {
                                eprintln!("[yw1] jogador: {n} (tipo {t})");
                            }
                            next = Some(match req.id {
                                1010 => {
                                    let slot = st.scene.host.current_slot;
                                    // novo jogo / carregar diário: a hora volta a ser a do PC
                                    clock::reset();
                                    let sv = match &st.scene.host.player_name {
                                        // Novo jogo (NameMenu confirmou): cria o diário já salvo.
                                        Some(name) => {
                                            flags::reset();
                                            let sv = save::Save::new_game(name, t);
                                            save::store(slot, &sv);
                                            sv
                                        }
                                        None => {
                                            save::restore_flags(slot);
                                            save::load(slot)
                                                .unwrap_or_else(|| save::Save::new_game("Nate", t))
                                        }
                                    };
                                    Next::Field(sv, slot)
                                }
                                id if scene::script_for(id).is_some() => {
                                    Next::Script(id, req.args, t)
                                }
                                id => {
                                    eprintln!("[yw1] cena {id} ainda não reconstruída: voltando ao título");
                                    Next::Title
                                }
                            });
                        }
                        Ok(None) => {}
                        Err(e) => {
                            eprintln!("[yw1] erro no script da cena {}: {e}", st.scene.id);
                            next = Some(Next::Title);
                        }
                    }
                    first = false;
                }
                let host = &mut st.scene.host;
                if host.bgm_stop {
                    host.bgm_stop = false;
                    st.music = None;
                }
                if let Some(name) = host.bgm_play.take() {
                    st.music = host.bgm_path(&name).and_then(|p| {
                        let rel = p
                            .strip_prefix(romfs.as_deref()?)
                            .ok()?
                            .to_string_lossy()
                            .replace('\\', "/");
                        play_stream(romfs.as_deref()?, &rel, &name)
                    });
                    if let Some(m) = &st.music {
                        m.set_volume(settings.bgm_volume());
                    }
                }
                st.scene.draw(&mut screens);
                false
            }
            Stage::Room(r) => {
                let now = Instant::now();
                let dt = now.duration_since(r.clock).as_secs_f64();
                r.play += dt;
                r.pending = (r.pending + dt).min(TICK * 4.0);
                r.clock = now;
                let mut first = true;
                while r.pending >= TICK && map_jump.is_none() {
                    r.pending -= TICK;
                    r.room.camera_speed = settings.camera_speed;
                    let ev_done = r.event.as_ref().is_some_and(|e| e.finished());
                    if ev_done {
                        r.event = None;
                        r.save.prologue_done = true;
                        let sv = r.snapshot();
                        save::store(r.slot, &sv);
                        r.story = open_story(romfs.as_deref(), &r.save);
                    }
                    if let Some(menu) = r.menu.as_mut() {
                        match menu.tick(first.then_some(&input)) {
                            Ok(None) => {}
                            Ok(Some(req)) => {
                                let id = req.id;
                                if menu.host.save_requested {
                                    let sv = r.snapshot();
                                    save::store(r.slot, &sv);
                                }
                                r.menu = None;
                                settings = Settings::load();
                                if let Some(m) = &r.music {
                                    m.set_volume(settings.bgm_volume());
                                }
                                if scene::script_for(id).is_some() || id == 2110 {
                                    let fa = romfs.as_deref().and_then(|rf| {
                                        assets::arc0::Archive::open(&rf.join("yw1_a.fa")).ok()
                                    });
                                    // Diário = salvar (2110): grava direto e volta ao campo.
                                    if id == 2110 {
                                        let sv = r.snapshot();
                                        save::store(r.slot, &sv);
                                    } else if let (Some(fa), Some(rf)) = (fa, romfs.as_deref()) {
                                        r.menu = scene::ScriptScene::open_as(
                                            std::rc::Rc::new(fa),
                                            rf.to_path_buf(),
                                            id,
                                            req.args,
                                            r.save.player_type,
                                        )
                                        .map_err(|e| eprintln!("[yw1] menu {id}: {e}"))
                                        .ok();
                                    }
                                } else if id != -1 && id != 1010 {
                                    eprintln!("[yw1] menu pediu a cena {id} (ainda não reconstruída); voltando ao campo");
                                }
                            }
                            Err(e) => {
                                eprintln!("[yw1] erro no menu: {e}");
                                r.menu = None;
                            }
                        }
                    } else if let Some(ev) = r.event.as_mut() {
                        if let Some(prologue::Music::Play(name)) =
                            ev.tick(&mut r.room, first.then_some(&input))
                        {
                            r.music = None;
                            r.music = play_named(romfs.as_deref(), name, settings.bgm_volume());
                        }
                    } else {
                        // capítulo 1 / jogo livre: eventos, batalhas e lente
                        let outs = match r.story.as_mut() {
                            Some(story) => {
                                story.tick(&mut r.room, first.then_some(&input), &mut r.save)
                            }
                            None => Vec::new(),
                        };
                        if let Some(story) = r.story.as_ref() {
                            for o in outs {
                                match o {
                                    chapter1::Out::Warp(to, arrival, yaw) => {
                                        map_jump = Some(room::MapJump::to(to, arrival, yaw));
                                    }
                                    chapter1::Out::Save => {
                                        let sv = r.snapshot();
                                        save::store(r.slot, &sv);
                                    }
                                }
                            }
                            // música: batalha ou a do mapa
                            let want = if story.in_battle() {
                                Some("M25_battle01")
                            } else {
                                map_music(&r.room.id)
                            };
                            if want.map(str::to_string) != r.music_name {
                                r.music = None;
                                r.music = want.and_then(|m| {
                                    play_named(romfs.as_deref(), m, settings.bgm_volume())
                                });
                                r.music_name = want.map(str::to_string);
                            }
                        }
                        let story_busy = r.story.as_ref().is_some_and(|s| s.busy());
                        if !story_busy && map_jump.is_none() {
                            map_jump = r.room.tick(&input);
                        }
                        // X (Xbox Y / PS △) abre o menu principal, como no 3DS.
                        if first && !story_busy && input.pressed(input::Button::X) {
                            let fa = romfs.as_deref().and_then(|rf| {
                                assets::arc0::Archive::open(&rf.join("yw1_a.fa")).ok()
                            });
                            if let (Some(fa), Some(rf)) = (fa, romfs.as_deref()) {
                                r.menu = scene::ScriptScene::open_as(
                                    std::rc::Rc::new(fa),
                                    rf.to_path_buf(),
                                    2030,
                                    vec![xq::Value::Int(2030)],
                                    r.save.player_type,
                                )
                                .map_err(|e| eprintln!("[yw1] menu principal: {e}"))
                                .ok();
                            }
                        }
                    }
                    first = false;
                }
                if window.is_key_pressed(Key::F5, KeyRepeat::No) {
                    let sv = r.snapshot();
                    save::store(r.slot, &sv);
                }
                if let Some(menu) = &r.menu {
                    // o 2D do menu ocupa as duas telas (o 3D fica pausado por baixo)
                    menu.draw(&mut screens);
                    hi_top = None;
                } else {
                    screens.bottom.fill(0);
                    let mut scene = r
                        .event
                        .as_mut()
                        .and_then(|e| e.scene_frame(settings.render_scale, &mut screens))
                        .map(|f| (f.width, f.height, f.color.clone()));
                    if scene.is_none() {
                        scene = r
                            .story
                            .as_mut()
                            .and_then(|s| s.frame(settings.render_scale, &mut screens))
                            .map(|f| (f.width, f.height, f.color.clone()));
                    }
                    let (fw, fh, mut top) = match scene {
                        Some(x) => x,
                        None => {
                            let frame = r.room.draw(settings.render_scale);
                            (frame.width, frame.height, frame.color.clone())
                        }
                    };
                    if let Some(ev) = &r.event {
                        ev.draw(&mut top, fw, fh, &mut screens);
                    }
                    if let Some(st) = &r.story {
                        st.overlay(&mut top, fw, fh, &mut screens);
                        if let Some(h) = st.hint(&r.save) {
                            st.draw_hint(&h, &mut screens);
                        }
                    }
                    hi_top = Some(top);
                }
                false
            }
            Stage::Loading(l) => {
                l.frame += 1;
                if l.frame == 2 {
                    // Carrega de fato (depois de já ter mostrado um quadro do loading).
                    let t0 = Instant::now();
                    l.room =
                        load_room(romfs.as_deref(), &l.map, l.save.player_type).map(|mut r| {
                            r.room.place(l.pos, l.yaw);
                            r.save = l.save.clone();
                            r.slot = l.slot;
                            if !r.save.prologue_done && (l.map == "t101i02" || l.map == "t101i04") {
                                r.event = romfs
                                    .as_deref()
                                    .and_then(|rf| {
                                        assets::arc0::Archive::open(&rf.join("yw1_a.fa")).ok()
                                    })
                                    .map(|fa| {
                                        dialog::set_player_name(&r.save.name);
                                        prologue::Prologue::new(
                                            std::rc::Rc::new(fa),
                                            r.save.player_type,
                                        )
                                    });
                            } else {
                                r.story = open_story(romfs.as_deref(), &r.save);
                                if let Some(m) = map_music(&l.map) {
                                    r.music =
                                        play_named(romfs.as_deref(), m, settings.bgm_volume());
                                    r.music_name = Some(m.to_string());
                                }
                            }
                            r
                        });
                    eprintln!(
                        "[yw1] loading {}: {:.0} ms",
                        l.map,
                        t0.elapsed().as_secs_f64() * 1000.0
                    );
                }
                screens.clear();
                if let Some(layout) = &l.layout {
                    for a in [&mut l.base, &mut l.eyes].into_iter().flatten() {
                        a.tick();
                    }
                    let anims: Vec<&layout::Anim> =
                        [&l.base, &l.eyes].into_iter().flatten().collect();
                    let mut t = render2d::Target {
                        pixels: &mut screens.top,
                        width: screen::TOP_W,
                        height: screen::TOP_H,
                    };
                    layout.draw(&mut t, &anims);
                }
                l.frame >= LOADING_MIN
            }
            Stage::Idle => false,
        };
        if let Some(j) = map_jump.take() {
            if let Stage::Room(r) = &mut stage {
                eprintln!("[yw1] {} → {} (chegada {:?})", r.room.id, j.to, j.arrival);
                let mut sv = r.snapshot();
                sv.map = j.to.clone();
                sv.pos = j.arrival;
                sv.yaw = j.yaw;
                let slot = r.slot;
                stage = Stage::Loading(Box::new(open_loading(
                    romfs.as_deref(),
                    j.to,
                    j.arrival,
                    j.yaw,
                    sv,
                    slot,
                )));
            }
        } else if let Some(n) = next.take() {
            stage = match n {
                Next::Script(id, args, t) => match open_script(romfs.as_deref(), id, args, t) {
                    Some(st) => {
                        window.set_title(&format!("Yo-kai Watch — port nativo (yw1) — cena {id}"));
                        Stage::Script(Box::new(st))
                    }
                    None => Stage::Idle,
                },
                Next::Field(sv, slot) => {
                    let (map, pos, yaw) = (sv.map.clone(), sv.pos, sv.yaw);
                    Stage::Loading(Box::new(open_loading(
                        romfs.as_deref(),
                        map,
                        pos,
                        yaw,
                        sv,
                        slot,
                    )))
                }
                Next::Title => match load_title(romfs.as_deref()) {
                    Some(t) => Stage::Title(Box::new(t)),
                    None => Stage::Idle,
                },
            };
            screens.clear();
        } else if attract {
            let Stage::Title(t) = std::mem::replace(&mut stage, Stage::Idle) else {
                unreachable!()
            };
            eprintln!("[yw1] título ocioso por 30 s: repetindo a abertura");
            stage = match romfs
                .as_deref()
                .and_then(|r| open_movie(r, &BOOT_MOVIES[1], &mut screens.top))
            {
                Some(m) => Stage::Movie(m, Some(t)),
                None => {
                    let mut t = t;
                    t.scene.resume_after_movie();
                    Stage::Title(t)
                }
            };
        } else if finished {
            let previous = std::mem::replace(&mut stage, Stage::Idle);
            if let Stage::Movie(m, _) = &previous {
                eprintln!(
                    "[yw1] {}: {} ({} quadros em {:.2} s)",
                    m.name,
                    if skip { "pulado" } else { "fim" },
                    m.shown,
                    m.clock()
                );
            }
            screens.clear();
            stage = match previous {
                Stage::Loading(l) => match l.room {
                    Some(r) => {
                        window.set_title(&format!("Yo-kai Watch — port nativo (yw1) — {}", l.map));
                        Stage::Room(Box::new(r))
                    }
                    None => Stage::Idle,
                },
                Stage::Movie(_, Some(mut t)) => {
                    t.scene.resume_after_movie();
                    t.clock = Instant::now();
                    Stage::Title(t)
                }
                _ => loop {
                    let Some(spec) = movies.next() else {
                        break match load_title(romfs.as_deref()) {
                            Some(t) => {
                                window
                                    .set_title("Yo-kai Watch — port nativo (yw1) — menu de título");
                                Stage::Title(Box::new(t))
                            }
                            None => Stage::Idle,
                        };
                    };
                    if let Some(m) = romfs
                        .as_deref()
                        .and_then(|r| open_movie(r, spec, &mut screens.top))
                    {
                        break Stage::Movie(m, None);
                    }
                },
            };
        }
        let scale = if matches!(stage, Stage::Room(_)) {
            settings.render_scale
        } else {
            1
        };
        screens.compose_scaled(&mut fb, scale, hi_top.as_deref().filter(|_| scale > 0));
        hi_top = None;
        window
            .update_with_buffer(&fb, WIN_W * scale, WIN_H * scale)
            .expect("atualizar janela");
    }
}
