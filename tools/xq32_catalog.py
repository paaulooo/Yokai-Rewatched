"""Catalogo de opcodes dos scripts XQ32 (seq/**/*.xq) + desmontagem com nomes globais resolvidos.

Uso (de qualquer pasta):
  python xq32_catalog.py catalog [--glob PREFIXO ...]   -> work/notes/xq32_opcodes.md e .json
  python xq32_catalog.py dis seq/event/ev01_0005.xq      -> desmonta com hashes de funcao resolvidos (nomes globais)
  python xq32_catalog.py dumpall                         -> work/xq/*.txt (todos os scripts desmontados, resolvidos)

Resolucao de nomes: cada .xq so traz o nome das proprias funcoes/strings; `call(#hash)` para funcao de outro
arquivo aparece como hash. Aqui montamos um dicionario global crc32(nome) -> nome com TODAS as funcoes e strings
de todos os .xq (prog_common_001e.xq contem a maior parte das funcoes de biblioteca).
"""
import sys, os, csv, json, collections, struct, zlib
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import xq32_disasm as X
from paths import FA, ROOT, WORK

NOTES = WORK / 'notes'

_rows = None
def rows():
    global _rows
    if _rows is None:
        _rows = {r['path']: r for r in csv.DictReader(open(ROOT / 'data' / 'romfs' / 'yw1_a_files.csv'))}
    return _rows

def read_fa(path):
    r = rows()[path]
    with open(FA, 'rb') as f:
        f.seek(int(r['offset_in_romfs_fa'])); return f.read(int(r['size']))

_cache = {}
def load_all():
    """parse de todos os .xq (cache)."""
    if _cache: return _cache
    with open(FA, 'rb') as f:
        for p, r in rows().items():
            if not p.endswith('.xq'): continue
            f.seek(int(r['offset_in_romfs_fa'])); d = f.read(int(r['size']))
            try: _cache[p] = X.parse(d)
            except Exception: pass
    return _cache

_gnames = None
def global_names():
    global _gnames
    if _gnames is None:
        g = {}
        for p, x in load_all().items():
            for k, v in x['names'].items(): g.setdefault(k, v)
            for f in x['funcs']:
                nm = x['s'](f[0]); g[zlib.crc32(nm.encode('shift-jis', 'replace')) & 0xffffffff] = nm
        _gnames = g
    return _gnames

NATIVE_NAMES = {}  # preenchido pelo dicionario curado em OPDOC (abaixo)

def fmt_arg(x, t, v, g):
    if t == 2:
        n = x['names'].get(v & 0xffffffff) or g.get(v & 0xffffffff)
        return '#%s' % n if n else '#%08x' % (v & 0xffffffff)
    return X.fmt_arg(x, t, v)

def dis(x, g):
    labels = {}
    for j in x['jumps']:
        labels.setdefault(j[2], []).append(x['s'](j[0]))
    out = []
    for f in sorted(x['funcs'], key=lambda f: f[2]):
        out.append('\nfunc %s  (instr %d..%d)' % (x['s'](f[0]), f[2], f[3]))
        for i in range(f[2], f[3]):
            for lb in labels.get(i, []): out.append('  %s:' % lb)
            a0, ac, ret, op, _ = x['insts'][i]
            args = ', '.join(fmt_arg(x, *x['args'][a0 + k], g) for k in range(ac))
            nm = 'op%d' % op
            if op == 20:
                nm = 'call'
            elif op in OPDOC:
                nm = OPDOC[op][0] + '#' + str(op)
            out.append('  %4d  $%-5d = %s(%s)' % (i, ret, nm, args))
    return '\n'.join(out)

# ---------------------------------------------------------------------------------------------
# Dicionario curado: opcode -> (nome curto, assinatura, significado, status)
# status: C = confirmado (code.bin ou comportamento inequivoco no script), H = hipotese
# ---------------------------------------------------------------------------------------------
OPDOC = {}
def _d(op, name, sig, desc, st='H'):
    OPDOC[op] = (name, sig, desc, st)
