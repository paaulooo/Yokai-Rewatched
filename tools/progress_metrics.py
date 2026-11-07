"""Mede o progresso do port: quanto do jogo já foi extraído e quanto do código já existe em Rust.

Saída: work/notes/progress_metrics.json (+ resumo no terminal). Rodar de qualquer pasta:
  python progress_metrics.py

Métricas (todas contadas, nenhuma chutada):
  1. opcodes de script: dos opcodes distintos usados pelos .xq (work/notes/xq32_opcodes.json), quantos têm um braço
     `NNNN =>` nos hosts Rust (xq.rs, menu_host.rs, event_host.rs, scene.rs, title.rs) — por opcode e ponderado pelo nº
     de chamadas nos scripts.
  2. subcódigos de event_cmd (op2110): idem, a partir das chamadas `event_cmd#2110(NNNNN, ...)` em work/xq/*.txt.
  3. code.bin: funções no export do Ghidra (data/ghidra/code.bin.c) e quantas foram lidas/reimplementadas
     (endereços FUN_xxxxxxxx citados no código Rust e nos docs).
  4. formatos do RomFS: lido de work/notes/format_coverage.json (gerado pelo teste Rust `format_coverage`).
  5. eventos e mapas: work/notes/event_coverage.json e map_coverage.json (testes Rust `event_coverage` e `map_coverage`,
     rodar com `cargo test --release <nome> -- --ignored`). "Evento termina" = o script chega ao fim na VM com o host de
     eventos; NÃO garante que efeitos, flags e visual estejam certos (51 % das chamadas de event_cmd ainda são ignoradas).
"""
import json, os, re, sys, collections
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from paths import ROOT, WORK

ENGINE = ROOT / 'engine' / 'src'
NOTES = WORK / 'notes'


def rust_sources():
    for dp, _, fs in os.walk(ENGINE):
        for f in fs:
            if f.endswith('.rs'):
                p = os.path.join(dp, f)
                yield p, open(p, encoding='utf8').read()


def implemented_ops():
    """Números em braços de match (`1234 =>`, `1234 | 5678 =>`, `1234 if ...`) dos hosts."""
    ops = set()
    arm = re.compile(r'^\s*((?:\d{2,5}\s*\|\s*)*\d{2,5})\s*(?:if\b[^=]*)?=>', re.M)
    for p, s in rust_sources():
        if not any(k in p for k in ('xq.rs', 'menu_host.rs', 'event_host.rs', 'scene.rs', 'title.rs')):
            continue
        for m in arm.finditer(s):
            for n in re.findall(r'\d+', m.group(1)):
                ops.add(int(n))
    return ops


def main():
    cat = json.load(open(NOTES / 'xq32_opcodes.json', encoding='utf8'))
    ops = implemented_ops()
    # opcodes estruturais da VM (mov/aritmética/salto/chamada) são executados no laço do interpretador
    builtin = {int(k) for k, v in cat.items() if v.get('status') == 'C' and int(k) < 600}
    used = {int(k): v['count'] for k, v in cat.items()}
    done = {k for k in used if k in ops or k in builtin}
    calls_total = sum(used.values())
    calls_done = sum(c for k, c in used.items() if k in done)

    # subcódigos do op2110
    sub = collections.Counter()
    for f in (WORK / 'xq').glob('*.txt'):
        for m in re.finditer(r'event_cmd#2110\((\d+)', f.read_text(encoding='utf8', errors='replace')):
            sub[int(m.group(1))] += 1
    sub_done = {k for k in sub if k in ops}

    # code.bin
    src = (ROOT / 'data' / 'ghidra' / 'code.bin.c').read_text(encoding='utf8', errors='replace')
    funcs = set(re.findall(r'^\S[^\n(]*?\b(FUN_[0-9a-f]{8})\(', src, re.M))
    cited = set()
    for _, s in rust_sources():
        cited |= set(re.findall(r'FUN_[0-9a-f]{8}', s))
    for d in (ROOT / 'docs').glob('*.md'):
        cited |= set(re.findall(r'FUN_[0-9a-f]{8}', d.read_text(encoding='utf8')))
    cited &= funcs

    rust_lines = sum(s.count('\n') for _, s in rust_sources())
    fmt = json.load(open(NOTES / 'format_coverage.json', encoding='utf8')) if (NOTES / 'format_coverage.json').exists() else None

    leaf = None
    if fmt:
        cont = {'xc', 'xr', 'xv'}
        rows = [r for r in fmt['by_ext'] if r['ext'] not in cont] + fmt['inner']
        leaf = dict(files=sum(r['files'] for r in rows), ok_files=sum(r['ok_files'] for r in rows),
                    bytes=sum(r['bytes'] for r in rows), ok_bytes=sum(r['ok_bytes'] for r in rows))
    def load(name):
        f = NOTES / name
        return json.load(open(f, encoding='utf8')) if f.exists() else None
    out = dict(
        events=load('event_coverage.json'), maps=load('map_coverage.json'),
        leaf=leaf,
        opcodes=dict(total=len(used), done=len(done), calls_total=calls_total, calls_done=calls_done),
        event_cmd=dict(total=len(sub), done=len(sub_done), calls_total=sum(sub.values()),
                       calls_done=sum(c for k, c in sub.items() if k in sub_done)),
        code_bin=dict(functions=len(funcs), read=len(cited)),
        rust_lines=rust_lines,
        formats=fmt,
    )
    json.dump(out, open(NOTES / 'progress_metrics.json', 'w', encoding='utf8'), indent=1, ensure_ascii=False)
    o, e, c = out['opcodes'], out['event_cmd'], out['code_bin']
    print(f"opcodes: {o['done']}/{o['total']} ({100*o['done']/o['total']:.1f}%) · chamadas {100*o['calls_done']/o['calls_total']:.1f}%")
    print(f"event_cmd: {e['done']}/{e['total']} ({100*e['done']/max(1,e['total']):.1f}%) · chamadas {100*e['calls_done']/max(1,e['calls_total']):.1f}%")
    print(f"code.bin: {c['read']}/{c['functions']} funções lidas ({100*c['read']/c['functions']:.2f}%)")
    print(f"Rust: {rust_lines} linhas")
    if out['events']:
        ev = out['events']
        print(f"eventos: {ev['finishes']}/{ev['total']} terminam na VM ({100*ev['finishes']/ev['total']:.1f}%)")
    if out['maps']:
        mp = out['maps']
        print(f"mapas: {mp['load']}/{mp['maps']} carregam ({100*mp['load']/mp['maps']:.1f}%), {mp['triangles']} triângulos")
    if leaf:
        print(f"extração (folhas): {leaf['ok_files']}/{leaf['files']} arquivos ({100*leaf['ok_files']/leaf['files']:.1f}%), "
              f"{100*leaf['ok_bytes']/leaf['bytes']:.1f}% dos bytes")


if __name__ == '__main__':
    main()
