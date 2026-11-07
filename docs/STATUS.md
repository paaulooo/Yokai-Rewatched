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
| **Runtime Rust** | `engine/` (crate `yw1`): executável com janela de 2 telas que toca o boot — logo Level-5 → abertura (`yw1_op.moflex`, olho esquerdo do estéreo) com a música (`yw1_op_en.bcstm`, decodificador DSP-ADPCM nativo, relógio do áudio sincroniza o vídeo) → menu de título | `engine/`, FORMATS.md (`.moflex`, `.bcstm`) |
| **Leitores em Rust** | `engine/src/assets/`: L5, ARC0 (lê os 20.835 arquivos do `.fa`), XPCK, IMGC (todos os formatos; idêntico pixel a pixel ao `tools/imgc.py` em RGBA8, RGBA4, ETC1 e ETC1A4), XPVB/XPVI, ANMC00 | testes em `cargo test` |
| **Menu de título (Rust)** | Reconstruído dos layouts reais (`data/menu/title_{u,d}00_en.xa`) **e da lógica do script** `seq/menu/TitleMenu.xq` (XQ32 desmontado): fade de entrada, logo em fade de 120 quadros, música `M01_MAIN_THEME`, botões conforme o save (sem save só "New Game"), modo demonstração após 30 s ociosos (repete a abertura), saída com moldura ao contrário + fades. Animações = flipbooks do ANMC. Entrada: teclado, mouse como toque e gamepads (Xbox, PS4/PS5, Switch Pro, genéricos SDL) via `gilrs` | `engine/src/title.rs`, `layout.rs`, `input.rs`, FORMATS.md (layouts) |
| **Texturas embutidas** | `.xi` dentro de `.xa` (menus) e `.xc` (mapas) + texturas soltas dos mapas (`<id>.xi`, `<id>n.xi`) → PNG em `work/embedded_xi/`, com `manifest.json` (tamanho/fmt por textura). Escopo padrão: 80 `.xa` da raiz de `data/menu/` + quartos `t101i01..i04` (172 + 44 texturas); idempotente e incremental (só o que falta) | `tools/export_embedded_xi.py` (--map t101i02, --xa <arquivo>, --all p/ subpastas de data/menu/) |
| **Quarto 3D (Rust)** | `t101i02` (quarto do Nate) carregado nativamente: `.xc` de mapa → malhas XMPR (4.053 triângulos), materiais/texturas via `RES.bin`, câmera do `mapenv` (Fov 35, Dist 80, RotV 45), rasterizador 3D em software com **resolução interna ajustável** (F1/F2: 1×=400×240 … 6×), jogador provisório (caixa) com colisão contra as paredes da malha de colisão. `New Game` no título leva direto ao quarto (atalho) | `engine/src/room.rs`, `gfx3d.rs`, `assets/{xmpr,chrc,cfgbin}.rs`, `settings.rs`; `YW_START=room` |

| **Interpretador XQ32 (Rust)** | `engine/src/xq.rs`: VM com corrotinas, chamadas entre scripts (cena + `prog_menu_001e`/`prog_common_001e`), contexto `$0..` por corrotina. Roda o `TitleMenu.xq` e a **cadeia real do New Game**: SaveLoadMenu (2111, escolher slot) → CharaMakeMenu (2060, menino/menina) → NameMenu (2070) → 1010 (abre o quarto). Host de menus (`menu_host.rs`): layouts, objetos 2D, botões em matriz com foco, música por cena, troca de cena. Teste `new_game_chain` percorre tudo sem janela | `xq.rs`, `menu_host.rs`, `scene.rs`; `YW_START=newgame` |