_d(10, 'yield', '()', 'espera 1 quadro (cede o controle ao escalonador)', 'C')
_d(11, 'return', '(v)', 'retorna v da funcao (grava em $1000)', 'C')
_d(12, 'yield_n', '(n)', 'espera n quadros', 'H')
_d(13, 'time', '()', 'le contador de tempo/quadro', 'H')
_d(20, 'call', '(#func, args...)', 'chama funcao por hash CRC-32 (do proprio script ou da biblioteca/engine)', 'C')
_d(30, 'jmp_if', '(#label, cond)', 'salta se cond != 0', 'C')
_d(31, 'jmp', '(#label)', 'salto incondicional', 'C')
_d(33, 'jmp_ifnot', '(#label, cond)', 'salta se cond == 0', 'C')
_d(100, 'mov', '(v)', 'atribui v ao destino', 'C')
_d(120, 'not', '(a)', 'negacao logica', 'C')
_d(121, 'or', '(a,b)', 'ou logico (|| sem curto-circuito)', 'H')
_d(122, 'and', '(a,b)', 'e logico', 'H')
_d(130, 'eq', '(a,b)', 'a == b', 'C')
_d(131, 'ne', '(a,b)', 'a != b', 'C')
_d(132, 'lt', '(a,b)', 'a < b', 'H')
_d(133, 'le', '(a,b)', 'a <= b', 'H')
_d(134, 'gt', '(a,b)', 'a > b', 'C')
_d(135, 'ge', '(a,b)', 'a >= b', 'H')
_d(150, 'add', '(a,b)', 'a + b', 'H')
_d(151, 'sub', '(a,b)', 'a - b', 'H')
_d(152, 'mul', '(a,b)', 'a * b', 'H')
_d(153, 'div', '(a,b)', 'a / b', 'H')
_d(160, 'and_bits', '(a,b)', 'a & b (testa flag em mascara, p.ex. 1024)', 'H')
_d(240, 'inc', '()', 'incrementa a variavel destino', 'C')
_d(251, 'neg_or_sign', '(a)', 'unario (usado em WaitFrame2)', 'H')
_d(500, 'log', '(args...)', 'debug-print / "parametros" (descarta); 1 str = texto de log (ex: "==== Event End ====")', 'H')
_d(501, 'sprintf', '(fmt, args...)', 'formata string estilo printf, devolve string', 'H')
_d(522, 'strdup', '(s)', 'copia/guarda string em variavel', 'H')
_d(2102, 'scene_pre_exit', '()', 'handler em 0x16698c: fecha o menu/cena atual antes de 2104 (le fila de eventos)', 'H')
_d(2104, 'scene_change', '(sceneId, back=-1, args...)', 'troca de cena (ver cenas 1010/2xxx). Handler em FUN_0016697c -> FUN_0032c350; args extras lidos pela cena nova via op2212(n)', 'C')
_d(2106, 'get_fade_state', '()', 'le estado de fade/ocultacao da tela (salvo e restaurado via 2109)', 'H')
_d(2109, 'screen_cover', '(on)', 'liga/desliga cobertura/bloqueio de entrada da tela durante transicoes (1 antes de montar, 0 apos fade)', 'H')
_d(2110, 'event_cmd', '(sub, args...)', 'DESPACHO de comandos de evento/cutscene: 1o arg = subcodigo (11004 cria objeto, 14007 anima, 14016 posiciona...); ver tabela de subcodigos', 'C')
_d(2190, 'menu_lock_on', '()', 'bloqueia a camada de menu (par com 2191)', 'H')
_d(2191, 'menu_lock_off', '()', 'libera a camada de menu', 'H')
_d(2193, 'menu_slide_in', '(side,flag)', 'anima entrada de tela', 'H')
_d(2194, 'menu_slide_end', '(side,flag)', 'fim da animacao', 'H')
_d(2211, 'spawn', '(#funcName, args...)', 'dispara funcao do script em paralelo (corrotina)', 'C')
_d(2212, 'scene_arg', '(n)', 'le o n-esimo argumento passado a cena corrente por 2104 (indice 3 = 1o extra: 2111 -> flag "novo jogo")', 'H')
_d(2502, 'cam_move', '(id, x, frames)', 'move/anima objeto de camera 2D', 'H')
_d(2512, 'timer_set', '(f, frames)', 'ajusta valor ao longo de frames', 'H')
_d(3010, 'bg_mode', '(n)', 'modo de fundo do menu', 'H')
_d(3011, 'screen_in', '(s,flag)', 'mostra tela', 'H')
_d(3012, 'screen_out', '(s,flag)', 'esconde tela', 'H')
_d(3402, 'load_img', '(res, path)', 'carrega imagem (.xi) em recurso nomeado', 'C')
_d(3502, 'load_anim', '(res, path)', 'carrega pacote de animacao 2D (.xc/.xa) em recurso nomeado', 'C')
_d(16526, 'load_anim_lg', '(res, path<LG>)', 'idem 3502 com sufixo de idioma <LG>', 'H')
_d(4002, 'cam_create', '(name, flags)', 'cria camera 2D', 'H')
_d(4010, 'cam_bind', '(screen, name)', 'liga camera a tela', 'H')
_d(5001, 'obj_free', '(obj)', 'destroi objeto 2D', 'C')
_d(5002, 'obj_new', '(name, screen, prio, ...)', 'cria objeto de UI/animacao 2D (handler 0x168b6c). screen 0=cima 1=baixo', 'C')
_d(5005, 'obj_show', '(obj, flag)', 'visivel/oculto', 'H')
_d(5006, 'obj_hold', '(obj, flag)', 'flag de pausa/travamento do objeto', 'H')
_d(5009, 'obj_exists', '(obj)', 'existe objeto com esse nome', 'H')
_d(5013, 'obj_parent', '(obj, parent)', 'define pai', 'H')
_d(5100, 'obj_fading', '(obj)', 'fade de alfa em andamento', 'H')
_d(5102, 'obj_alpha', '(obj, a)', 'define alfa', 'C')
_d(5103, 'obj_alpha_to', '(obj, a, frames)', 'fade de alfa', 'C')
_d(5242, 'obj_pos', '(obj, x, y)', 'posicao', 'H')
_d(5247, 'obj_pos_add', '(obj, dx, dy)', 'deslocamento relativo', 'H')
_d(5600, 'obj_set_res', '(obj, res)', 'associa recurso de animacao ao objeto', 'H')
_d(5622, 'obj_play', '(obj, anim, flags)', 'toca animacao nomeada (flags: 2 = loop?, 6/0xc = sair)', 'C')
_d(13609, 'map_warp', '(map, x, y, z, ...)', 'teleporta para mapa (usado em RunCmd_Map1)', 'H')
_d(13231, 'trigger_area', '(id, x, y, z, r)', 'cria gatilho/zona (nº, posicao, raio 150)', 'H')
_d(14003, 'flag_set', '(#flag, v)', 'grava flag/variavel global (hash do nome)', 'H')
_d(14004, 'flag_get', '(#flag|name)', 'le flag/variavel global', 'H')
_d(14800, 'bgm_prepare', '(name)', 'carrega/prepara musica (M01_MAIN_THEME...) devolve handle', 'C')
_d(14804, 'se_play', '(id)', 'toca efeito sonoro de menu', 'H')
_d(14805, 'bgm_is_loading', '(h)', 'musica ainda carregando?', 'H')
_d(14806, 'bgm_play', '(h)', 'toca musica', 'C')
_d(14807, 'bgm_is_fading', '(h)', 'fade de musica em andamento', 'H')
_d(14808, 'bgm_fade_out', '(h, frames)', 'fade out', 'C')
_d(14809, 'bgm_fade_in', '(h, frames)', 'fade in/stop', 'H')
_d(14810, 'bgm_current', '()', 'handle da musica corrente', 'H')
_d(15150, 'player_get_type', '()', 'FUN_003cda6c -> FUN_002b8e9c: devolve (playerdata+0x48)&3 = tipo do protagonista (1 = menino/Nate, 2 = menina/Katie; CharaMakeMenu_GetPlayerType)', 'C')
_d(15155, 'player_set_name', '(name)', 'FUN_003ce960: copia string (max 0x20 B) para playerdata+0x1c (DAT_005c6a08+0x10)', 'C')
_d(15156, 'player_set_type', '(t)', 'FUN_003cea14: grava t&3 em playerdata+0x48 (bits 0-1). Chamado com CharaMakeMenu_GetPlayerType no fim do NameMenu 2070', 'C')
_d(13608, 'event_start', '(evName, perVersion)', 'FUN_003b10e8: inicia seq/event/<evName>.xq; perVersion=1 escolhe sufixo a/b pelo tipo do jogador (RunEventEachVersion)', 'H')
_d(13623, 'event_running', '()', 'FUN_003a32a4: 1 enquanto ha evento em execucao (RunEvent faz spin com op30)', 'H')
_d(14000, 'flag_set', '(key, v=1)', 'FUN_003ea038 -> FUN_002ae46c(key,3,v): marca flag nomeada (RunEvent grava 14000(evName,1) = evento concluido)', 'C')
_d(13609, 'map_warp', '(map, x, y, z, rotY, ?, fade)', 'FUN_003d537c: teleporta o jogador/lider para o mapa (t101g00...) na posicao dada', 'H')

