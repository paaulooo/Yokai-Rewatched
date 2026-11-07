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

## Layouts de menu 2D (`data/menu/*.xa` = XPCK) (`engine/src/layout.rs`, `engine/src/assets/anmc.rs`)
Ex.: `title_u00_en.xa` (tela de cima do título) e `title_d00_en.xa` (de baixo). Conteúdo: `NNN.xi` (texturas, na ordem
da lista de texturas do ANMC), `000.pvb` (XPVB: pos float3 em **pixels** com origem no centro da peça, cor constante
(1,1,1,1), UV float2 em **texels**), `NNN.pbi` (XPVI, uma por malha; aqui o campo de formato é 0 mas os índices são
**u16**, em **lista de triângulos**, globais no buffer) e `RES.bin` (L5 → `ANMC00`).
* `ANMC00`: palavras 5..0x18 = 20 × u32 `(quantidade << 16 | offset em palavras)`, alternando seção e
  `(tipo, bytes por entrada)`. Seções: 0 texturas (20 B: hash do nome), 2 materiais (224 B: +0 hash, +16 hash da textura,
  +24/+28 = 1/largura, 1/altura), 8 malhas (60 B: +0 hash `#nó k`, +12 hash do material, caixa), 12 nós (32 B: +0 hash,
  +4 `nº de malhas << 16 | offset do nome`, caixa), 14 keyframes (20 B: hash do nó, duração em quadros de 60 Hz,
  deslocamento xyz), 16 instâncias (48 B: hash, `nº de keyframes << 16 | nome`, ?, posição xyz, caixa), 18 strings.
* Hashes = **CRC-32** dos nomes (`bg00`=046c3b6e, `main_bg`=18832ffb). Malha `k` do nó `n` = `crc32("#n k")`.
* Cada instância é um **flipbook**: keyframes consumidos em ordem (no título de cima 64+21+7+1+6 = 99 = total da seção).
  Tela (origem no canto superior esquerdo) = posição da instância + deslocamento do keyframe + coordenada local.
  Conferido: botão 1 de baixo = (78,94) + (82,0) + local [-83..83]×[0..55] → centrado em x=160.
* Título de cima: `bg00` (cidade com Yo-kai espiando, 64 keyframes em ciclo), `title_00` (logo com brilho), `youkai_wa`
  (moldura de Yo-kai: `waa_00..05`, de 2000×1200 a 404×244 em ~12 quadros), `title_trial` (versão demo), `version`.
  z por vértice: cidade 24, logo/sobreposições 0, moldura −6 (paralaxe do 3D).
* Ainda não entendidos: campos `f0` da instância, seção "CHP" (`#CHP`), o resto dos 224 B do material, flag de loop.

## `XQ32` — scripts (`seq/**/*.xq`, 1.319 arquivos) (`tools/xq32_disasm.py`)
* **Semântica executável** (interpretador `engine/src/xq.rs`, validada rodando a cadeia do New Game): 10 yield · 11 return · 20 call ·
  30 salta-se-verdadeiro · 31 salta · 33 salta-se-falso · 100 mov · 120 not · **121 E · 122 OU** · 130 = · 131 ≠ · 132 ≥ · 133 ≤ · 134 > · 135 < ·
  150-154 + − × ÷ % · 160/161 & | · 240/241 ++/−− do destino · 501 sprintf · 2211 cria corrotina. Variáveis: `$0..999` contexto da corrotina,
  `$1000` retorno, `$1001+` temporários, `$2000+` locais, `$3000+` argumentos, `$4000+` globais do arquivo. Funções de biblioteca vêm de
  `seq/prog_common_001e.xq` e `seq/prog_menu_001e.xq`.