## Não feito / hipóteses em aberto
* **Vertex shader PICA**: desmontador (`tools/pica_shader_disasm.py`) e tradutor para GLSL (`tools/pica_shader_to_glsl.py`,
  com reconstrução de `if/else` do `ifc`) prontos e validados contra as 441 instruções reais — na verdade 7 programas
  concatenados em `data/gpu/shader_code.bin`. Achado: o bloco com `ifc` é **skinning de 4 ossos no próprio shader**
  (índices em `v8`, pesos em `v7`, matrizes em `c18..c20[+a0.x/a0.y]`), não "iluminação de 2 luzes" como se pensava
  antes — ver `docs/GPU-PICA.md`. Teste mínimo de Vulkan (`tools/vk_min_test.py`) confirma instância/device/fila/
  command-pool OK nesta máquina (Radeon RX 5500 XT, driver Vulkan 1.4). **`VSH_OUTMAP_*` real da captura lido e
  CONFIRMADO** (não é mais hipótese): `o0`=posição, `o1`=cor, `o2`=UV0(+w), `o3`=UV1, `o4`=UV2, `o5`=view (visada),
  `o6`=quaternion/normquat (orientação p/ normal) — ver tabela em `docs/GPU-PICA.md`. `glslang`/`shaderc` instalados
  (MSYS2 UCRT64) e os 7 `.vert` gerados **compilam para SPIR-V sem erro e validam** (`work/shaders/*.spv`).
  **Uniforms float (`c[0..95]`) decodificados e validados** (`tools/pica_uniforms.py`) — float32 direto e o
  empacotamento float24 de 3 palavras/vec4 decifrado contra dados reais (ver `docs/GPU-PICA.md`).
  **Pipeline mínimo em Vulkan funcionando** (`tools/vk_render_min.py`): renderiza off-screen com o SPIR-V real de
  `prog0` + geometria real da captura, resultado em `work/shaders/vk_render_min.png`. Achado em aberto: os uniforms
  REAIS do draw 0 rodados por `prog0` dão resultado degenerado (matriz "mundo" ignora `v0.x`) — a correspondência
  draw→programa de shader (`VSH_ENTRYPOINT`) ainda não está validada, então o teste Vulkan usa uma matriz construída
  à mão, não os uniforms reais. **Falta**: resolver essa correspondência p/ comparar pixel a pixel contra
  `docs/img/rt480_final.png`; desmontar/validar o 2º shader maior (1323 palavras) visto no quadro (mem. de código do
  PICA tem 512 palavras) — ainda não capturado em `data/gpu/`; adicionar sampling de textura real ao pipeline; e
  mapear `TEXENV_*` (combinação fixa de texturas, faz o papel de "fragment shader" no PICA200).
* **Materiais**: `.mtr` (`MTRC00` + tabelas `LUTC00` de iluminação) e `.atr`, `.txp`, `.cmn`, `.lkat` não decodificados. Por isso
  malha→textura ainda não é exata (o export usa os `.xi` do mesmo `.xc`).
* **Esqueleto e animação**: `.mbn` (168 bytes/osso, hashes), `.mtn2/.mtm2/.mtninf/.imminf` não lidos. Malhas saem em pose de bind.
* **Outros formatos**: `XSCR` (`.xs`), `.npcbin`/`.bin`/`.cfg` de mapas e NPCs, `.ralcam`, `.mapenv`, `.moflex`
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

## Arquitetura decidida (2026-11-05)
O jogo roda em **Rust** (`engine/`); Python fica só para extração/análise. O menu de título foi **reimplementado** em
Rust a partir dos dados e do script `TitleMenu.xq` (não recompilado). O que ainda é interpretação/hipótese está anotado
em `engine/src/title.rs`: significado dos opcodes nativos (fades, flags de animação 2/6/12), duração do estado `push`
dos botões, ordem de desenho em empates de z. A voz `title_call_*` não é usada por este script.
Seguir adiante (o jogo depois do menu) exige decifrar XQ32 e/ou a lógica do `code.bin`, o renderizador PICA em Rust
(wgpu/Vulkan, reaproveitando o tradutor de shader) e um decodificador Mobiclip nativo (hoje o vídeo vem do ffmpeg).

