# Formatos de arquivo (Level-5 / 3DS)

Código de referência: `tools/` · Referência externa usada: plugin Level-5 do **Kuriimu2** (GitHub `FanTranslatorsInternational/Kuriimu2`,
`plugins/Level5/plugin_level5`) para ARC0/XPCK/IMGC/compressão. O formato de malha (XMPR) **não** tem documentação pública conhecida:
foi decifrado lendo o `code.bin` e validado visualmente.

## Compressão Level-5 (`tools/l5.py`)
Cabeçalho de 4 bytes (u32 little-endian): `método = v & 7`, `tamanho descomprimido = v >> 3`. Métodos: 0 = sem compressão,
1 = LZ10 (flag byte MSB-first; ref de 2 bytes: `len=(b0>>4)+3`, `disp=((b0&0xF)<<8|b1)+1`), 2 = Huffman 4 bits, 3 = Huffman 8 bits
(árvore estilo Nintendo; bitstream em palavras u32 LE, MSB primeiro; **o byte de tamanho da árvore conta como índice 0**), 4 = RLE
(flag bit7: run de `(f&0x7f)+3`; senão literais `(f&0x7f)+1`). 5 = zlib (não visto). No jogo: função `FUN_00318bf0`.

## `yw1_a.fa` — ARC0 (`tools/fa_list.py`)
Cabeçalho de 72 bytes: `"ARC0"`, `dirEntriesOff, dirHashOff, fileEntriesOff, nameOff, dataOff` (u32), `dirCount` (i16), `dirHashCount` (i16),
`fileCount` (u32), ... Tabelas comprimidas (L5): diretórios (20 B: crc32, firstDirIdx u16, dirCount i16, firstFile u16, fileCount i16,
fileNameStart i32, dirNameStart i32), arquivos (16 B: crc32, nameOffInDir, fileOffset, fileSize; offset relativo a `dataOff`), tabela de
nomes (Shift-JIS, LZ10). Neste jogo: 1.471 diretórios, 20.835 arquivos, 386 MB. Lista completa: `data/romfs/yw1_a_files.csv`.
Tipos (contagem): `.bin` 10.525 · `.npcbin` 3.350 · `.xc` 3.151 · `.xq` 1.319 · `.xi` 1.233 · `.xv` 495 · `.xa` 391 · `.cfg` 231 · `.xs` 45 · `.xk` 37 · `.ralcam` 27 · `.xr` 14 · outros.
Maior parte do volume: `data/menu/*`, `data/res/map/*` (10.528 arquivos pequenos), `data/event/*`, `data/map/t10xg00` (mapas 3D).

## `XPCK` — contêiner (`.xc .xv .xa .xk .xr .xf`) (`tools/xpck.py`)
Header: `"XPCK"`, `u16 (count & 0xFFF | tipo<<12)`, `u16 infoOff, nameOff, dataOff` (todos em unidades de 4 bytes), `u16 infoSize, nameSize`,
`u32 dataSize`. Entradas de 12 B: `hash u32, nameOff u16, fileOffLow u16, fileSizeLow u16, fileOffHigh u8, fileSizeHigh u8`
(offset = ((high<<16)|low)<<2). Nomes: tabela comprimida (L5). Conteúdos vistos em `.xc` de personagem: `000.prm` (malha), `000.xi` (textura),
`NNN.mbn` (ossos), `NNN.cmn`, `000.mtr/.atr` (material), `000.sil`, `000.lkat`, `RES.bin` (comprimido L5; começa com `CHRC00`), e `.mtn2/.mtm2/.mtninf/.imminf` (animação). Um `.xc` pode conter outro `.xc`.