# subcodigos de op2110 (event_cmd) - significado inferido do uso em ev00/ev01
SUB2110 = {
    11004: ('obj_load', '(3, nome, caminho.xc, ...)', 'carrega modelo/efeito/mapa (.xc) como objeto de cena nomeado', 'H'),
    11006: ('obj_motion_load', '(nome, caminho.xc, slot)', 'carrega movimento (pontos/animacao) para o objeto no slot', 'H'),
    11007: ('obj_motion_free', '(nome, slot)', 'libera o slot de movimento', 'H'),
    11008: ('obj_free', '(nome)', 'destroi o objeto', 'H'),
    11013: ('obj_prepare', '(nome, 0)', 'prepara/instancia', 'H'),
    11046: ('obj_attach', '(nome, pai, ponto)', 'prende objeto a ponto nomeado (evp_*) de outro', 'H'),
    14007: ('obj_motion_play', '(nome, anim, slot, 0)', 'toca movimento carregado', 'H'),
    14010: ('obj_visible', '(nome, 0/1)', 'mostra/oculta objeto', 'H'),
    14016: ('obj_place', '(nome, 0, x, y, z, rotY)', 'posiciona/gira (graus)', 'H'),
    14020: ('obj_scale', '(nome, sx, sy, sz)', 'escala / offset', 'H'),
    16026: ('input_lock', '(0/1)', 'trava/libera a entrada do jogador durante cena', 'H'),
    27003: ('light_bind', '(luz, nome)', 'associa arquivo .xk de iluminacao a um objeto de cena', 'H'),
    27005: ('light_apply', '(luz, -1, 1)', 'ativa iluminacao', 'H'),
    10010: ('wait_frames_neg', '(-n)', 'WaitFrame2: espera n quadros considerando velocidade', 'H'),
    1200: ('name_get', '(buf)', 'NameMenu: le nome atual', 'H'),
    1300: ('name_chk', '(1)', 'NameMenu: valida', 'H'),
    1301: ('name_chk2', '(buf)', 'NameMenu: valida string', 'H'),
}