## Próximos passos (ordem sugerida)
1. ~~Desassemblador PICA~~, ~~tradutor GLSL~~, ~~teste mínimo de Vulkan~~, ~~leitura de `VSH_OUTMAP_*` real~~,
   ~~instalação de `glslang`/`shaderc`~~, ~~compilação dos `.vert` para SPIR-V~~, ~~decodificador de uniforms float~~
   e ~~pipeline mínimo em Vulkan (1 draw, geometria real + shader real)~~ feitos (`tools/pica_shader_disasm.py`,
   `tools/pica_shader_to_glsl.py`, `tools/pica_uniforms.py`, `tools/vk_render_min.py`, `work/shaders/*.spv`,
   `work/shaders/vk_render_min.png`).
2. ~~Desmontar XQ32~~ (`tools/xq32_disasm.py`). Próximo: interpretador XQ32 em Rust (rodar os scripts em vez de
   reescrever cada um), começando pelos opcodes vistos no título; cena 2111 (novo jogo/continuar); saves; letra da
   abertura (`movietext`); efeitos sonoros do menu (`.bcsar`).
3. Resolver a correspondência real draw→programa de shader (`VSH_ENTRYPOINT`) para poder usar os uniforms reais
   (hoje o pipeline mínimo usa uma matriz de teste construída à mão — ver achado em `docs/GPU-PICA.md`); depois
   expandir para os 71 draws de `data/capture/cap`, adicionar texturas reais (sampler/`VkImage`) e comparar com
   `docs/img/rt480_final.png`.
4. Decodificar `.mtr` (materiais) e `.mbn`/`.mtn2` (esqueleto/animação); exportar glTF.
5. Completar o mapa de serviços e começar o design do HLE (APT, FS/archive `rom:`, GSP, HID, DSP, CFG, PTM...).
6. Recompilação estática do ARM11 (ARMv6K, modo ARM) — ver notas em ENVIRONMENT.md sobre o `code.bin`.

## Rumo ao primeiro quarto jogável (pesquisa de 2026-11-06; notas brutas em `work/notes/`, não versionadas)
* Quarto do menino = **`t101i02`** (cama, escrivaninha, espelho); `t101i01` é o térreo. Spawn do jogador: (-10, 0, 15.76) ângulo 90 (`SetPosLeaderChara` do Phase1). Menina = `t101i04`.
* Fluxo do New Game (scripts XQ32): Título → `op2104(2111)` SaveLoadMenu (escolha de slot) → 2060 CharaMakeMenu (menino/menina) → 2070 NameMenu (nome) → 1010 campo → fase 1:
  `ev01_0003` (aviso em preto) → `ev01_0005a/b` (**1ª cutscene 3D**, 473 quadros, câmera por `.xv`, personagens c526/c903/c102/c103/c534 + yo-kai d003/d012/d001) → warp `t101g00` → `ev01_0010a/b` (rua, 19 falas) → warp `t101i02` → `ev01_0020a/b` (entra correndo no quarto; **fim: jogador assume controle**).
* Falta para chegar lá: interpretador XQ32 (plano mínimo em `work/notes/xq32_newgame_plan.md`; 213 funções/265 opcodes alcançáveis, 60 subcódigos de `op2110`), esqueleto+animação de personagem (`.mbn`/`.mtn2`, não decifrados), materiais reais (`.mtr/.atr` — hoje a mistura é heurística: textura com >5% de texels semitransparentes = alfa; camadas `-s0` omitidas; alfa reduzido a 0.35 como PROVISÓRIO), câmera por keyframes (`.xv`), texto de diálogo (`data/txt/ev/*.cfg.bin` = cfg.bin).
* Hipóteses da câmera de jogo (`room.rs`): Fov vertical, Dist ao alvo (jogador+6 em y), câmera no lado +z. Não lidas do código do jogo.

