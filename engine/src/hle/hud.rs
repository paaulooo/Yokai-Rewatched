//! HUD ancorada nas bordas no widescreen (fase G5). Com a tela de cima larga (G3, `widescreen.rs`), o 2D que não é de
//! tela cheia fica no retângulo 5:3 do meio; aqui cada elemento de HUD ganha uma âncora (−1 / 0 / +1) e o renderizador
//! (`render::vulkan::larga_ymap`) o encosta na borda correspondente da imagem larga, à mesma distância que tinha da
//! borda nativa (deslocamento de ∓(W'−400)/2·N no eixo y do alvo).
//!
//! **Confirmado no jogo** (passo 0, rota `route_probe4` a 21:9, `YW_HUD_DIAG`/`YW_HUD_FN`):
//! - Lado: com todo o 2D forçado em −1 (`YW_HUD_ANCORA_FORCAR=-1`) as caixas de diálogo foram para a ESQUERDA da tela:
//!   o y menor do alvo (linhas de memória) é a esquerda da tela de cima. Âncora −1 = borda esquerda, +1 = direita.
//! - O 2D sai do mesmo laço que o 3D: `FUN_0032f4dc` (quadro do renderizador) → `FUN_00323c2c(rend, ctx, camada, …)`
//!   percorre os itens da camada (`*(rend+0x70)`, `*(rend+0x78)` itens, ordenados) e chama o vfunc +0x38 do
//!   desenhista de cada item (`item[0x66]`) com a matriz em `item+4`. Para as malhas (caixas, painéis, ícones) o
//!   vfunc é `FUN_001bb848(desenhista, ctx, matriz, …)` → `FUN_0031e01c`/`FUN_002f89a8` → `FUN_00317030` (o draw).
//!   O texto (fonte em 0x15800900) sai do mesmo laço por outro desenhista (`FUN_002f70c0` → `FUN_00317030`), com a
//!   matriz de modelo no heap (etiqueta `obj`). `FUN_00322acc` desenha outra lista (r0 = lista) e `FUN_00324694` o
//!   véu de tela cheia (shader 0x2d2f398d).
//! - Um "painel" do jogo NÃO é um item só: a caixa do diálogo (desenhista 0x8fe5c28) e o texto dela (matriz 0x8fe5d80)
//!   são itens separados do laço, e o texto cresce letra a letra (caixa x 40..62 → 40..331 em 1 s). Âncora por item
//!   pela caixa do próprio item faria o texto pular de borda enquanto é digitado.
//!
//! **Grupo e âncora** (o que foi implementado):
//! - Lado do jogo: ganchos de entrada/saída em `FUN_00323c2c` (r2 = camada), `FUN_001bb848` (r0 = desenhista) e
//!   `FUN_00322acc` (r0) mantêm uma pilha; no draw (`etiquetas::depois_draw`) o grupo = hash(pilha, matriz de modelo).
//!   Viaja na etiqueta (`EtiqDraw::grupo`) até a thread "gpu".
//! - Mais: o retrato do falante, o balão do nome por cima dele e a caixa do diálogo (que o jogo desenha numa textura,
//!   0x1f000000, e põe na tela como UM quadro de y 0..212·x 0..400 do alvo) são itens distintos que se sobrepõem; o
//!   texto de nome/fala usa textura própria (cache de glifos 0x1f3fd200, desenhada num alvo 400×240 que NÃO é o da tela).
//! - Lado da GPU: só os draws `LARGA_2D` no alvo que o DisplayTransfer da tela de cima lê (`alvo_da_tela`) e com cena
//!   3D no quadro (HUD do jogo; numa tela só 2D — menu, título, nome — a arte fica no 5:3 central e tudo com ela). A
//!   caixa de cada draw no alvo vem do shader de vértice na CPU (`pica::caixa_2d`). No fim do quadro (`fim_quadro` →
//!   `decidir`) os grupos que se SOBREPÕEM na tela viram um conjunto (o elemento), decidido pela união das caixas: o
//!   terço (esquerdo/central/direito dos 400) em que cai o centro dela; larga demais (> 2/3) = centro; com a caixa
//!   inteira, também a parte fora da tela (o painel do tutorial entra deslizando de y −401..31 até −18..414: largo nos
//!   dois = centro, não pula de borda no caminho); 8 px de histerese nas divisas. Caixas de tela inteira (véus,
//!   fundos) não juntam nada. No quadro seguinte (`ancora`) todos os draws do grupo usam essa decisão — a não ser que
//!   o draw tenha saído do lugar (> 24 px da caixa do grupo: o nome do falante que troca de lado); grupo novo, que
//!   mudou de lugar ou draw sem grupo: a do draw já desenhado no quadro que ele sobrepõe, senão a da própria caixa
//!   (vale 1 quadro). [H] um elemento que aparece num quadro já ao lado de outro decide junto com ele só a partir do
//!   2º quadro.
//! - `YW_HUD_ANCORA=0` / `render::set_hud_ancorada(false)`: HUD centralizada como na G3.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

