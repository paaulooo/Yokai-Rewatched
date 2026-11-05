# GPU: GSP / GX / PICA200

## Como o jogo desenha (código do `code.bin`)
* Serviço `gsp::Gpu`. Init `FUN_00139078`; thread de interrupções `FUN_0013b780`; init das filas `FUN_0013b6e0`; fila de comandos GX na memória
  compartilhada do GSP (15 slots × 0x20 B em `+0x800 + 0x200*threadId`); empurra comando `FUN_003035cc`; executor da fila de requisições `FUN_00307d00`;
  `TriggerCmdReqQueue` (IPC header `0xc0000`) = `FUN_0013d60c`.
* Wrappers GX: `00138e20`, `00138ea0`, **`00138f44` = ProcessCommandList (breakpoint dos scripts GDB, r0=endereço da lista, r1=tamanho)**, `00139014`, `00139254`.
  Chamadores: `00137134`, `00137168`, `00137188`, `00137148`, `00307d00`.
* Comandos GX: 0 RequestDMA · 1 ProcessCommandList · 2 MemoryFill · 3 DisplayTransfer · 4 TextureCopy · 5 FlushCacheRegions. Interrupções (7 callbacks): PSC0/PSC1/VBlank0/VBlank1/PPF/P3D/DMA.

## Endereços
VRAM: VA `0x1F000000` / PA `0x18000000` (6 MB). Heap linear: VA `0x14000000` ↔ PA `0x20000000` (FCRAM, 64 MB neste jogo).
Registradores PICA guardam endereços `>>3`: `addr = reg*8`. Ex.: `TEXUNIT0_ADDR1=0x0302fa40` → PA `0x1817d200` → offset VRAM `0x17d200`.

## Formato da lista de comandos (`tools/pica_decode.py`)
Pares `(dado, cabeçalho)` em u32 LE. Cabeçalho: `reg = bits 0-15`, `máscara de bytes = bits 16-19`, `parâmetros extras = bits 20-27`, `modo consecutivo = bit 31`.
Depois do cabeçalho vêm `extra` palavras; cada comando é alinhado a 8 bytes (cabeçalho de padding `0x8025e`). Fim da lista: `FINALIZE (0x010) = 0x12345678`.
Nomes oficiais: `tools/pica_regnames.json` (copiados do depurador PICA do Azahar → "Copiar tudo").
Correções que já custaram caro: `0x11c` = DEPTHBUFFER_LOC, `0x11d` = COLORBUFFER_LOC; `0x111` = FRAMEBUFFER_FLUSH, `0x110` = FRAMEBUFFER_INVALIDATE.

## Estrutura de um quadro (captura em `data/capture/cap`, `list_01..07`)
7 listas: **setup (0x270 B) → draws do 3D → troca de RT (0xa0) → draws → troca de RT (0xa0) → draws da tela de baixo → fim (0x50)**.
71 draws no total, 100 % `DRAWELEMENTS` (nunca `DRAWARRAYS`), triangle strip.
* RT da tela de cima: `FRAMEBUFFER_DIM=0x0118f1e0` → 480×400 (rotacionado). `VIEWPORT_WIDTH/HEIGHT` (float24 `0x46e000`→240, `0x469000`→200; o PICA guarda a metade) = 480×400.
  **Isso é 2× no eixo da altura da tela** (supersampling/FSAA do 3DS); o DisplayTransfer reduz para 400×240. Depois do decode: `transpose` + flip vertical (framebuffer é guardado de baixo para cima).
  Tela de baixo: 240×320 (rotacionado) = 320×240. Cor `0x18244700`, profundidade `0x18537e00`; DisplayTransfer `0x1f244700 → 0x1f346500` (flags `0x1001`).
* Recursos usados: 1 vertex shader por draw (uploads de 441 e 1323 palavras), **skinning por 4 influências de osso no proprio vertex
  shader** (ver abaixo — corrigido; achava-se que era so iluminação), iluminação por hardware com LUTs (2 luzes), TEXENV0–5, blend/depth/stencil/alpha test/scissor/culling,
  registradores GSH; **não** vistos: fog, proctex, gas.
* As listas 1 e 3 (31 draws idênticos) — pergunta aberta (ver STATUS.md).