# --- opcodes nativos inferidos pelo uso nos scripts do prologo (H = hipotese) ---
_d(1000, 'pad_any', '()', 'alguma tecla/toque novo neste quadro (TitleMenu_Start espera ate o logo ou entrada)', 'H')
_d(1103, 'pad_down', '(btn)', 'botao btn pressionado (MovieCheckSkip usa 4 e 6, mais 1203 = toque)', 'H')
_d(1110, 'pad_bind', '(h, n, #func, args...)', 'registra callback de pad/toque', 'H')
_d(1112, 'pad_unbind', '(h)', 'remove callback', 'H')
_d(1113, 'pad_query', '(h)', 'le estado do callback', 'H')
_d(3420, 'res_get_a', '(resName)', 'obtem recurso carregado (par com 3421)', 'H')
_d(3421, 'res_get_b', '(resName)', 'idem', 'H')
_d(3544, 'font_set', '(res, name, w, h)', 'associa fonte/atlas de texto a recurso', 'H')
_d(3702, 'load_model', '(res, path, flags)', 'carrega modelo 3D em recurso nomeado (CharaMakeMenu: data/character/c001008/c001008_p00)', 'H')
_d(5008, 'obj_enable', '(obj, on)', 'ativa/desativa objeto', 'H')
_d(5045, 'obj_get_anim', '(obj, n)', 'devolve nome da animacao n do objeto', 'H')
_d(5046, 'obj_set_param', '(obj, idx, vals...)', 'parametros do objeto (cor/escala)', 'H')
_d(5051, 'obj_rect', '(obj, flag, x, y, w, h)', 'retangulo de toque do objeto', 'H')
_d(5202, 'obj_scale', '(obj, sx, sy)', 'escala', 'H')
_d(5248, 'obj_pos_to', '(obj, dx, dy, frames, ease)', 'move relativo ao longo de frames', 'H')
_d(5250, 'obj_front', '(obj)', 'traz para frente', 'H')
_d(5252, 'obj_update', '(obj)', 'aplica/atualiza transformacao', 'H')
_d(5262, 'obj_rot', '(obj, ang)', 'rotacao', 'H')
_d(5322, 'screen_fx_a', '(scr, id, v)', 'efeito de tela por id', 'H')
_d(5323, 'screen_fx_b', '(scr, id, v, frames, 0)', 'efeito de tela animado', 'H')
_d(5325, 'screen_fx_c', '(scr, id, 0)', 'efeito de tela', 'H')
_d(5430, 'obj_set_img', '(obj, res, a, b)', 'associa imagem a objeto', 'H')
_d(5623, 'obj_play2', '(obj, anim, flag[, blend])', 'toca animacao (busto de dialogo)', 'H')
_d(5910, 'text_clear', '(obj)', 'limpa texto do objeto', 'H')
_d(5911, 'text_set', '(obj, str[, a, b])', 'define o texto exibido', 'H')
_d(5932, 'text_scale_x', '(obj, s)', 'escala de texto X', 'H')
_d(5933, 'text_scale_y', '(obj, s)', 'escala de texto Y', 'H')
_d(5942, 'text_align_x', '(obj, a)', 'alinhamento', 'H')
_d(5943, 'text_align_y', '(obj, a)', 'alinhamento', 'H')
_d(6002, 'obj3_param', '(obj, 0, 130, 0)', 'parametro de objeto 3D de menu', 'H')
_d(6046, 'obj3_set', '(obj, idx, v)', 'parametro de objeto 3D de menu', 'H')
_d(12054, 'lens_state', '(n, 0)', 'estado da lente/slot (TitleMenu_UpdateLensEnable testa &1024)', 'H')
_d(13135, 'ui_lock', '(on)', 'bloqueio de UI', 'H')
_d(13229, 'map_set_start', '(1, x, y, z)', 'define ponto de spawn do mapa antes de montar (seguido de WaitBuildMap)', 'H')
_d(13650, 'text_arg', '(h, i)', 'le argumento de texto', 'H')
_d(13730, 'fmt_tag_a', '(s, i, TAG, v)', 'substitui <TAG> em string (LOCATION, HOUR...)', 'H')
_d(13760, 'talk_get_line', '(textGroup, idx)', 'devolve a linha idx do grupo de texto (EventTalkRun; "" = fim)', 'H')
_d(13761, 'talk_open_line', '(group, idx, ...)', 'abre janela de fala para a linha', 'H')
_d(13790, 'fmt_name', '(s, TAG, v)', 'substitui <ITEM_NAME> etc.', 'H')
_d(13791, 'fmt_tag', '(s, type, TAG, v)', 'substitui <TAG> (CHARA_NAME, NICK_NAME, ITEM_NUM)', 'H')
_d(15138, 'party_query', '(i, 1)', 'consulta equipe', 'H')
_d(16525, 'text_font', '(obj)', 'fonte padrao do objeto de texto', 'H')
_d(17000, 'text_flags', '(obj, 512|1024)', 'flags de texto', 'H')
_d(531, 'array_index', '(a, i)', 'indexa vetor/tabela', 'H')
SUB2110.update({
    28008: ('path_begin', '()', 'inicia lista de waypoints', 'H'),
    28009: ('path_point', '(x, y, z, frames)', 'adiciona waypoint', 'H'),
    28010: ('path_run', '(nome, vel, 0, anim, ...)', 'faz o personagem percorrer o caminho', 'H'),
    11016: ('obj_turn', '(nome, ponto, 1, ang)', 'orienta objeto', 'H'),
    11017: ('obj_offset', '(nome, a, b, c)', 'offset/escala de objeto de evento', 'H'),
    11030: ('emote', '(nome, "emuNNNN", 1, ...)', 'balao de emocao (emu0001, emu0003, emu0016)', 'H'),
    11061: ('obj_active', '(nome, 0/1)', 'ativa/desativa personagem', 'H'),
    11062: ('obj_mode', '(nome, 2)', 'modo de objeto', 'H'),
    16023: ('event_begin', '()', 'trava controle do jogador, inicia evento visual', 'H'),
    16029: ('first_cut', '()', 'StartFirstCut: libera a 1a tomada (fim do WaitBuild)', 'H'),
    16031: ('setup_visual', '()', 'SetupVisualEvent', 'H'),
    16035: ('setup_seamless', '()', 'SetupSeamlessEvent: evento sem corte de mapa', 'H'),
    15203: ('cam_reset', '()', 'reinicia camera de evento', 'H'),
    13000: ('pic_load', '(nome, caminho.xa, 0, 5)', 'carrega ilustracao (ev_pic)', 'H'),
    13003: ('pic_frame', '(nome, "01")', 'seleciona quadro da ilustracao', 'H'),
    11052: ('skin_load', '(nome, arquivo.skinwatch, 0)', 'carrega skin (relogio no braco)', 'H'),
    14019: ('obj_flag', '(nome, 1, 0)', 'flag de objeto', 'H'),
    27006: ('prop_attach', '(nome)', 'liga adereco (rede, gaiola) a mao do personagem', 'H'),
    12011: ('time_scale', '(0.2, 0)', 'escala de tempo', 'H'),
    14000: ('wait_load', '(-1)', 'WaitBuild: consulta carregamento pendente (subcodigo dentro do 2110)', 'H'),
})