/// `YW_HUD_DIAG=N`: grupos novos do lado do jogo (até N) e as decisões de âncora (lado da GPU, até 8·N).
pub fn diag() -> u64 {
    static E: OnceLock<u64> = OnceLock::new();
    *E.get_or_init(|| std::env::var("YW_HUD_DIAG").ok().and_then(|v| v.parse().ok()).unwrap_or(0))
}

/// As etiquetas do jogo precisam andar (para o grupo de cada draw chegar à thread "gpu").
pub fn quer() -> bool {
    crate::render::hud_ativa() || diag() > 0
}

/// Laço dos itens de uma camada (r2 = camada), desenhista de malha (r0) e a outra lista de desenho (r0).
const FUNCOES: [(u32, usize); 3] = [(0x0032_3c2c, 2), (0x001b_b848, 0), (0x0032_2acc, 0)];

/// Pilha de ganchos (thread do jogo): (função, valor do registrador) das funções em andamento.
static PILHA: Mutex<Vec<(u32, u32)>> = Mutex::new(Vec::new());
static REGISTRADO: AtomicBool = AtomicBool::new(false);

/// `YW_HUD_FN="0xVA:rN,…"` troca as funções da pilha (investigação).
fn funcoes() -> Vec<(u32, usize)> {
    let Ok(v) = std::env::var("YW_HUD_FN") else { return FUNCOES.to_vec() };
    v.split(',')
        .filter_map(|it| {
            let (a, r) = it.trim().split_once(':')?;
            let va = u32::from_str_radix(a.trim().trim_start_matches("0x"), 16).ok()?;
            let r: usize = r.trim().trim_start_matches('r').parse().ok()?;
            (r < 13).then_some((va, r))
        })
        .collect()
}

/// Registra os ganchos (uma vez, na thread do jogo, quando as etiquetas ligam por causa da HUD). Ficam registrados
/// depois (custo: um push/pop por item desenhado); sem HUD ancorada não empilham nada.
pub fn registrar() {
    if REGISTRADO.swap(true, Relaxed) {
        return;
    }
    use crate::hle::hooks::{self, Tipo};
    for (va, r) in funcoes() {
        let a = hooks::registrar(
            va,
            Tipo::antes(move |c| {
                if quer() {
                    let mut p = PILHA.lock().unwrap_or_else(|x| x.into_inner());
                    if p.len() < 64 {
                        p.push((va, c.r[r]));
                    }
                }
            }),
            "hud: entrada de item",
            0,
            false,
        );
        let b = hooks::registrar(
            va,
            Tipo::depois(move |_, _| {
                let mut p = PILHA.lock().unwrap_or_else(|x| x.into_inner());
                if let Some(k) = p.iter().rposition(|x| x.0 == va) {
                    p.truncate(k);
                }
            }),
            "hud: saída de item",
            0,
            false,
        );
        if let Err(e) = a.and(b) {
            eprintln!("[hud] {e}: HUD ancorada só pela posição");
        }
    }
}