Cabeçalho: `"XQ32"`, u16 nº de funções, pares u16 (offset em palavras, quantidade) de funções, saltos, instruções e
argumentos, u16 nº de globais, u16 offset das strings; cada tabela é um bloco L5. Tamanhos conferidos em
`TitleMenu.xq` (27×24, 47×12, 292×12, 572×8 bytes, exatos):
* função (24 B): u32 nome, u32 crc32(nome) (bate nas 27), u16 instr. início/fim, u16 1º salto, u16 nº de saltos, 2 × u16.
* salto (12 B): u32 nome (`@000@`...), u32 crc32, u32 índice da instrução.
* instrução (12 B): u16 1º argumento, u16 nº de argumentos, u16 variável de destino, u16 opcode, u32 0.
* argumento (8 B): u32 tipo (1 int, 2 hash CRC-32, 3 float, 4 variável `$n`, 24 string), u32 valor.
* Opcodes pelo uso: `20` chama função do script pelo hash; `31` salta; `33` salta se falso; `30` salta se verdadeiro;
  `10` espera 1 quadro; `100` atribui; `130` ==; `134` >; `240` incrementa; `500` parâmetros; `2211` dispara função
  em paralelo; `2104` troca de cena. Os de 1000+ são nativos do motor (5002 cria objeto, 5622 toca animação com flags,
  5102/5103 alfa/fade de alfa, 14800/14806/14808 música...). Hashes sem string no arquivo = funções externas.
`TitleMenu.xq` em resumo: prepara `M01_MAIN_THEME` → toca `yw1_op` → monta as telas → fade 30 → `Menu_TitleStart`
(logo alfa 0→1 em 120 quadros, música) → espera; 1800 quadros ociosos repetem a abertura; botões dependem do save;
New Game/Continue → moldura ao contrário, fades, cena 2111. A abertura também tem letra na tela de baixo
(`movietext` ← `data/txt/op00a_*.cfg.bin`, ainda não implementado).

## `.moflex` — vídeo Mobiclip (`romfs/mov/`, fora do `.fa`) (`engine/src/video.rs`)
Contêiner moflex com vídeo Mobiclip 400×240 (tela de cima), **sem trilha de áudio**. Medido com ffprobe/ffmpeg 8.1:
`logo_l5.moflex` = 260 quadros a 60 fps (4,33 s); `yw1_op.moflex` = 4630 quadros a 48 fps. A abertura é **estéreo
intercalada**: quadros alternam olho esquerdo/direito (deslocamento horizontal de ±19 px trocando de sinal a cada
quadro; quadro n ≈ quadro n+2) → 2315 quadros por olho a 24 fps = 96,46 s. Os outros: `ec01..ec11_{atari,hazure,rare}`
(cápsulas do Crank-a-kai), `yw1_ed_a/b` (encerramento), `yw1_sr_a/b`. No `code.bin`: `LogoMenu`, `lxpCMovieManager`,
`lxpCObjMovie` e a string `.moflex`. Decodificação hoje via ffmpeg (provisório); falta um decodificador Mobiclip em Rust.

## `.bcstm` — stream de áudio (`romfs/snd/product/stream/`) (`engine/src/audio/bcstm.rs`)
`CSTM` little-endian; blocos INFO (0x4000), SEEK (0x4001), DATA (0x4002) pela tabela em 0x14 (12 B/entrada). INFO+8
tem 3 referências (u16 tipo, u16 pad, u32 offset relativo a INFO+8): stream info (0x4100), track info, tabela de canais
(0x0101). Stream info: `u8 codificação (2 = DSP-ADPCM), u8 loop, u8 canais`, +0x04 taxa, +0x0c nº de amostras,
+0x10 nº de blocos, +0x14 bytes/bloco/canal, +0x18 amostras/bloco, +0x24 tamanho com padding do último bloco,
+0x34 offset dos dados (relativo a DATA+8). Cada canal → info ADPCM: 16 coeficientes i16, `ps`, `hist1`, `hist2`.
Dados = blocos intercalados por canal; quadro DSP de 8 B (cabeçalho `par<<4 | escala` + 14 nibbles).
A música da abertura é `yw1_op_en.dspadpcm.bcstm`: 2 canais, 32728 Hz, 3.157.162 amostras = **96,47 s** (bate com o
vídeo). Validação: com o arredondamento do ffmpeg a saída é idêntica amostra a amostra (atravessando blocos); o runtime
usa a fórmula de referência da Nintendo, que difere do ffmpeg em no máx. 17/32768 (teste `cargo test`).