def sig_of(x, a0, ac):
    return ''.join({1: 'i', 2: 'h', 3: 'f', 4: 'v', 24: 's'}.get(x['args'][a0 + k][0], '?') for k in range(ac))


def cmd_catalog(prefixes):
    ALL = load_all()
    g = global_names()
    stats = collections.defaultdict(lambda: dict(n=0, files=set(), sigs=collections.Counter(), ex=None, rets=0))
    sub = collections.defaultdict(lambda: dict(n=0, files=set(), sigs=collections.Counter(), ex=None))
    nfun = 0
    for p, x in ALL.items():
        if prefixes and not any(p.startswith(q) for q in prefixes): continue
        for fn in x['funcs']:
            nfun += 1
        for (a0, ac, ret, op, _) in x['insts']:
            s = stats[op]; s['n'] += 1; s['files'].add(p)
            s['sigs'][sig_of(x, a0, ac)] += 1
            if s['ex'] is None:
                s['ex'] = (p, ', '.join(fmt_arg(x, *x['args'][a0 + k], g) for k in range(ac)))
            if op == 2110 and ac:
                t, v = x['args'][a0]
                if t == 1:
                    q = sub[v]; q['n'] += 1; q['files'].add(p)
                    q['sigs'][sig_of(x, a0 + 1, ac - 1)] += 1
                    if q['ex'] is None:
                        q['ex'] = (p, ', '.join(fmt_arg(x, *x['args'][a0 + k], g) for k in range(1, ac)))
    handlers = {}
    hp = NOTES / 'opcode_handlers_raw.json'
    if hp.exists(): handlers = json.load(open(hp))
    out = ['# Catalogo de opcodes XQ32', '',
           'Gerado por `tools/xq32_catalog.py catalog` sobre %d scripts%s (%d funcoes).' % (
               sum(1 for p in ALL if not prefixes or any(p.startswith(q) for q in prefixes)),
               (' (prefixos %s)' % prefixes) if prefixes else '', nfun), '',
           'Status: **C** confirmado (code.bin/uso inequivoco), **H** hipotese (inferido do uso nos scripts). '
           'Assinatura de argumentos: i=int h=hash s=string f=float v=variavel. '
           '`handler` = VA da funcao nativa na tabela (fn,id) do code.bin (hipotese estrutural, ver xq32_start_sequence.md).', '',
           '| op | nome | n | arquivos | assinaturas mais comuns | handler | sig/semantica | st |', '|---|---|---|---|---|---|---|---|']
    js = {}
    for op in sorted(stats):
        s = stats[op]
        doc = OPDOC.get(op)
        h = ','.join(handlers.get(str(op), [])[:2])
        sg = ' ; '.join('`%s`x%d' % (k or '()', v) for k, v in s['sigs'].most_common(3))
        out.append('| %d | %s | %d | %d | %s | %s | %s | %s |' % (
            op, doc[0] if doc else '', s['n'], len(s['files']), sg, h,
            (doc[1] + ' ' + doc[2]) if doc else '(sem doc)  ex: %s(%s)' % (op, (s['ex'][1][:60] if s['ex'] else '')),
            doc[3] if doc else ''))
        js[op] = dict(count=s['n'], files=len(s['files']), sigs=dict(s['sigs'].most_common(5)), handler=handlers.get(str(op), []),
                      name=doc[0] if doc else None, desc=doc[2] if doc else None, status=doc[3] if doc else None,
                      example=s['ex'])
    out += ['', '## Subcodigos de op2110 (event_cmd)', '',
            '| sub | nome | n | arquivos | assinaturas | exemplo | semantica | st |', '|---|---|---|---|---|---|---|---|']
    for sc in sorted(sub):
        q = sub[sc]; d = SUB2110.get(sc)
        out.append('| %d | %s | %d | %d | %s | %s | %s | %s |' % (
            sc, d[0] if d else '', q['n'], len(q['files']),
            ' ; '.join('`%s`x%d' % (k or '()', v) for k, v in q['sigs'].most_common(2)),
            '`%s`' % (q['ex'][1][:70] if q['ex'] else ''), (d[1] + ' ' + d[2]) if d else '', d[3] if d else ''))
    NOTES.mkdir(parents=True, exist_ok=True)
    (NOTES / 'xq32_opcodes.md').write_text('\n'.join(out), encoding='utf-8')
    json.dump(js, open(NOTES / 'xq32_opcodes.json', 'w'), indent=1, default=str)
    print('ok:', len(stats), 'opcodes,', len(sub), 'subcodigos 2110')