### Interpretador XQ32 — o que foi corrigido/descoberto ao rodar os scripts (2026-11-06)
* `op121` = E e `op122` = OU (o catálogo tinha invertido; só assim `IsMessageNext` e `LoadMenu_Select` fazem sentido).
* Comparações: 130 =, 131 ≠, 132 ≥, 133 ≤, 134 >, 135 < (HIPÓTESE que fecha todos os `for`/esperas vistos).
* `$0..$999` é contexto **da corrotina**: argumentos da cena no script principal; nos callbacks de objeto, os parâmetros do objeto (`op5046(obj, i, v)` → `$i`; 7/8/9 = animações `_off/_on/_push`).
* Laços de espera sem `yield` (esperando outra corrotina) cedem o quadro após 256 saltos para trás (HIPÓTESE do comportamento do motor).
* `op5248` = mover até posição absoluta em N quadros; `op5250` = "ainda se movendo?"; `op12054(slot)` bit 1024 = compatível com Lens.
* Provisório: janelas de mensagem/sim-não respondem "sim" na hora (`Common_SelectYN`, `OpenInfoWinStrEx` interceptadas); **sem texto** (fonte `fnt/*.xf` não decifrada); teclado de nome não funciona — Start confirma com "Nate"/"Katie"; a pilha de cenas (`op2104` empilha, `op2102` desempilha?) é tratada como troca simples; o título continua o reconstruído à mão (`title.rs`).
* Faltam para os menus ficarem completos: fonte/texto, callbacks de controle (`op1110/1113`), modelos 3D do CharaMake (`op37xx/6xxx`), efeitos sonoros. Para "iniciar o jogo" de verdade: scripts de fase (`seq/phs/c01_trigger`) + eventos (`ev01_0003/0005/0010/0020`) com o host 3D.
* Título: a moldura de Yo-kai (`youkai_wa`) fica oculta no título parado e só entra ao escolher New Game/Continue (`EndAnimationUpScreen`, flag 12); flag 2 = criada oculta, 6 = oculta de novo. Confirmado pelo dono comparando com o 3DS.
* **Fonte e textos (2026-11-06)**: formato `FNTC01` decifrado e portado (`assets/font.rs`); menus mostram os textos reais das tabelas `*_text_en.cfg.bin` (ex.: "Start a new file.", "Play from the beginning."). Posição/alinhamento de texto são HIPÓTESE (`op5932` largura da caixa, `op5942` 1 = centro); "Diary" ainda sai encostado no número do slot. O REA (MCP) não analisa o `code.bin` neste Windows: o adaptador Ghidra dele só aceita PE/6502 e `GHIDRA_INSTALL_DIR` não está configurado — a conferência usou o export `data/ghidra/code.bin.c`.

## Do menu até descer a escada (2026-11-06)
Fluxo jogável: título → New Game → diário (2111) → menino/menina (2060) → nome (2070, Start = nome padrão) → **loading** (layout real
`loading_en.xa`, mínimo 45 quadros) → prólogo → controle no quarto → **escada** (`MJ_t101i02_t101i01` do `_funcpt.bin`) → loading → térreo
`t101i01` ao pé da escada. Continue carrega o diário salvo.
* **Texturas/UV corrigidas**: o V é usado direto (linha 0 da imagem = v 0) em mapas e personagens — a inversão antiga escondia o erro
  em pisos/paredes e deixava móveis "esticados" e uma mancha preta (era UV errado, não mistura). Cama, pôster, tapete e estante agora
  batem com o jogo. O remendo de alfa 0,35 e a omissão das camadas `-s0` foram removidos.
* **Personagem**: Nate (`c001000`) com esqueleto `.mbn`, skinning por pesos (atributos 7/8 + tabela de hashes do XMPR) e animação XMTN
  (`c000010_p10.xc`, 55 clipes `MINF`: 立ち1L parado, 歩き1L andar, 走り1L correr...). Achado: a contagem de chaves é u16 (12 bits), não
  u8 como no Metanoia. Protótipo `tools/mtn_probe.py`, Rust `engine/src/character.rs`.
* **Diálogos**: `engine/src/dialog.rs` — textos reais `data/txt/ev/*_{m,f}_en.cfg.bin`, balão `mes00`/`talk_send` de `win_en.xa`,
  monólogo em tela preta. Prólogo (`engine/src/prologue.rs`): ev01_0003 + ev01_0020 (Nate entra correndo, 4 falas) com a música
  `ev01_0020_01`; depois `M38_keta_house` (HIPÓTESE pelo map_config).