## `IMGC` — textura (`.xi`) (`tools/imgc.py`, `tools/etc1.py`)
Header: `"IMGC00\0\0"` (magic[3]='C' = CTR), `i16 entryOff, u8 fmt, u8 const, u8 mips, u8 bitDepth, i16 bytesPerTile, i16 w, i16 h,
u16 palInfoOff, palCount, imgInfoOff, imgCount, i32 dataOff, i32 platform`. Entrada de imagem (stride 24): `tileOff, tileSize, dataOff, dataSize`
(relativos a `dataOff`; cada blob é comprimido L5). A **tabela de tiles** é um array de `i16` (índice do tile de 8×8 no blob de dados; negativo = vazio)
em ordem de linha. Dentro de cada tile de 8×8 a ordem dos pixels é **Morton com Y no bit mais baixo** (o PICA nativo usa X); em ETC1/ETC1A4 os
4 blocos 4×4 do tile seguem a mesma ordem e **os pixels dentro do bloco estão transpostos** em relação ao ETC1 padrão. Imagem não vem invertida.
Formatos (campo fmt, jogo CTR2): 0 RGBA8, 1 RGBA4, 2 RGBA5551, 3 BGR888, 4 RGB565, 0x0B LA88, 0x0C LA44, 0x0D L8, 0x0E L4, 0x0F A8, 0x10 A4,
0x1B ETC1, 0x1C ETC1, 0x1D ETC1A4 (desambiguar por bitDepth). Neste jogo ocorrem: 0x00/32 bpp (587), 0x1C/8 bpp = ETC1A4 (377), 0x1B/4 bpp = ETC1 (211), 0x04 (29), 0x01 (28), 0x03 (1).

## `XMPR` — malha (`.prm`) (`tools/prm.py`)
Layout: header de 0x40 bytes = `"XMPR", u32 0x40 (offset do XPRM), u32 tamanhoXPRM, u32 offsetInfo(0x3C bytes: caixa/esfera de colisão em floats),
pares (offset,tamanho)` para 3 tabelas opcionais, tabela de hashes de ossos (`u32 × N`), string do nome do esqueleto (ex. `y001000_output.skin1`) e string
do material (ex. `DefaultLib.y001000_01`). **Uma `.prm` = uma malha = um material**.
* `XPRM` (20 B): `magic, u32 offVB(=0x14), u32 tamVB, u32 offVI, u32 tamVI`.
* `XPVB`: `magic, u16 offAttrs, u16 offFixed, u16 offVerts, u16 stride, u32 count`; 3 blobs L5:
  1. **atributos** (RLE): 10 entradas × 4 B `[tipo, offset, tamanho, flag]`; tipo 2 = float2, 3 = float3, 4 = float4; `flag&1` = atributo constante (valor vem do blob 2).
  2. **constantes** (LZ10): floats dos atributos fixos.
  3. **vértices** (LZ10): `count × stride` bytes (floats puros). Exemplo Whisper: stride 80 = pos(0,float3) | cor fixa(attr1) | normal(12,float3) | UV(24,float2) | float4×3 (32,48,64: pesos/índices de osso).
* `XPVI`: `magic, u16 fmt (2 = u16), u16 off, u32 count`; blob L5 com os índices. Primitiva: **triangle strip** com triângulos degenerados (índices repetidos) — igual ao que vai para o PICA (`PRIMITIVE_CONFIG`=strip).
* Os buffers são basicamente o que a GPU consome: no quadro capturado o personagem do draw 48 tem o mesmo layout (80 B/vértice).
Funções do jogo: `FUN_001b6740/001b68cc` (XMPR), `FUN_001a7970` (XPRM), `FUN_002edd64→FUN_002ebdec` (XPVB), `FUN_002edcbc→FUN_00304104` (XPVI), blobs `FUN_00303874`; magics `XPVB/XPVI` em `DAT_005c66c4/66c8`.

## Ainda não decifrados
`MTRC00` (`.mtr`, material; contém `LUTC00` = LUTs de iluminação), `.atr` (`ATRC01`), `.mbn` (osso, 168 B), `.cmn` (12 B), `.txp`, `.sil` (`XCSL`), `.lkat`,
`.mtn2` (`XMTN`), `.mtm2` (`XMTM`), `XQ32` (scripts), `XSCR`, `XFSP`, `CAME` (`.cfg` de câmera), `.npcbin`, `.ralcam`, `.mapenv` (texto Shift-JIS), `.moflex`, `.bcsar` (`romfs/snd/product/`).