LIBS = ['seq/prog_common_001e.xq', 'seq/prog_menu_001e.xq']

# raizes do 'New Game' ate o controle no quarto: (arquivo, funcao)
ROOTS = [
    ('seq/menu/TitleMenu.xq', 'Menu_TitleToNewMain'), ('seq/menu/SaveLoadMenu_001e.xq', 'SaveLoadMenu'),
    ('seq/menu/CharaMakeMenu_001e.xq', 'CharaMakeMenu'), ('seq/menu/NameMenu_001e.xq', 'NameMenu'),
    ('seq/phs/c01_trigger_0.01.xq', 'RunCmd_Phase1'), ('seq/phs/c01_trigger_0.01.xq', 'RunCmd_Phase2'),
    ('seq/event/ev01_0003.xq', 'Main'), ('seq/event/ev01_0005a.xq', 'Main'), ('seq/event/ev01_0005b.xq', 'Main'),
    ('seq/event/ev01_0010a.xq', 'Main'), ('seq/event/ev01_0010b.xq', 'Main'),
    ('seq/event/ev01_0020a.xq', 'Main'), ('seq/event/ev01_0020b.xq', 'Main'),
]


def cmd_plan():
    """fecho transitivo de funcoes alcancaveis a partir de ROOTS e histograma de opcodes (so o que o prologo executa)."""
    ALL = load_all()
    byhash = {}   # (arquivo, crc) -> (arquivo, idx)
    libidx = {}; anyidx = {}
    for p, x in ALL.items():
        for i, f in enumerate(x['funcs']):
            nm = x['s'](f[0]); h = zlib.crc32(nm.encode('shift-jis', 'replace')) & 0xffffffff
            byhash[(p, h)] = (p, i)
            if p in LIBS: libidx.setdefault(h, (p, i))
            anyidx.setdefault(h, (p, i))
    seen = set(); stack = []; unresolved = collections.Counter()
    for p, fn in ROOTS:
        x = ALL[p]
        for i, f in enumerate(x['funcs']):
            if x['s'](f[0]) == fn: stack.append((p, i))
    hist = collections.Counter(); sub = collections.Counter(); perfile = collections.defaultdict(set)
    while stack:
        p, i = stack.pop()
        if (p, i) in seen: continue
        seen.add((p, i)); x = ALL[p]; f = x['funcs'][i]
        perfile[p].add(x['s'](f[0]))
        for k in range(f[2], f[3]):
            a0, ac, ret, op, _ = x['insts'][k]
            hist[op] += 1
            if op == 2110 and ac and x['args'][a0][0] == 1: sub[x['args'][a0][1]] += 1
            if op in (20, 2211) and ac:
                t, v = x['args'][a0]
                h = v & 0xffffffff if t == 2 else (zlib.crc32(x['s'](v).encode('shift-jis', 'replace')) & 0xffffffff if t == 24 else None)
                if h is None: continue
                tgt = byhash.get((p, h)) or libidx.get(h) or anyidx.get(h)  # menus carregados juntos (NameMenu chama CharaMakeMenu_*)
                if tgt: stack.append(tgt)
                else: unresolved[(x['names'].get(h) or global_names().get(h) or '%08x' % h)] += 1
    out = ['# Plano minimo do interpretador (opcodes executados pelo caminho New Game -> controle no quarto)', '',
           'Funcoes alcancaveis: %d (de %d raizes). Instrucoes: %d. Opcodes distintos: %d. Subcodigos 2110 distintos: %d.' % (
               len(seen), len(ROOTS), sum(hist.values()), len(hist), len(sub)), '',
           '## Opcodes por frequencia no caminho', '', '| op | nome | n | semantica | st |', '|---|---|---|---|---|']
    for op, n in hist.most_common():
        d = OPDOC.get(op)
        out.append('| %d | %s | %d | %s | %s |' % (op, d[0] if d else '', n, (d[1] + ' ' + d[2]) if d else '(sem doc)', d[3] if d else ''))
    out += ['', '## Subcodigos de op2110 no caminho', '', '| sub | nome | n | semantica |', '|---|---|---|---|']
    for sc, n in sub.most_common():
        d = SUB2110.get(sc)
        out.append('| %d | %s | %d | %s |' % (sc, d[0] if d else '', n, (d[1] + ' ' + d[2]) if d else ''))
    out += ['', '## Chamadas nao resolvidas (funcao fornecida pelo motor, nao por script)', '']
    for nm, n in unresolved.most_common(): out.append('* `%s` x%d' % (nm, n))
    out += ['', '## Funcoes alcancadas por arquivo', '']
    for p in sorted(perfile): out.append('* `%s` (%d): %s' % (p, len(perfile[p]), ', '.join(sorted(perfile[p]))[:1500]))
    NOTES.mkdir(parents=True, exist_ok=True)
    (NOTES / 'xq32_newgame_plan.md').write_text('\n'.join(out), encoding='utf-8')
    print('ok plan:', len(seen), 'funcs,', len(hist), 'opcodes,', len(sub), 'sub2110,', len(unresolved), 'nao resolvidas')


def main():
    a = sys.argv[1:]
    if not a: print(__doc__); return
    if a[0] == 'catalog':
        cmd_catalog(a[2:] if len(a) > 1 and a[1] == '--glob' else [])
    elif a[0] == 'plan':
        cmd_plan()
    elif a[0] == 'dis':
        x = X.parse(read_fa(a[1])); print(dis(x, global_names()))
    elif a[0] == 'dumpall':
        (WORK / 'xq').mkdir(parents=True, exist_ok=True)
        g = global_names()
        for p, x in load_all().items():
            (WORK / 'xq' / (p.replace('/', '__')[:-3] + '.txt')).write_text(dis(x, g), encoding='utf-8')
        print('ok')

if __name__ == '__main__':
    main()
