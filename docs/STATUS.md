# Estado do projeto e roadmap

Meta declarada pelo usuário: *"readaptar o jogo para que seja rodável nativamente no PC"*, com suporte a mods, **para aprender
o processo**. Estratégia: (1) entender o jogo (arquivos, serviços, GPU); (2) reimplementar o que o 3DS fornece (HLE de serviços
+ renderizador PICA) ; (3) recompilar/portar o código ARM (`code.bin`).

## Feito (verificado)
| Área | Resultado | Onde |
|---|---|---|
| Extração | CIA → ExeFS/RomFS; `code.bin` descompactado e carregado no Ghidra com mapa de memória correto | ENVIRONMENT.md |
| Serviços (IPC) | 276 funções com `SendSyncRequest` inventariadas; 64 mapeadas a serviço via GDB; 18 nomes de comando confirmados | IPC-SERVICES.md, `data/ipc/` |
| GPU/GSP | Arquitetura GX (fila de comandos, interrupções) mapeada; funções-chave identificadas | GPU-PICA.md |
| Listas PICA | Decodificador com nomes oficiais de registrador; captura de um quadro (7 listas, 71 draws) | `tools/pica_decode.py`, `tools/frame_sim.py` |
| Quadro renderizado | Render target decodificado da VRAM → imagem correta do jogo (400×240) | `docs/img/rt480_final.png` |
| Texturas do quadro | RGBA8/RGBA4/IA8/ETC1/ETC1A4 decodificadas (17 únicas) | `docs/img/tex_contact.png`, `tools/frame_sim.py` |
| Vértices | Layout dos buffers lidos do heap linear (pos/cor/normal/UV/pesos), índices u16, triangle strip | GPU-PICA.md |
| Arquivo do jogo | `yw1_a.fa` (ARC0) lido: 20.835 arquivos com caminhos reais, tudo extraído | FORMATS.md |
| Texturas do jogo | 1.233 `.xi` (IMGC) → PNG, sem erros | `docs/img/xi_sample.png` |
| Modelos | `.xc` (XPCK) → 1.590 `.prm` (XMPR) → OBJ+UV, sem erros; ex.: Whisper com textura | `docs/img/models_sample.png`, `y001000_textured.png` |

## Não feito / hipóteses em aberto
* **Vertex shader PICA**: desmontador (`tools/pica_shader_disasm.py`) e tradutor para GLSL (`tools/pica_shader_to_glsl.py`,
  com reconstrução de `if/else` do `ifc`) prontos e validados contra as 441 instruções reais — na verdade 7 programas
  concatenados em `data/gpu/shader_code.bin`. Achado: o bloco com `ifc` é **skinning de 4 ossos no próprio shader**
  (índices em `v8`, pesos em `v7`, matrizes em `c18..c20[+a0.x/a0.y]`), não "iluminação de 2 luzes" como se pensava
  antes — ver `docs/GPU-PICA.md`. Teste mínimo de Vulkan (`tools/vk_min_test.py`) confirma instância/device/fila/
  command-pool OK nesta máquina (Radeon RX 5500 XT, driver Vulkan 1.4). **Falta**: ler `VSH_OUTMAP_*` real p/ confirmar
  a que semântica cada saída `o0..o6` corresponde (hoje hipótese); instalar `glslang`/`shaderc` (sem toolchain C/C++
  nesta máquina ainda) p/ compilar os `.vert` gerados para SPIR-V; montar pipeline Vulkan mínimo e comparar o resultado
  contra `docs/img/rt480_final.png`; e desmontar/validar o 2º shader maior (1323 palavras) visto no quadro (mem. de
  código do PICA tem 512 palavras) — ainda não capturado em `data/gpu/`.
* **Materiais**: `.mtr` (`MTRC00` + tabelas `LUTC00` de iluminação) e `.atr`, `.txp`, `.cmn`, `.lkat` não decodificados. Por isso
  malha→textura ainda não é exata (o export usa os `.xi` do mesmo `.xc`).
* **Esqueleto e animação**: `.mbn` (168 bytes/osso, hashes), `.mtn2/.mtm2/.mtninf/.imminf` não lidos. Malhas saem em pose de bind.
* **Outros formatos**: `XQ32` (scripts, 1.319 arquivos), `XSCR` (`.xs`), `.npcbin`/`.bin`/`.cfg` de mapas e NPCs, `.ralcam`, `.mapenv`, `.moflex`
  (vídeo), `.bcsar` (som), `.ctpk`. Mapas (`data/map/*`) e UI não foram exportados.
* **Serviços**: ~200 das 276 funções IPC ainda sem serviço/nome (só 73 sites `svc 0x32` foram atingidos nas execuções).
* **Dúvidas técnicas abertas**:
  - As listas 1 e 3 do quadro capturado têm exatamente os mesmos 31 draws (mesmo RT/viewport). Pode ser passe duplicado ou olho
    direito do 3D; **não confirmado**.
  - Duas texturas (512×1024, formato RGBA5551 segundo o registrador) saem como ruído — formato/endereço provavelmente errado.
    (`0x180fd200` na VRAM e `0x21910e00` no heap linear.)
  - Ordem exata do scaling do `DisplayTransfer` (flags `0x1001` observadas; bits de downscale não verificados). A conta de 480×400→400×240
    veio de viewport/scissor, não do flag.
  - CiTrace (gravador de comandos do Azahar) não gerou arquivo; foi substituído por GDB.

## Próximos passos (ordem sugerida)
1. ~~Desassemblador PICA~~ e ~~tradutor GLSL~~ feitos (`tools/pica_shader_disasm.py`, `tools/pica_shader_to_glsl.py`).
   ~~Teste mínimo de Vulkan~~ feito (`tools/vk_min_test.py`). Próximo: instalar `glslang`/`shaderc` (MSYS2 `pacman -S
   mingw-w64-ucrt-x86_64-shaderc mingw-w64-ucrt-x86_64-glslang` ou equivalente) para compilar os `.vert` gerados para
   SPIR-V, e ler `VSH_OUTMAP_*` real da captura para confirmar a semântica de `o0..o6`.
2. Renderizador mínimo em **Vulkan** que reproduz o quadro capturado sem o Azahar (uniforms de matriz vêm de `VSH_FLOATUNIFORM_*`).
   Verificação: renderizar os 71 draws de `data/capture/cap` e comparar com `docs/img/rt480_final.png`.
3. Decodificar `.mtr` (materiais) e `.mbn`/`.mtn2` (esqueleto/animação); exportar glTF.
4. Completar o mapa de serviços e começar o design do HLE (APT, FS/archive `rom:`, GSP, HID, DSP, CFG, PTM...).
5. Recompilação estática do ARM11 (ARMv6K, modo ARM) — ver notas em ENVIRONMENT.md sobre o `code.bin`.
