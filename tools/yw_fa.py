"""Acesso direto a arquivos do yw1_a.fa pelo caminho (usa data/romfs/yw1_a_files.csv). Uso: from yw_fa import get, ls"""
import csv, os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from paths import *
_idx = None
def index():
    global _idx
    if _idx is None:
        _idx = {}
        with open(ROOT/'data'/'romfs'/'yw1_a_files.csv', encoding='utf-8') as f:
            r = csv.reader(f); next(r)
            for p, o, s in r: _idx[p] = (int(o), int(s))
    return _idx
def get(path):
    o, s = index()[path]
    with open(FA, 'rb') as f: f.seek(o); return f.read(s)
def ls(prefix):
    return sorted(p for p in index() if p.startswith(prefix))