* **Save**: `engine/src/save.rs` — 3 diários em `%APPDATA%/yw1/saves`, lidos pelo SaveLoadMenu via `op12054/12053/12050`; F5 salva;
  troca de mapa leva a posição; marca `prologue_done`.
* **REA**: funciona com o Ghidra 12.1.4 do projeto via PE ARMNT (`tools/codebin_pe.py`; ver ENVIRONMENT.md). MCP precisa reiniciar o
  Claude Code para pegar o `GHIDRA_INSTALL_DIR`.
* **Falta** (não feito nesta etapa): `ev01_0005` (1ª cutscene 3D: câmera `.xv`, movimentos por objeto, Yo-kai) e `ev01_0010` (rua
  `t101g00`, formato `.xr`) — pulados com log; efeitos sonoros (`yw_sound_en.bcsar` não lido); menina sem validação (`t101i04`, `c002000`);
  NPCs (mãe etc.); materiais `.mtr` (mistura ainda heurística); teclado do nome; depuração no Azahar não foi usada.

## Controles, cutscenes e menus (2026-11-06, 3ª etapa)
* **Cutscene `ev01_0005`** (`engine/src/cutscene.rs`): câmera XCMA decifrada (4 faixas: posição, alvo, FOV 45°, rolagem; cabeçalho
  de 16 B + bloco L5 por faixa), âncora `point_a` (todos os `.mbn`, não só os do manifesto), 9 atores presos a nós com o movimento do
  evento, cenário = rua `t101g00` (140 mil triângulos, 0,1 s). A lógica do jogo roda a **30 quadros/s** (`WaitFrame 473` = 15,8 s).
  Falta: efeito de céu (sol/arco-íris), protagonista `ei000040` (sem movimento neste evento), carros animados da rua.
* **Rua `t101g00`**: grade de blocos `t101gNN.xc` + bibliotecas de textura `.xr` (`RESC01`); o manifesto `CHRC00` agora é lido por TAG
  (220 materiais, 240 texturas, 290 registros, 100 malhas, 110 nós, **20 = bibliotecas externas**, ex. `t100g00` → `t1000.xr`).
* **Eventos pelo script** (`engine/src/event_host.rs`): `ev01_0010a/b` roda do início ao fim na VM com um host de eventos
  (atores, movimentos `ev_mot`, posições `14016`, rotas `28009/28010`, prender a nó `11046`, falas `OpenTextCaptionDetail2`, esperas
  `WaitFrame2`, câmera `LoadCameraMotion/SetDriveCameraMot`, fades). Strings da VM agora são sem perda (Shift-JIS em latin-1) para
  comparar nomes de clipe. `<PNAME>` = nome do jogador. Câmera de parâmetro (`SetCameraParamData2`) ainda é substituída por uma
  câmera de mapa atrás do protagonista.
* **Prólogo completo**: ev01_0003 (aviso) → ev01_0005 (cutscene, Start pula) → ev01_0010 (parquinho, 19 falas) → ev01_0020 (quarto).
* **Menu do campo** (X): `MainMenu_003a.xq` real (Inventory, Settings, Quests, Diary; Medallium/Yo-kai bloqueados "???" como no
  início do jogo). B fecha. Botões abrem 2040 Itens, 2160 Info, 2180 **Settings** (`ConfigMenu`), 2190 Quests; Diary (2110) salva.
  VM ganhou vetores (`op530`/`op531`, `op100` com índices).
* **Opções do jogo**: `op14150(i)`/`op14151(i, v)` = ler/gravar opção (música, efeitos, 2× AUTO/FIXED, ON/OFF; padrão 4 4 0 0 0),
  persistidas em `settings.ini` (`game_options`); volume da música do jogo aplicado.
* **Nome**: digitado pelo teclado do PC na tela 2070 (até 8 letras, Backspace apaga, Enter confirma).