/// Grupo de um draw do jogo (thread do jogo, gancho "depois" de `FUN_00317030`): hash da pilha de itens e da matriz
/// de modelo (`obj`, 0 = sem). Nunca 0.
pub fn grupo(e: &crate::hle::hooks::Entrada, obj: u32) -> u32 {
    let p = PILHA.lock().unwrap_or_else(|x| x.into_inner());
    let mut h: u32 = 0x811c_9dc5 ^ obj;
    for &(va, v) in p.iter() {
        h = (h ^ va).wrapping_mul(0x0100_0193);
        h = (h ^ v).wrapping_mul(0x0100_0193);
    }
    let h = h.wrapping_mul(0x0100_0193) | 1;
    if diag() > 0 {
        static VISTAS: OnceLock<Mutex<HashMap<u32, u64>>> = OnceLock::new();
        static N: AtomicU64 = AtomicU64::new(0);
        let mut m = VISTAS.get_or_init(Default::default).lock().unwrap_or_else(|x| x.into_inner());
        let c = m.entry(h).or_insert(0);
        *c += 1;
        if *c == 1 && N.fetch_add(1, Relaxed) < diag() {
            let q = crate::hle::VBLANKS.load(Relaxed);
            let t: Vec<String> = p.iter().map(|(a, v)| format!("{a:#x}:{v:#x}")).collect();
            eprintln!("[hud-grupo] q={q} grupo {h:#010x} lr {:#x} malha {:#x} obj {obj:#x} · {}", e.lr, e.r[1], t.join(" > "));
        }
    }
    h
}

/// `YW_HUD_ANCORA_FORCAR=-1|1`: todo 2D central com essa âncora (passo 0: qual lado da tela é o y menor do alvo).
fn forcar() -> Option<f32> {
    static E: OnceLock<Option<f32>> = OnceLock::new();
    *E.get_or_init(|| std::env::var("YW_HUD_ANCORA_FORCAR").ok().and_then(|v| v.parse().ok()))
}

/// Estado da thread "gpu".
#[derive(Default)]
struct Gpu {
    /// draws 2D do quadro em andamento: (caixa, âncora) — para os draws ainda sem decisão do quadro anterior
    caixas: Vec<([f32; 4], f32)>,
    /// grupos do quadro em andamento, na ordem do 1º draw: (grupo, união das caixas, largura do alvo)
    grupos: Vec<(u32, [f32; 4], f32)>,
    indice: HashMap<u32, usize>,
    /// decisão de cada grupo no fim do quadro anterior: (âncora, união das caixas dele naquele quadro)
    decididas: HashMap<u32, (f32, [f32; 4])>,
    quadro: u64,
    /// alvos de cor que os últimos DisplayTransfers da tela de cima leram (o jogo pode alternar dois)
    alvos: [u32; 2],
}

/// O alvo de cor `addr` é o da tela de cima (lido pelo DisplayTransfer de um dos 2 últimos quadros)? Os outros alvos
/// 400×240 são texturas que o jogo desenha para usar depois (ex.: o nome no balão do retrato) e ficam no centro.
pub fn alvo_da_tela(addr: u32) -> bool {
    addr != 0 && gpu().alvos.contains(&addr)
}

fn gpu() -> std::sync::MutexGuard<'static, Gpu> {
    static G: OnceLock<Mutex<Gpu>> = OnceLock::new();
    G.get_or_init(Default::default).lock().unwrap_or_else(|x| x.into_inner())
}

/// Folga (pixels) em volta das divisas dos terços: um grupo que já tinha âncora só troca se o centro passar dela.
const HISTERESE: f32 = 8.0;
/// Quanto (pixels) o centro do draw pode sair da caixa do grupo no quadro anterior e ainda usar a decisão de lá (o
/// texto que cresce letra a letra anda ~8 px/quadro; o nome do falante que troca de lado anda centenas).
const FOLGA_MOVE: f32 = 24.0;

