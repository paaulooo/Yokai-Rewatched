"""Roda o jogo recompilado COM a janela por um tempo fixo, com roteiro de entrada, para medir o que só existe na janela
(cadência da interpolação, redesenhos, apresentação). Nunca captura a área de trabalho: as medidas vêm do log
(linhas `[janela]`, a cada 5 s).

    python window_run.py NOME --secs 150 [--input "20:start; +8:a"] [--set graficos.interpolacao=120 ...]

As configurações do usuário (%APPDATA%/yw1/settings.toml) não são tocadas: o teste usa uma CÓPIA em
work/runs/NOME/settings.toml, com os `--set seção.chave=valor` aplicados (`YW_SETTINGS_TOML`). Sem som (YW_AUDIO=none).
A janela fecha sozinha depois de --secs (`YW_WINDOW_SECS`). Log em work/runs/NOME.log; logs da execução em
work/logs/NOME/.
"""
import argparse
import os
import re
import subprocess

from paths import WORK

EXE = WORK / "recomp" / "target" / "release" / "yw1r.exe"


def settings_copy(dst, sets):
    """Copia o settings.toml do usuário aplicando `seção.chave=valor` (valor em TOML: 120, true, "desligado")."""
    src = os.path.join(os.environ.get("APPDATA", "."), "yw1", "settings.toml")
    text = open(src, encoding="utf8").read() if os.path.exists(src) else ""
    for s in sets:
        key, val = s.split("=", 1)
        sec, k = key.split(".", 1)
        if not re.fullmatch(r"-?\d+(\.\d+)?|true|false|\".*\"", val):
            val = f'"{val}"'
        m = re.search(rf"^\[{re.escape(sec)}\]\s*$", text, re.M)
        if not m:
            text += f"\n[{sec}]\n{k} = {val}\n"
            continue
        end = re.search(r"^\[", text[m.end():], re.M)
        body_end = m.end() + (end.start() if end else len(text) - m.end())
        body = text[m.end():body_end]
        if re.search(rf"^{re.escape(k)}\s*=", body, re.M):
            body = re.sub(rf"^{re.escape(k)}\s*=.*$", f"{k} = {val}", body, flags=re.M)
        else:
            body = f"\n{k} = {val}" + body
        text = text[:m.end()] + body + text[body_end:]
    dst.write_text(text, encoding="utf8")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("name")
    ap.add_argument("--secs", type=float, default=60)
    ap.add_argument("--input", default="")
    ap.add_argument("--set", action="append", default=[], help="seção.chave=valor no settings.toml da cópia")
    ap.add_argument("--env", action="append", default=[], help="VAR=valor extra no ambiente")
    a = ap.parse_args()
    out = WORK / "runs" / a.name
    out.mkdir(parents=True, exist_ok=True)
    st = out / "settings.toml"
    settings_copy(st, a.set)
    env = dict(os.environ)
    env.update(YW_AUDIO="none", YW_DSP_STATS="0", YW_WINDOW_SECS=str(a.secs), YW_SETTINGS_TOML=str(st))
    for e in a.env:
        k, v = e.split("=", 1)
        env[k] = v
    logdir = WORK / "logs" / a.name
    logdir.mkdir(parents=True, exist_ok=True)
    for f in logdir.glob("*"):
        f.unlink()
    env["YW_LOG_DIR"] = str(logdir)
    if a.input:
        env["YW_INPUT"] = a.input
    log = WORK / "runs" / f"{a.name}.log"
    with open(log, "w", encoding="utf8", errors="replace") as f:
        r = subprocess.run([str(EXE)], cwd=EXE.parent.parent.parent, env=env, stdout=f, stderr=subprocess.STDOUT,
                           timeout=a.secs + 90)
    lines = log.read_text(encoding="utf8", errors="replace").splitlines()
    print(f"saída {r.returncode} · log {log}")
    print("\n".join(l for l in lines if l.startswith("[janela]")))


if __name__ == "__main__":
    main()