## Ainda não decifrados
`MTRC00` (`.mtr`, material; contém `LUTC00` = LUTs de iluminação), `.atr` (`ATRC01`), `.mbn` (osso, 168 B), `.cmn` (12 B), `.txp`, `.sil` (`XCSL`), `.lkat`,
`.mtn2` (`XMTN`), `.mtm2` (`XMTM`), `XQ32` (scripts), `XSCR`, `XFSP`, `CAME` (`.cfg` de câmera), `.npcbin`, `.ralcam`, `.mapenv` (texto Shift-JIS), `.bcsar` (`romfs/snd/product/`).

## Mapas 3D (`data/map/<id>/<id>.xc`) — `engine/src/room.rs`, `tools/map_probe.py`
`.xc` (XPCK) com `RES.bin` (`CHRC00`, L5): manifesto cujas tabelas (hash = CRC-32 do nome) dão a ordem dos arquivos `NNN.prm` (malhas),
`NNN.mbn` (nós, 168 B: hash, hash do pai, translação f32 nas palavras 3..5), `NNN.xi` (texturas); registros de material de 224 B (+16 = hash da
textura). Malha = XMPR (stride 36: pos f32×3 · cor de vértice RGBA f32 · UV f32×2 com v=0 embaixo; índices = triangle strip). Nome `<id>cl.*` =
colisão (material `col-`) e decalques de sombra; `jmden23-*`/`jn1-*` = mesma geometria duas vezes (HIPÓTESE: alta/baixa). `<id>_mapenv.bin` e
`<id>_funcpt.bin` são **cfg.bin** (T2B: `u32 crc32(nome) | u8 n | tipos 2 bits | n×u32`, nomes por CRC numa tabela final; `engine/src/assets/cfgbin.rs`)
com árvore `PTREE/PTVAL`: `CAMERA{Fov,Dist,RotH,RotV,Near,Far}`, `LIGHT`, `WEATHER`... Alfa do vértice multiplica o alfa da textura (sombras suaves).
`.mtr/.atr` (MTRC00/ATRC01, bit-packed) ainda não decifrados.

## Fonte (`fnt/ft_nrm.xf`, `fnt/ft_sml.xf`) — `engine/src/assets/font.rs`
XPCK com `000.xi` (folha 1024×504) + `FNT.bin` (`FNTC01`, sem compressão por fora). Cabeçalho: 0x08 u16 nº de texturas · 0x0c u16 altura
da linha (18) · 0x10 u16 ? (929) · 0x1c/0x1e, 0x20/0x22, 0x24/0x26 = (offset em palavras, nº) das tabelas de tamanhos (4 B), caracteres
"grandes" (8 B) e "pequenos" (8 B), cada uma um bloco L5 — os tamanhos de entrada conferem com o leitor do jogo (`code.bin.c` ~413187,
que aloca `nº × 4/8/8`). Tamanho: i8 dx, i8 dy, u8 w, u8 h. Caractere: u16 código UCS-2 · u16 (bits 0-9 índice de tamanho, 10-15 avanço) ·
u32 (bits 0-3 canal, 4-17 x, 18-31 y) — única divisão que cabe na textura nos 8.000 caracteres. Canais 0/1/2 = R/G/B: três folhas de
glyphs sobrepostas. Validado desenhando "A", "g" e katakana.
Textos de menu: `op13733(tabela, #chave)` → `data/res/text/menu/<tabela>_en.cfg.bin`, entradas `TEXT_INFO [crc32(chave), índice, texto, flag]`
(texto UTF-8 com `
` literal e marcações `<CR>`, `</C>`, `<SAVE_SLOT>`).