/// Âncora pela caixa no eixo y do alvo (a horizontal da tela de cima, 0..`h`): −1 = esquerda (y menor), +1 = direita,
/// 0 = centro. Usa a caixa INTEIRA, também a parte fora da tela (um painel que entra deslizando decide igual no
/// caminho e em repouso): larga demais (> 2/3 da tela) = centro; senão o terço em que cai o centro dela. `antes` = a
/// âncora que o grupo tinha (histerese nas divisas).
pub fn ancora_da_caixa(b: [f32; 4], h: f32, antes: Option<f32>) -> f32 {
    let (y0, y1) = (b[1], b[3]);
    if y1 <= y0 || y1 - y0 > h * 2.0 / 3.0 {
        return 0.0;
    }
    let c = (y0 + y1) / 2.0;
    let terco = |c: f32| {
        if c < h / 3.0 {
            -1.0
        } else if c > h * 2.0 / 3.0 {
            1.0
        } else {
            0.0
        }
    };
    let a = terco(c);
    match antes {
        Some(p) if p != a && (terco(c - HISTERESE) == p || terco(c + HISTERESE) == p) => p,
        _ => a,
    }
}

/// Caixa que cobre (quase) a tela inteira: véu, fundo, quadro de vídeo — não junta elementos.
fn tela_inteira(b: [f32; 4], w: f32, h: f32) -> bool {
    let cx = b[2].min(w) - b[0].max(0.0);
    let cy = b[3].min(h) - b[1].max(0.0);
    cx >= w * 0.9 && cy >= h * 0.9
}

fn uniao(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]
}

/// As caixas se sobrepõem (mais que 1 px nos dois eixos): partes do mesmo elemento na tela.
fn sobrepoe(a: [f32; 4], b: [f32; 4]) -> bool {
    a[2].min(b[2]) - a[0].max(b[0]) > 1.0 && a[3].min(b[3]) - a[1].max(b[1]) > 1.0
}

fn dentro(b: [f32; 4], x: f32, y: f32, folga: f32) -> bool {
    b[0] - folga <= x && x <= b[2] + folga && b[1] - folga <= y && y <= b[3] + folga
}

