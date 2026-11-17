"""Gera LOCALMENTE (work/mods_teste/, nunca versionar) mods de dados de teste a partir do yw1_a.fa do usuário.

    python mod_make_test.py                      # -> work/mods_teste/teste_dados/ (mod.toml + romfs/…)
    YW_MODS_DIR=…/work/mods_teste  yw1r.exe      # o jogo usa esta pasta de mods em vez de %APPDATA%/yw1/mods

Dois arquivos abertos por FUN_0047359c na rota "Continue" (route_probe4) e visíveis nela:
- data/res/text/menu/saveloadmenu_text_en.cfg.bin (tela "CHOOSE ENTRY"): troca "No data." e
  "Continue your game." por textos do MESMO tamanho em bytes (a tabela de strings e os deslocamentos ficam iguais; formato em tools/cfgbin.py);
- data/map/t101g00/t101g01.xi (IMGC ETC1, 1024×1024: o minimapa de Uptown Springdale na tela de baixo, aberto ao
  chegar no campo): troca vermelho ↔ azul em todos os blocos ETC1 (bytes 7 e 5 do bloco de 64 bits: os campos R e
  B têm o mesmo formato nos modos individual e diferencial). Os dados de pixel são recomprimidos em LZ10 Level-5 só
  com literais (método 1, o mesmo do original; o decodificador do jogo, FUN_00318bf0, trata o bit de bandeira 0 como
  literal) e o tamanho no índice de imagens é atualizado.
  (data/menu/face_icon/face_icon.xi também passa por FUN_0047359c nessa tela, mas não aparece nela: são rostos de
  Yo-kai.)
"""
import struct
import sys

import imgc
import l5
import yw_fa
from paths import WORK

TEXTO = "data/res/text/menu/saveloadmenu_text_en.cfg.bin"
MAPA = "data/map/t101g00/t101g01.xi"
TROCAS = [(b"No data.", b"MOD OK!!"), (b"Continue your game.", b"Texto do mod ativo.")]


def lz10_literais(dados: bytes) -> bytes:
    """LZ10 Level-5 (cabeçalho u32 = tamanho << 3 | 1) só com literais: a cada 8 bytes, uma bandeira 0."""
    out = bytearray(struct.pack("<I", len(dados) << 3 | 1))
    for i in range(0, len(dados), 8):
        out.append(0)
        out += dados[i:i + 8]
    while len(out) % 4:
        out.append(0)
    return bytes(out)


def texto_trocado() -> bytes:
    d = bytearray(yw_fa.get(TEXTO))
    _, so, sl, _ = struct.unpack_from("<4I", d, 0)
    for velho, novo in TROCAS:
        assert len(velho) == len(novo)
        i = d.find(velho + b"\0", so, so + sl)
        assert i >= 0, velho
        d[i:i + len(novo)] = novo
    return bytes(d)


def mapa_trocado() -> bytes:
    d = yw_fa.get(MAPA)
    H = imgc.parse(d)
    assert (H["fmt"], H["bd"], H["cnt"]) == (0x1B, 4, 1), H
    _, _, _, _, _, _, _, _, _, _, _, iio, iic, dofs, _ = struct.unpack_from("<8shBBBBhhhHHHHii", d, 0)
    assert iic == 1
    t0, ts, d0, ds = struct.unpack_from("<4i", d, iio)
    px = bytearray(l5.l5dec(d[dofs + d0:dofs + d0 + ds]))
    assert len(px) % 8 == 0
    for i in range(0, len(px), 8):  # bloco ETC1 = u64 little endian: byte 7 = R, 6 = G, 5 = B
        px[i + 5], px[i + 7] = px[i + 7], px[i + 5]
    novo = lz10_literais(bytes(px))
    assert l5.l5dec(novo) == bytes(px)
    assert d[dofs + d0 + ds:].strip(b"\0") == b"", "dados de pixel não são o fim do arquivo"
    out = bytearray(d[:dofs + d0]) + novo
    struct.pack_into("<4i", out, iio, t0, ts, d0, len(novo))
    # confere decodificando como o resto das ferramentas: R novo = B antigo
    a, _ = imgc.decode(d)
    b, _ = imgc.decode(bytes(out))
    assert (a[:, :, 0] == b[:, :, 2]).all() and (a[:, :, 1] == b[:, :, 1]).all()
    return bytes(out)


def main():
    raiz = WORK / "mods_teste" / "teste_dados"
    (raiz / "romfs").mkdir(parents=True, exist_ok=True)
    (raiz / "mod.toml").write_text(
        'nome = "Teste de dados (local)"\nversao = "1"\nautor = "mod_make_test.py"\nprioridade = 0\n'
        'descricao = "Texto da escolha do diário trocado e minimapa de Uptown com vermelho e azul trocados"\n',
        encoding="utf8")
    for caminho, dados in ((TEXTO, texto_trocado()), (MAPA, mapa_trocado())):
        p = raiz / "romfs" / caminho
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(dados)
        print(f"{caminho}: {len(dados)} bytes -> {p}")


if __name__ == "__main__":
    sys.exit(main())