## Relógio do PC (2026-11-06)
- O relógio do jogo (`op14041`, usado pelos relógios de parede dos mapas e por dia/noite) agora segue a hora local
  do PC; a data do diário (`op12055`, tela de salvar/carregar) é a do PC no momento em que se salva (`saved_at` no
  `.ini`). Formato e regras em `FORMATS.md` § "Relógio do jogo". Teste: `clock_follows_pc_and_story_overrides`.
- Ainda não há variação visual dia/noite nos mapas (iluminação/variantes noturnas) — só a hora já está disponível.

## Reatribuição de texturas (2026-11-06)
* **Auditoria malha→textura**: quartos `t101i01..i04` OK (todo material pescando textura nomeada no `RES.bin`;
  os únicos `tex_hash = 0` são `.col-`/`.snd…` — colisão/sombra, sem textura por natureza). Menus `data/menu/*.xa`:
  553 materiais, 4 com textura hash 0 → `#cover 0` em `warp_d00_en.xa` e `mm_quest_d00_en{,_f,_ko}.xa`. `gaiji.xa`
  é um ANMC sem seções de animação (offsets lixo quando n=0) — só vale tratar no probe, o runtime não o usa.
* **FALHA CORRIGIDA**: material com hash 0 derrubava o layout inteiro (`"material/textura não encontrados"`)
  no `layout.rs`, e no `menu_render.py` dava `ValueError`. Agora malha de material sem textura desenha cor
  sólida (preto, **HIPÓTESE** — confere com os nomes `bg_black`/`cover`; a cor real seria dado do material
  ainda não decifrado) em `render2d.rs::draw_triangle_flat` / `menu_render.py::tri_flat`. Hash de textura
  ausente do pacote agora avisa e fica sem textura em vez de quebrar o menu. Núcleo do rasterizador 2D
  unificado em `render2d.rs::scan` (texturizado e sólido varrem o mesmo triângulo).
* Teste novo `layout::tests::untextured_materials_load` (warp + mm_quest carregam e têm malhas sem textura);
  renders de verificação em `work/notes/render/reattr_mm_quest_d00.png` e `reattr_warp_d00.png`.
* Segue aberto: cor real do material sem textura (procurar no registro de 224 B do ANMC; `+16` = hash da
  textura, `+0` = hash do nome, `+12` = hash de uma biblioteca externa provável, `+24`/`+28` = 1.0).

## Yo-kai e batalhas — até o primeiro Yo-kai e o jogo livre (2026-11-06)
- `engine/src/yokai.rs`: banco de Yo-kai do RomFS (nomes, modelos, atributos, golpes, Soultimate, rank, encontros,
  relógio de Yo-kai por mapa). Testes: `database_matches_the_game`, `mount_wildwood_wild_yokai`.
- `engine/src/battle.rs`: batalha em tempo real (anel de até 6, 3 na frente; L/R gira, ↑/↓ alvo, A Soultimate,
  Y purificar, B fugir na natureza), modelos e clipes de batalha reais no cenário `t100b02`, HP/mensagens com peças dos
  layouts de batalha, música `M25_battle01`. Teste: `tutorial_battle_is_won`; imagem: `battle_snapshot` (ignorado).
- `engine/src/chapter1.rs`: 30 passos do capítulo 1 (18 eventos pelo script real com as conversas `EventTalkRun`,
  teleportes dos scripts de fase, Cadin entra → tutorial contra Buhu → Cadin sai → Buhu entra no time → salva) e o
  jogo livre: segurar Y parado no Mt. Wildwood/rua procura Yo-kai pelas tabelas reais (período = relógio do PC,
  relógio rank E), vitória pode trazer amizade. Teste: `chapter1_reaches_free_play` (ignorado, ≈21 mil ticks).
- Save: `party`, `money`, `chapter`. `YW_START=chapter1` / `YW_START=wild` para testar.
- [H] ainda: fórmula de dano, velocidade, chance de amizade, posições/câmera da batalha, os 8 pontos de busca
  (setor pela posição), disparo das fases (gatilhos `*_trigger.cfg.bin` em bytecode base64), minigames de
  Soultimate/purificar, itens/comida na batalha, ícones do anel, NPCs.