## Personagens e animação — `engine/src/character.rs`, `tools/mtn_probe.py`, `tools/skel_probe.py`
* `.mbn` (168 B): hash, hash do pai, flags, translação local 0x0c, rotação 3×3 (convenção linha, v' = v·A) 0x18, escala 0x3c; mundo
  composto bate com a matriz gravada (1e-7). XMPR de personagem: slot 0 posição (modelo, bind), 2 normal, 4 UV (V direto), 7 pesos,
  8 índices → tabela de hashes de osso no header 0x28 (offset, tamanho).
* XMTN (`.mtn2`; referência Ploaj/Metanoia `Level5_MTN2.cs`, corrigida): 0x10 offset dos dados L5, 0x14/0x18/0x1c nº de faixas de
  posição/rotação/escala, 0x54 nº de quadros. Dados: hashes de osso, 3 descritores (tipo, tipo de dado 1 = i16/32767, 2 = f32, nº de
  componentes), tabela de 16 B por faixa (offsets de flags/quadros/valores). Flags: i16 osso + **u16 (12 bits nº de chaves | 4 bits flag)**.
  Quadros i16 absolutos na linha do tempo do arquivo; rotação = quatérnion (x,y,z,w).
* MINF (`.mtninf`, 96 B): 0x1c hash, 0x20 nome Shift-JIS, 0x44 hash do movimento, 0x4c início, 0x50 fim, 0x54 velocidade (0.5).
* Mapas: `_funcpt.bin` `FP`/`FP_DEF` com `MAP_JUMP{JUMP_TO{MapID} STD_POS{POS}}` — STD_POS é a chegada no mapa destino.

## Cutscene `ev01_0005` (1ª cutscene 3D) — levantamento, ainda NÃO implementada
* Script `seq/event/ev01_0005a.xq`: objeto âncora `point_a` (`data/event/ev_point/point_a.xc`, 28 nós `evp_aNN`, próprio movimento
  `c0010_point_a`); personagens `c526000, c903000, c102000 (Eddie), c103000 (Bear), c534000` e Yo-kai `d003000/d003001, d001000,
  d012000` (`data/character/<id>/<id>_p00.xc`) presos a nós (`op2110(11046, obj, 'point_a', 'evp_aNN')`), cada um com movimento
  `data/event/ev01/ev01_0005/ev01_0005_c0010_<obj>.xc` (XMTN + 1 MINF); protagonista = `ei000040` (partes de c001 e c002 juntas);
  cenário = efeito `ev010005001000.xc` (sol, raios, arco-íris: planos de ±892, com `.mtm2/.imm2` de animação de material/imagem);
  duração `WaitFrame 473` + fade 30. Música: stream `ev01_0005`.
* Câmera `ev01_0005_c0010_camera.xv` (XPCK: `CMR.bin` `XCMT` + `000.cmr2` `XCMA`): cabeçalho 0x20 hash do nome, 0x28 nº de quadros
  (503), 0x30 f32 0.5; 4 descritores em 0x60 `(tipo 1, dado 2 = f32, ?, componentes, 0, 503)` com 3,3,1,1 componentes
  (HIPÓTESE: posição, alvo, rolagem, FOV); bloco L5 em 0x98 (6.180 B) começando com flags `i16 -1` + `u16 0x21b9` (mesmo esquema
  12 bits de contagem do XMTN). Layout dos valores ainda não fecha — continuar daqui.
* Rua `t101g00` (cenário de `ev01_0010` e destino seguinte): grade de blocos `t101gNN.xc` (cada um com prm/mbn/mtr próprios, manifesto
  `RES.bin` com tabelas em ordem diferente da dos interiores) + texturas compartilhadas em `t1010(_en).xr` (XPCK com `RES.bin` que
  não é CHRC00). Não carregada ainda.

## Relógio do jogo e data do diário (lidos no `code.bin`)
- Contador `u32` em `[DAT_005c69f8+0x10]+0xc`, em terços de segundo desde 00:00 (`FUN_002b3810`):
  `op14041(0)` hora = t/10800 · `(1)` minuto = (t/180)%60 · `(2)` segundo = (t/3)%60 ·
  `(3)` período: 0 se 06:00–17:59, senão 3 (noite) · `(≥4)` contador cru.
- `op14040(h, m)` grava `h·10800 + m·180` (`FUN_002aec6c`). `FUN_00371b44` avança `+dt·3` por quadro e zera após
  23h, exceto com o flag `time_lock` (crc32 `0x2a329223`, `op14000('time_lock', v)`).
- `op12055(slot, i)` (`FUN_003fb1cc`): data em que o diário foi gravado — `u16` ano, `u8` mês, dia, hora, minuto.
- **Port** (`engine/src/clock.rs`): a hora vem do relógio local do PC (`GetLocalTime`); hora forçada pela história
  (`op14040`) vale até `time_lock` ser solto ou até novo jogo/carregar; a data do diário é a do PC ao salvar.
  `YW_CLOCK=HH:MM` fixa a hora de partida. [a regra da hora forçada é decisão do port]

## Yo-kai, batalha e encontros (validado contra os dados; layout dos registros conferido com o Albatross de Tiniifan)
- `data/res/character/chara_param_0.02.cfg.bin` `CHARA_PARAM_INFO`: 0 param · 1 base · 2 tribo · 3..7 HP/FOR/ESP/DEF/VEL ·
  9/11/13/15 golpes ataque/técnica/inspirar/guarda com 10/12/14/16 = chance % · 18..23 multiplicador por elemento ·
  24 Soultimate · 28 habilidade · 29 dinheiro · 30 experiência.
- `chara_base_0.04j.cfg.bin` (`CHARA_BASE_INFO` / `CHARA_BASE_YOKAI_INFO`): 0 base · 1 prefixo (0 c, 5 x, 6 y, 7 z) ·
  2 número · 3 variante (→ modelo `y311000`) · 4 nome (`chara_text_en` `NOUN_INFO` campo 5) · **11 rank** (0 E … 5 S;
  Jibanyan D, Shmoopie C, Shogunyan S — bate com o jogo).
- `battle_command_0.02f.cfg.bin` `BATTLE_COMMAND_INFO`: 0 hash · 1 nome (`skill_text`/`battle_text`) · 2 categoria
  (3 golpe, 2 guarda, 5 Soultimate) · 3 tipo (0 ataque, 1 técnica, 2 inspirar, 5 guarda). Ex.: Cadin = Stepping Slice /
  Whirlwind / Cicada Ninjutsu / Soultimate Cicada Cut.
- `common_enc_0.02m.cfg.bin` e `data/res/map/<m>/<m>_enc_*.cfg.bin`: `ENCOUNT_TABLE(crc32(nome), i1..i4, …, música?)` →
  `ENCOUNT_CHARA(param, nível, …)`. `ev01_btl0010` = um Buhu (variante fraca) nível 2.
- `<m>_watchmap_0.02j.cfg.bin` `WM_YOKAI_PRESET_*`: CONFIG(período 0 dia / 3 noite = `op14041(3)`, estágio, 1º bloco, nº) ·
  BLOCK(ponto 1..8, 1º dado, nº) · DATA(tabela de encontro, peso, …, condição base64 opcional). O jogo só deixa achar
  Yo-kai de rank ≤ rank do relógio.
- Clipes de batalha (todos os Yo-kai): `戦1立ち1L`, `戦1こうげき1`, `戦1ようじゅつ1`, `戦1ガード1L`, `戦1ダメージ1`, `戦1死1`,
  `戦1勝利1`, `戦1ため1L`, `戦1ひっさつ1`. Malha `Scene_Material` (24 vértices) = caixa de volume, não se desenha.
- Interface: `data/menu/battle_u00_en.xa` (barras de HP, gauge, mensagens) e `battle_d00_en.xa` (anel, botões, purificar).
- A lógica da batalha é nativa (os .xq de batalha só fazem menus/efeitos); fórmulas ainda não lidas do code.bin [H].

## Colisão dos mapas (corrigido 2026-11-06)
- XPVI +4 = formato do índice: **2 = triangle strip** (todas as 114 malhas visuais conferidas), **0 = lista de triângulos**
  (as 5 malhas de colisão `cl`, todas com trios sem repetição). Ler a colisão como strip gerava triângulos falsos: paredes
  diagonais que prendiam o jogador no pé da escada e "chão" inexistente.
- Grupo `<mapa>cl.`: material `col-` = só faces verticais (paredes); materiais `sndNN-sdwNN-` (som de passo + sombra) = o chão
  andável, inclusive as rampas da escada. Térreo `t101i01`: chão 0, escada rampa 0→16, hall de entrada −2 (porta em z 75).
- O sentido dos triângulos de colisão vem invertido (normal para baixo): o piso vale por |ny| > 0,3.

## Flags, som, materiais e câmera: o que foi medido (2026-11-06)
- **Flags**: `data/res/sys/flag_config_0.01.cfg.bin` → `FLAG_INFO(bit, hash do nome)`: 2.328 registros, 2.322 hashes distintos, bits 0..2034.
  O jogo guarda as flags num mapa de 2.560 bits (`0xA00`) dentro do objeto de save: `flag_set` (`FUN_003ea038`) acha o bit
  (`FUN_0031a1b8`) e liga/desliga (`FUN_002ae46c`). Os scripts escrevem 560 flags (489 com definição, 71 sem) e leem 63; 1.221
  escritas: mapas 469, fases 390, sistema 280, comum 32, eventos 29, menus 21. Ainda NÃO tratadas no port (só `time_lock`).
- **Uso dos opcodes de batalha**: 6xxx (objetos 3D), 4xxx (câmera), 14060, 14802 e 14804 (som) são quase só dos 299 scripts de golpe
  (`seq/battle/spcmnd/`); nos eventos da história o trabalho passa pelo `op2110` (53 subcomandos, 20.048 chamadas; o port trata 8).
- **Som**: `snd/product/yw_sound_en.bcsar` = CSAR 2.3.0.0, 5.271.160 B, blocos STRG (2.325 nomes: `seq_` 429, `bank_` 175, `evNN_…`),
  INFO (tabelas 0x2100=1.714 [sons, pela documentação pública], 0x2104=373, 0x2101=175, 0x2103=175, 0x2105=52, 0x2102=10,
  0x2106=1.573 [arquivos]) e FILE (4,8 MB). `se_play` usa nome (39, todos presentes no banco) ou número (53 ids, 0..321).
  `data/snd/cfg/event/*.cfg.bin`: 263 arquivos, 194 com `EVENT [?, hash, ?, ?]` (1.457 registros); o hash NÃO é o crc32 dos
  nomes do banco (0/1.457). Sonda: `tools/bcsar_probe.py`.
- **Materiais** (amostra de 260 pacotes): `.mtr` `MTRC00` (76 B em 80 %), `.atr` `ATRC01` (36..52 B), `.txp` (36 B sempre; 2 hashes +
  zeros + 1,0 1,0), `.cmn` (12 B em 97 %), `.ref` (25 B = caminho de biblioteca de texturas, ex. `data/map/t102g00/t1020.xr`),
  `.imm2` `XIMA`, `.mtm2` `XMTM`, `.sil` `XCSL`. Funções ainda não decifradas [H].
- **Texturas** (IMGC `fmt/bd`): 7.958 em 12 combinações; ETC1/ETC1A4 (`0x1b/0x1c`) 3.697 = 82 % dos pixels; RGBA8 1.357.

## Flags por tipo, câmera por parâmetros e "terminou?" (2026-11-06, 4ª etapa)
- **flag_config**: `FLAG_INFO_BEGIN(tipo, n)` abre uma seção; `FLAG_INFO(bit, hash)` são os registros. **Cada tipo tem numeração de bits própria.**
  `flag_config_0.01` (2.328 registros; tipos 0, 1, 6, 7, 8) e `flag_config` (1.012; tipos 0, 1, 9, 17) são tabelas DIFERENTES: dos 85 hashes em
  comum, 76 mudam de bit; 651 flags só da antiga cairiam em células já usadas na nova. Por isso o port guarda por hash (`engine/src/flags.rs`).
  Tipo por opcode, medido nos scripts: 14000/14001 → 0 (971/116 chamadas), 14011/14012 → 6, 14003 → 7 e 8, 14005/14006 → 8.
  Handlers de 14003, 14004 e 14011 NÃO estão no export do Ghidra (o REA mostrou que fazem parte do fim de uma função vizinha, `FUN_002db830`): semântica [H].
- **REA**: aceita OFFSET DE ARQUIVO do PE-wrapper, não VA: arg = VA − 0xFFE00 (ex.: VA 0x3dc6fc → 0x2dc8fc). Precisa de `GHIDRA_INSTALL_DIR` e `JAVA_HOME` no shell.
- **Câmera de evento por parâmetros**: `data/res/event/<evento>.cfg` (texto): `CAMERA_ITEM "nome", x, y, z, alvoX, alvoY, alvoZ, fov, near, far;`.
  `SetCameraParamData2(cam, quadros, "nome", …)` e `SetCameraParamData` (a 4102/4103 etc. dentro de `CtrlCameraPos/Ref/Fov` interpolam em `quadros × 2`);
  `CtrlCameraShake(cam, quadros, amplitude, velocidade, …)` (amplitude 0 = parar).
- **op2110**: 14007(obj, clipe, [slot]) toca o clipe pelo NOME (nome terminado em `L` repete; os outros tocam uma vez); 14009(obj) = "o movimento terminou?"
  (o script repete a consulta até vir verdadeira); op6813(obj) = o mesmo para objeto 3D. 14020(obj, rx, ry, rz) é ROTAÇÃO em graus (valores medidos: 0 0 0, 0 0 -90, 90 0 0, 0 70 0), não escala.
- Opcodes 4xxx/6xxx/14060/14802/14804 são dos 299 scripts de golpe da batalha (`seq/battle/spcmnd/`), não de eventos.

## Correções da 5ª etapa (2026-11-07)
- **`op2110` sub 11004** é `(3, nome, caminho.xc, 0, 0, 0)`: o `3` é o tipo. O host lia nome e caminho uma posição antes, então `point_a`, o céu e todo objeto de cena nunca carregavam e os atores presos ao `point_a` ficavam sem posição.
- **Malhas rígidas** (efeitos, objetos; nenhum vértice com peso de osso): os vértices estão no espaço do PRÓPRIO nó e vão ao mundo pela pose dele. Nas malhas com peso eles já estão no espaço do modelo. O "céu" da `ev01_0005` são 26 planos de imagem fixa (`ma-fix_img_N`) em volta de nós do objeto `ev010005001000`.
- **Menus 3D** (`CharaMakeMenu`): `op3702(recurso, caminho sem .xc)` modelo `_p00` / movimentos `_p10`; `op6002` cria objeto e devolve o nome; `op6800/3730` ligam modelo/movimento; `op6810/6819` tocam clipe; `op6202/6222/6223/6227` escala/posição/rotação; câmera `op4102/4122/4142`.
- **Tutoriais**: `data/res/text/system_text_en.cfg.bin` `TEXT_INFO(crc32("sys_info_win_NN"), 0, texto)`; `OpenInfoWin(0, id, 0, 'tr0004')`.
- `sky/*.xk`: `default`/`talk` são XPCK com `SKY.bin` de 28 bytes; `s000/s001/middle/s999` têm outro cabeçalho (`XFSP`). Não decifrados.