## Decodificação de texturas do PICA (`tools/frame_sim.py`, `tools/tex_decode.py`)
`TEXUNIT0_DIM` = `(largura<<16)|altura`; `TEXUNIT0_ADDR1`×8 = endereço físico; `TEXUNIT0_TYPE & 0xF` = formato:
0 RGBA8 (bytes A,B,G,R) · 1 RGB8 · 2 RGBA5551 · 3 RGB565 · 4 RGBA4 · 5 IA8 · 6 RG8 · 7 I8 · 8 A8 · 9 IA4 · 10 I4 · 11 A4 · 12 ETC1 · 13 ETC1A4.
Tiles 8×8 com Morton (X no bit 0), linhas de baixo para cima. ETC1: blocos de 8 B (ETC1A4: 16 B, alfa 4 bpp antes), 4 blocos por tile em ordem X-primeiro, valor lido como u64 LE =
bloco ETC1 big-endian padrão; pixels em ordem coluna-maior. (Contraste com o IMGC dos arquivos: Y no bit 0.)

## Vértices (`frame_sim.attrs / vertices`)
`ATTRIBBUFFERS_LOC` (0x200..): base (`reg*8`), formato dos 12 atributos (4 bits: tipo `&3` = 0 i8, 1 u8, 2 i16, 3 float; tamanho `(>>2)+1`), contagem em `[31:28]` do high;
por buffer: `offset`, `ids de componente` (4 bits cada; ≥12 = padding), `bytes por vértice` (bits 16-23), `nº componentes` (bits 28-31). `INDEXBUFFER_CONFIG`: bit 31 = u16, bits 0-27 = offset a partir da base.
`VSH_ATTRIBUTES_PERMUTATION_*` mapeia atributo→registrador de entrada do shader. Exemplos reais: UI = quad com pos em pixels + UV em texels (atlas); cenário = pos float3 / cor float4 / UV float2 (36 B);
personagem = 80 B (pos, normal, UV, 3×float4 de pesos).

## Shader
`data/gpu/shader_code.bin` (441 instruções de 32 bits) e `shader_opdescs.bin` (61 descritores de operando) — extraídos de `VSH_CODETRANSFER_DATA*` e `VSH_OPDESCS_DATA*` (regs `0x2cc`, `0x2d6`).
Entrada: `VSH_ENTRYPOINT` (0x2ba), `VSH_OUTMAP_*`/`SH_OUTMAP_O0..` (mapa de saídas), uniforms float em `VSH_FLOATUNIFORM_*` (0x2c0/0x2c1; matrizes 4×4 como 4 vec4), bool em `VSH_BOOLUNIFORM`.

**Desmontador pronto**: `tools/pica_shader_disasm.py` (sem argumentos lê `data/gpu/shader_code.bin`/`shader_opdescs.bin`).
Encoding de instrução verificado contra o código-fonte do **nihstro** (github.com/neobrain/nihstro, `shader_bytecode.h`, BSD-3-Clause;
implementação de referência do 3dbrew "Shader Instruction Set") e validado rodando nas 441 instruções reais: 0 opcodes
desconhecidos, saída coerente (dp4 linha-a-linha de matriz view/model, extensão de posição p/ vec4 via `c91.y`≈1.0,
`o1 = c11 * v1` como cor, bloco `mad`/`cmp`/`ifc` de iluminação com 2 luzes — bate com o que já estava mapeado acima).
Resumo do encoding (bits, LSB=0):
* Opcode: bits 26-31 (6 bits). Tabela completa dos opcodes em `OPS` no script.
* Formato aritmético comum (ADD/DP3/DP4/MUL/MAX/MIN/MOV/RCP/RSQ/...): `operand_desc_id`=0-6, `src2`=7-11, `src1`=12-18,
  `idx`(endereçamento relativo a0.x/a0.y/aL)=19-20, `dest`=21-25.
* `CMP` (0x2e/0x2f): mesmos campos de src1/src2/idx; bits 21-23 = op da comparação de Y, bits 24-26 = op de X
  (0 `==` 1 `!=` 2 `<` 3 `<=` 4 `>` 5 `>=`); escreve em `cc.xy` (condition codes), não em `dest`.