## Colisão, ordem do prólogo e página de progresso (2026-11-06)
- Colisão: índice XPVI formato 0 = lista (ver FORMATS). A porta da rua e a escada estão alcançáveis
  (teste `house_exits_are_reachable`); ao chegar num mapa, o jogador vai para o chão mais próximo (±12).
- Título: New Game/Continue vão para a cena 2111 (diário) do script real — antes um atalho levava direto ao quarto e
  pulava personagem, nome, monólogo e a 1ª cutscene. `YW_START=room` também passa pelo prólogo. Start só pula a cutscene
  depois de 2 s.
- Progresso no navegador: https://claude.ai/artifact/MEY6sQqW54Uvx1HGeZgQTw (só texto, sem conteúdo do jogo).

## Medição do que falta (2026-11-06)
Números reproduzíveis (`tools/progress_metrics.py` lê os JSON de `work/notes/`; os testes Rust ignorados os geram:
`cargo test --release format_coverage|event_coverage|map_coverage -- --ignored`):
- **Extração**: 105.911 de 187.318 arquivos folha lidos (56,5 %; 98,1 % dos bytes). Faltam materiais (`mtr/atr/txp/cmn`,
  71.044), NPCs (`npcbin`, 3.349), efeitos/animação de material (4.510) e ~2 mil pequenos (`bcsar` incluído).
- **Tradução**: opcodes de script 112/1.056 (10,6 %; 81,8 % das chamadas), subcomandos do `op2110` 26/289 (9,0 %; 48,8 % das
  chamadas), funções do `code.bin` lidas 19/11.987 (0,16 %). Rust ≈12,8 mil linhas.
- **Eventos**: 723 de 747 scripts terminam na VM (travam 24: ev50_0020, ev51_0460, 11 de ev70, 11 de ev98); 22 estão ligados à
  rota jogável. "Termina" não garante efeitos/flags/visual certos.
- **Mapas**: 219 de 255 ids carregam (1,75 milhão de triângulos); 7 estão na rota.
- Opcodes mais usados ainda sem tratamento: 14060 (1.116), `flag_set` 14000 (983), 6001 (921), `obj3_set` 6046 (736),
  `obj_free` 5001 (720), `se_play` 14804 (690), `cam_bind` 4010 (609).
- Página com tudo isso (privada): https://claude.ai/artifact/MEY6sQqW54Uvx1HGeZgQTw — republicar no mesmo link ao avançar.

## Seis áreas em detalhe (2026-11-06)
Flags, objetos 3D, câmera, som, texturas e materiais estão descritos com números na página de progresso e em FORMATS.md
(§ "Flags, som, materiais e câmera"). Reprodutível: `tools/area_metrics.py` (→ `work/notes/area_metrics.json`),
`tools/bcsar_probe.py`, `cargo test --release texture_stats -- --ignored --nocapture`.
**Correção**: a página anterior atribuía a câmera/objetos 3D "de evento" os opcodes 4xxx/6xxx; eles são dos golpes de batalha.

## Estado do jogo e eventos (2026-11-06, 4ª etapa)
- `engine/src/flags.rs`: flags e variáveis globais por hash, salvas no diário (`flags =`), zeradas no novo jogo, restauradas no Continue.
  Opcodes 14000/14001 (confirmados no `code.bin`) e 14003–14006/14011/14012 (hipótese pelo tipo). Testes: `flags_follow_the_game_tables`, `tables_have_the_measured_kinds`.
- `event_host.rs`: câmera por parâmetros (`CAMERA_ITEM`), tremor, 29 subcomandos de `op2110` (15 com efeito, 14 só registram), `14007`/`14009` (clipe por nome e "terminou?"),
  `op6813` e as janelas `OpenInfoWin*`/`AddItemWin`. **Todos os 747 eventos chegam ao fim** (eram 723). Teste: `param_camera_follows_the_event_cfg`; medição: `event_coverage`.