/// Junta grupos que se sobrepõem na tela (um elemento do jogo é vários itens: o retrato, o balão do nome por cima
/// dele, o texto do nome, a caixa do diálogo que encosta no balão) e decide cada conjunto pela união das caixas.
/// Entra: (união do grupo, se cobre a tela inteira, âncora anterior); sai: a âncora de cada um, na mesma ordem.
fn decidir(grupos: &[([f32; 4], bool, Option<f32>)], h: f32) -> Vec<f32> {
    let n = grupos.len();
    let mut pai: Vec<usize> = (0..n).collect();
    fn raiz(p: &mut [usize], mut i: usize) -> usize {
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    for i in 0..n {
        if grupos[i].1 {
            continue;
        }
        for j in 0..i {
            if !grupos[j].1 && sobrepoe(grupos[i].0, grupos[j].0) {
                let (a, b) = (raiz(&mut pai, i), raiz(&mut pai, j));
                pai[a.max(b)] = a.min(b);
            }
        }
    }
    // união e âncora anterior do conjunto (a do 1º membro que tinha)
    let mut u: HashMap<usize, ([f32; 4], Option<f32>)> = HashMap::new();
    for i in 0..n {
        let r = raiz(&mut pai, i);
        let e = u.entry(r).or_insert((grupos[i].0, grupos[i].2));
        e.0 = uniao(e.0, grupos[i].0);
        e.1 = e.1.or(grupos[i].2);
    }
    (0..n)
        .map(|i| {
            if grupos[i].1 {
                return 0.0;
            }
            let (b, antes) = u[&raiz(&mut pai, i)];
            ancora_da_caixa(b, h, antes)
        })
        .collect()
}

/// Âncora de um draw 2D (`LARGA_2D`) da tela de cima com a HUD ancorada (thread "gpu", `pica::draw`).
/// `caixa` = (x0, y0, x1, y1) no alvo (None = não calculada). Grupo visto no quadro anterior (e o draw ainda no mesmo
/// lugar): a decisão de lá (todos os draws do grupo e dos que o tocam juntos; a interpolação reproduz pelo `Ctx`).
/// Grupo novo, que mudou de lugar ou sem grupo: o do draw do quadro que ele sobrepõe, senão pela caixa dele.
pub fn ancora(ctx: &crate::pica::raster::Ctx, caixa: Option<[f32; 4]>) -> f32 {
    if let Some(a) = forcar() {
        return a;
    }
    let grupo = crate::hle::etiquetas::draw_atual().et.map_or(0, |e| e.grupo);
    let (w, h) = (ctx.width as f32, (ctx.height_m1 + 1) as f32);
    let mut g = gpu();
    let fixo = match (grupo, caixa) {
        (0, _) => None,
        (_, Some(c)) => g.decididas.get(&grupo).filter(|d| dentro(d.1, (c[0] + c[2]) / 2.0, (c[1] + c[3]) / 2.0, FOLGA_MOVE)).map(|d| d.0),
        (_, None) => g.decididas.get(&grupo).map(|d| d.0),
    };
    let (a, origem) = match (fixo, caixa) {
        (Some(a), _) => (a, "grupo"),
        (None, Some(c)) => {
            let viz = if tela_inteira(c, w, h) { None } else { g.caixas.iter().rev().find(|(b, _)| sobrepoe(*b, c)).map(|x| x.1) };
            match viz {
                Some(a) => (a, "vizinho"),
                None => (ancora_da_caixa(c, h, None), "posição"),
            }
        }
        (None, None) => (0.0, "sem caixa"),
    };
    if let Some(c) = caixa {
        if !tela_inteira(c, w, h) && g.caixas.len() < 512 {
            g.caixas.push((c, a));
        }
        if grupo != 0 {
            match g.indice.get(&grupo).copied() {
                Some(k) => g.grupos[k].1 = uniao(g.grupos[k].1, c),
                None if g.grupos.len() < 512 => {
                    let k = g.grupos.len();
                    g.grupos.push((grupo, c, w));
                    g.indice.insert(grupo, k);
                }
                None => {}
            }
        }
    }
    if diag() > 0 {
        static N: AtomicU64 = AtomicU64::new(0);
        static VISTOS: OnceLock<Mutex<std::collections::HashSet<(u32, u32, [i32; 4])>>> = OnceLock::new();
        let t = ctx.tex[0].as_ref().map_or(0, |t| t.addr);
        let chave = (grupo, t, caixa.map_or([0; 4], |b| b.map(|v| (v / 16.0).round() as i32)));
        let novo = VISTOS.get_or_init(Default::default).lock().unwrap_or_else(|x| x.into_inner()).insert(chave);
        if novo && N.fetch_add(1, Relaxed) < diag() * 8 {
            eprintln!(
                "[hud] q={} quadro {} grupo {grupo:#010x} caixa {:?} → âncora {a} ({origem}) · tex0 {t:#x}",
                crate::hle::VBLANKS.load(Relaxed),
                g.quadro,
                caixa.map(|b| b.map(|v| v.round()))
            );
        }
    }
    a
}

/// Fim do quadro da tela de cima (DisplayTransfer de 400 linhas, `render::present_hires_check`; `alvo` = o alvo de
/// cor que ele leu): decide a âncora de cada grupo do quadro (`decidir`) para o quadro seguinte. Grupos que não
/// apareceram são esquecidos.
pub fn fim_quadro(alvo: u32) {
    let mut g = gpu();
    if g.alvos[0] != alvo {
        g.alvos = [alvo, g.alvos[0]];
    }
    let antigas = std::mem::take(&mut g.decididas);
    let entrada: Vec<([f32; 4], bool, Option<f32>)> =
        g.grupos.iter().map(|&(id, u, w)| (u, tela_inteira(u, w, 400.0), antigas.get(&id).map(|d| d.0))).collect();
    let saida = decidir(&entrada, 400.0);
    let mut novas = HashMap::with_capacity(g.grupos.len());
    for (k, &(id, u, _)) in g.grupos.iter().enumerate() {
        novas.insert(id, (saida[k], u));
        if diag() > 0 {
            if let Some(&(p, _)) = antigas.get(&id) {
                if p != saida[k] {
                    eprintln!("[hud] quadro {} grupo {id:#010x} troca de âncora {p} → {} (caixa {:?})", g.quadro, saida[k], u.map(|v| v.round()));
                }
            }
        }
    }
    g.decididas = novas;
    g.grupos.clear();
    g.indice.clear();
    g.caixas.clear();
    g.quadro += 1;
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn hud_ancora_por_tercos() {
        // banner no canto esquerdo, ícone à direita, caixa de diálogo de borda a borda, item do meio
        assert_eq!(ancora_da_caixa([0.0, 0.0, 30.0, 120.0], 400.0, None), -1.0);
        assert_eq!(ancora_da_caixa([20.0, 365.0, 66.0, 385.0], 400.0, None), 1.0);
        assert_eq!(ancora_da_caixa([8.0, 5.0, 144.0, 395.0], 400.0, None), 0.0);
        assert_eq!(ancora_da_caixa([100.0, 150.0, 140.0, 250.0], 400.0, None), 0.0);
        // a caixa inteira conta, também fora da tela: o painel do tutorial que entra pela esquerda (medido: y −401..31
        // no 1º quadro, −18..414 em repouso) é largo nos dois casos = centro; um banner que entra pela esquerda fica à
        // esquerda no caminho e em repouso
        assert_eq!(ancora_da_caixa([0.0, -401.0, 416.0, 31.0], 400.0, None), 0.0);
        assert_eq!(ancora_da_caixa([0.0, -150.0, 30.0, 0.0], 400.0, None), -1.0);
        // histerese: perto da divisa (133,3) quem já era −1 continua
        assert_eq!(ancora_da_caixa([0.0, 100.0, 30.0, 175.0], 400.0, None), 0.0);
        assert_eq!(ancora_da_caixa([0.0, 100.0, 30.0, 175.0], 400.0, Some(-1.0)), -1.0);
        assert_eq!(ancora_da_caixa([0.0, 120.0, 30.0, 200.0], 400.0, Some(-1.0)), 0.0);
    }

    #[test]
    fn hud_tela_inteira_nao_junta() {
        assert!(tela_inteira([0.0, 0.0, 240.0, 400.0], 240.0, 400.0));
        assert!(tela_inteira([-480.0, -800.0, 720.0, 1200.0], 240.0, 400.0));
        assert!(!tela_inteira([8.0, 5.0, 144.0, 395.0], 240.0, 400.0));
    }

    #[test]
    fn hud_conjuntos_por_sobreposicao() {
        // medido na rota (diálogo com retrato à direita): retrato, caixa do diálogo (desenhada numa textura e posta
        // como um quadro de 0..212), o nome por cima do retrato e o texto dentro da caixa: tudo um elemento só, largo
        // = centro (antes o retrato ia para a direita e o balão do nome ficava no meio: partido)
        let g = [
            ([144.0, 272.0, 400.0, 400.0], false, None),
            ([0.0, 0.0, 212.0, 400.0], false, None),
            ([150.0, 307.0, 172.0, 366.0], false, None),
            ([84.0, 41.0, 114.0, 64.0], false, None),
        ];
        assert_eq!(decidir(&g, 400.0), vec![0.0; 4]);
        // isolados: ícone à direita e banner à esquerda; um véu de tela cheia não os junta
        let g = [
            ([0.0, 0.0, 240.0, 400.0], true, None),
            ([20.0, 365.0, 66.0, 385.0], false, None),
            ([0.0, 0.0, 30.0, 120.0], false, None),
            ([5.0, 10.0, 25.0, 60.0], false, None),
        ];
        assert_eq!(decidir(&g, 400.0), vec![0.0, 1.0, -1.0, -1.0]);
    }
}