* `MAD`/`MADI` (0x38-0x3f / 0x30-0x37): `operand_desc_id`=0-4, `src3`=5-11, `src2`=10/12-16, `src1`=17-21, `idx`=22-23, `dest`=24-28.
* Fluxo (`IFC/JMPC/CALLC/BREAKC` formato 2; `IFU/CALLU/LOOP/JMPU` formato 3): `num_instructions`=0-7, `dest_offset`
  (destino absoluto, em palavras)=10-21, `op`/`bool_uniform_id`/`int_uniform_id`=22-25, `refy`=24, `refx`=25 — **a semântica exata
  de refx/refy/op (qual combinação AND/OR/negação dos condition codes) ainda é HIPÓTESE**, não confirmada por execução real.
* Operand descriptor (`shader_opdescs.bin`, palavra de 32 bits): `dest_mask`=0-3 (bit3=x...bit0=w), `negate_src1`=4,
  `src1_selector`=5-12 (4× 2 bits; o subcampo em 11-12 escolhe o componente de saída `.x`, 9-10→`.y`, 7-8→`.z`, 5-6→`.w`;
  cada subcampo vale 0=x,1=y,2=z,3=w da fonte), `negate_src2`=13, `src2_selector`=14-21 (mesmo esquema, `.x` em 20-21),
  `negate_src3`=22, `src3_selector`=23-30 (`.x` em 29-30). Confirmado por `nihstro/shader_bytecode.h`.

**Tradutor para GLSL pronto**: `tools/pica_shader_to_glsl.py` (sem argumentos lê os mesmos defaults e escreve um `.vert`
por "programa" em `work/shaders/` — `shader_code.bin` tem 7 programas concatenados, cada um terminado em `END`+`NOP`
de padding, um por upload/entry-point do GSP, não um só shader de 441 instruções). Cobre os opcodes vistos de fato:
`add/cmp/dp3/dp4/flr/ifc/mad/madi/max/min/mov/mova/mul/rcp/rsq`; qualquer outro vira comentário `// TODO` em vez de
código advinhado.

**Achado ao validar a tradução** (corrige a hipótese acima): o bloco com `ifc` em `prog3`/`prog4`/`prog5` (0x0069+)
não é só iluminação — é **skinning de 4 ossos dentro do próprio vertex shader**: `v8` (4 floats) guarda os índices
dos ossos, escalados por `c[91].w` e truncados para inteiro via `mova → a0x/a0y`; `v7` (4 floats) guarda os pesos;
o shader acumula `r5/r6 += dot(c[18+a0]..c[20+a0], pos) * peso` quatro vezes (duas por `mova`, duas por `a0x`/`a0y`
do mesmo `mova`) — ou seja, as matrizes de osso vivem em `c18..c20`(×N ossos) e a GPU faz o skinning via endereçamento
relativo às uniforms, não a CPU. O bloco `ifc`/`cmp` por si (comparação contra `-c91.y`, guarda de divisão-por-zero
antes de `rcp`/`rsq`) é a normalização do vetor resultante (visada/especular), não a luz em si.
Semântica do `ifc` confirmada batendo o ponto de união contra o próprio desmontado: bloco verdadeiro = `[ifc+1, dest)`,
bloco falso = `[dest, dest+num)`, continuação sempre em `dest+num` (conferido nos 3 casos reais — o código que começa
ali é sempre uma instrução nova, não repetida). A combinação exata do condicional (`op`/`refx`/`refy`) é HIPÓTESE
ainda (nos 4 casos reais `cc.x==cc.y` sempre, então não dá para distinguir a ordem do enum Or/And/JustX/JustY).

Teste mínimo de ambiente: `tools/vk_min_test.py` confirma que o Vulkan 1.4 do driver (Radeon RX 5500 XT) cria
instância/device/fila/command pool sem erro nesta máquina (via binding Python `vulkan`, não precisa de compilador
C/C++ instalado ainda).

Próximo passo: 1) ler `VSH_OUTMAP_*` real da captura para confirmar a que semântica cada `o0..o6` corresponde
(hoje é HIPÓTESE: o0=posição, o1=cor, o2/o3=UV, o4=? , o5=normal transformado, o6=specular/rim); 2) instalar
toolchain C/C++ + `glslang`/`shaderc` (MSYS2 `pacman`) para compilar os `.vert` gerados para SPIR-V; 3) montar o
pipeline mínimo em Vulkan (swapchain + 1 draw) e comparar contra `docs/img/rt480_final.png`.