- Medidas atuais (`tools/progress_metrics.py`): opcodes 123/1.056 (11,6 %; 82,7 % das chamadas), subcomandos 51/289 (17,6 %; 94,9 %), `code.bin` 24/11.987 (0,20 %), Rust ≈13,5 mil linhas.
- Pendente: o bytecode dos gatilhos de fase e de mapa (liga os capítulos 2–11), efeito visível de emoção/rotação/adereço, 24 subcomandos de evento, SFX (BCSAR), materiais.

## 5ª etapa: cutscene pelo script, prévia 3D da seleção, tutoriais (2026-11-07)
- A 1ª cutscene (`ev01_0005a/b`) agora roda pelo script real no host de eventos (`prologue.rs`); `cutscene.rs` ficou só com o leitor de câmera. Corrigidos o `11004` e as malhas rígidas (céu).
- `engine/src/menu3d.rs`: prévia 3D na tela de cima da seleção de personagem. Posição e escala ainda sem calibrar contra o jogo (a figura encosta na borda de baixo).
- Tutoriais `sys_info_win_11/13/20` entram no capítulo 1 e `OpenInfoWin` nos eventos mostra o texto do sistema.
- **Não resolvido**: tela de baixo do campo (HUD/minimapa não identificados; `watchmap_d00_en.xa` é a tela do Yo-kai Watch, não a do campo), animação do céu (`.imm2`/`.mtm2`), a rede e a gaiola na mão (`27006` só registra), "animação errada" ainda não reproduzida depois da correção do `11004`.
- Ferramentas novas de diagnóstico: `YW_EVENT=<evento> YW_STAGE=<mapa> cargo test event_snapshots -- --ignored` e `YW_SCENE=<id> cargo test scene_snapshot -- --ignored` (gravam quadros em `work/`).

## Auditoria menu → diário → personagem → nome → cutscene → quarto → rede (2026-11-07)
- **Céu da cutscene**: os planos `ma-fix_img` agora são desenhados primeiro, com mistura alfa e sem profundidade (atrás do cenário); antes saíam como manchas opacas por cima de tudo. Textura com filtro **bilinear** (`gfx3d::sample_bilinear`).
- **Nome (2070)**: ficava preta porque o script esperava para sempre uma janela `OpenInfoWinStr*` que o `MenuHost` não tratava; agora a família `OpenInfoWin*` é respondida como fechada (também nos menus). Teclado de baixo desenha completo; a tela de cima do nome segue vazia.
- **Quarto**: o prólogo roda o `ev01_0020a/b` REAL (antes uma versão à mão sem a rede). A rede (`r001000`) aparece no quarto, o protagonista a guarda e ela some em fade (o script não a deixa na mão; `27006` só registra). Osso da mão do protagonista: `r_wph1` (índice 10), achado por hash.
- **Pendente (visto nos quadros)**: tela do diário (2111) com texto cortado à direita, "Location" mostrando `0` e texto riscado pela borda da caixa (posição/alinhamento de texto são hipótese); retratos (`BustupAssign`) dos diálogos; tela de baixo do campo; fade de alfa (`SetAlphaAnim`) e emoções (`11030`).
- O `yw1.exe` precisa estar FECHADO para recompilar (`cargo build` falha com "acesso negado" se a janela estiver aberta).

## Texto dos menus (2026-11-07)
- `op5944(obj, 1)` = texto ancorado à direita (termina em `x`); `op5932(obj, r, g, b)` = cor do texto; `op5932(obj, n)` com um número NÃO é largura da caixa nos menus medidos (16 e 2). `op5950` e `op5943` seguem ignorados.
- Diário (2111): contadores à direita e data (relógio do PC) sem corte. "Location" fica em branco: a chave do nome do lugar do `map_config` (`MAP_INFO` último campo) não está em nenhuma tabela de texto e o padrão de nome não foi achado (34 mil variações testadas).
- Ainda deslocado: texto das linhas e caixas (desenhado ~7 px abaixo; "Nate" riscado pela linha azul).
