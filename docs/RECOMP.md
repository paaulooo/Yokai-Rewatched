# Recompilação estática do `code.bin` (ARM11 → Rust)

Arquitetura decidida em 2026-11-07. O jogo roda o **próprio código**, traduzido para Rust nativo, sem interpretador de
CPU. Os serviços do 3DS são HLE e o desenho é feito por um renderizador PICA próprio. O motor manual antigo
(`engine/src/main.rs`, executável `yw1`) fica congelado.

## Como rodar
```
cd recomp && cargo run --release            # gera work/recomp/ (≈8 s, com os shaders AOT); --stats só mostra as contagens
cd work/recomp && cargo build --release -p yw1-recomp-app    # ≈3,5 min do zero; só o motor mudou: ≈4–8 s
YW_HEADLESS_SECS=60 ./target/release/yw1r.exe                # roda sem janela e grava as telas em work/recomp_frames/
cd tools && python recomp_run.py NOME --secs 140 --input "24:a; +6:a; +3:touch(99,103)"   # rodada com roteiro
python frames_sheet.py --dir ../work/runs/NOME --step 3       # folha de contato · --at 125 = uma captura inteira
```
Para o Paulo jogar: `Jogar Yo-kai Watch (recompilado).bat` na raiz (compila se preciso e abre a janela).
Variáveis de ambiente:

| Variável | Para que serve |
|---|---|
| `YW_GAME` | o `.3ds`; padrão `BASE/Yo-Kai Watch (USA).3ds`. O RomFS é lido direto dele. |
| `YW_CODEBIN` | o `code.bin`. |
| `YW_RECOMP` | a pasta gerada. |
| `YW_TRACE_SVC` | registra as syscalls. |
| `YW_TRACE_IPC` | registra os comandos de serviço. |
| `YW_PICA_DEBUG` | estatísticas por draw. |
| `YW_RENDERER` | `vulkan` (padrão; cai para `soft` se não inicializar), `soft` (referência na CPU) ou `none`. |
| `YW_HEADLESS_SECS` | roda sem janela por N s (relógio do PC); `YW_SHOT_EVERY` = intervalo entre capturas (s do console, padrão 0,5); `YW_FRAMES_DIR` = pasta (padrão `work/recomp_frames`). Capturas `tNNNNN_sK_WxH.rgba`, NNNNN = instante do console em décimos. |
| `YW_INPUT` | roteiro de entrada (`hle/script.rs`): `TEMPO:AÇÃO[/DURAÇÃO]` separados por `;`, tempo em s do console ou `+N` relativo; ações `a b x y l r start select up down left right` (unidas por `+`) ou `touch(x,y)`. |
| `YW_GPU_TRACE` | registra cada combinação nova de parâmetros de DisplayTransfer. |
| `YW_SPEED` | só testes sem janela: acelera por igual o relógio do console (VBlank, áudio, tick, timeouts). O jogo não acompanha bem acima de ~1,5×. |
| `YW_GPU_SYNC=1` | executa os comandos do GX na thread do jogo (modo antigo, síncrono) em vez da thread de GPU. |
| `YW_VK_EAGER=1` | o Vulkan devolve os render targets para a memória no fim de cada lista (modo antigo) em vez de preguiçosamente. |
| `YW_VK_CPU_VS=1` | shader de vértice na CPU (caminho de reserva) em vez da GPU. |
| `YW_VK_NO_AOT=1` | desliga os shaders de vértice AOT (só o ubershader de vértice). |
| `YW_VK_AOT_SYNC=1` | compila os pipelines AOT na hora em vez de em segundo plano (o draw espera). |
| `YW_FA` | o `yw1_a.fa` que o gerador lê para os shaders AOT (padrão `BASE/romfs/yw1_a.fa`, senão o RomFS do `.3ds`). |
| `YW_SAVES` | pasta dos saves (padrão `%APPDATA%/yw1/recomp`); útil para rodadas paralelas isoladas. |

Janela (`hle/window.rs`, winit + egui desenhado com wgpu/Vulkan): Esc/F1 (ou Start+Select no controle) abre o menu
(Continuar · Configurações · Sair; Gráficos, Som, Controles, Controle, Emulação — no estilo do Zakuro), F11 tela cheia.
Configurações em `%APPDATA%/yw1/settings.toml`; com o menu aberto o console fica pausado (VBlank, DSP, tick e timers).
| `YW_VK_DEVICE` / `YW_VK_VALIDATION` | índice do GPU Vulkan / liga a camada de validação (se instalada). |
| `YW_GLSLANG` | caminho do `glslangValidator` usado pelo `engine/build.rs` (padrão: o do MSYS2 UCRT64). |
| `YW_VOLUME` | volume mestre do som, 0–100 (padrão 100). |
| `YW_AUDIO=none` | não abre o dispositivo de som (o DSP continua rodando). |
| `YW_DSP_STATS=0` | desliga a linha `[dsp]` por segundo (fontes tocando, pico, fila). |
| `YW_DSP_WAV` | grava a mixagem final num `.wav` (32728 Hz, estéreo). |
| `YW_DSP_TRACE` | registra cada mudança de configuração das fontes. |

O código gerado é derivado do jogo do usuário, então fica em `work/` e **nunca é versionado**.

## Gerador (`recomp/`)
- **Decodificadores.** `arm.rs` cobre ARMv6K e VFPv2; `thumb.rs` cobre Thumb, o que inclui o par BL/BLX. Instrução
  não suportada vira `rt::undefined` e só dispara se for executada.
- **Descoberta de funções.** Pontos de partida:
  - as funções `FUN_` do export do Ghidra;
  - o ponto de entrada 0x100000;
  - os alvos de BL;
  - as tabelas de construtores estáticos do RVCT (deslocamentos relativos);
  - literais carregados pelo código (`ldr rX, =f`, `ldr` seguido de `add pc`, ADR);
  - palavras de `.rodata` e `.data` e pools da `.text` que apontam para código.

  A busca se repete até estabilizar. Ela corrige entradas que o Ghidra marcou como ARM e na verdade são Thumb
  (137 casos), e acha as funções que não retornam: as cadeias que terminam em `svc 0x3C`.
- **Tabelas de salto.** O padrão do RVCT `addls pc, pc, rX, lsl #2`, mais as tabelas relativas escritas à mão
  (`sub rB, pc` + `ldr rA, [rB, rX]` + `add pc, rA, rB`).
- **Regiões densas.** O assembly escrito à mão com saltos computados (o decodificador de vídeo Mobiclip,
  0x14FFD0–0x162EBC, e mais dois trechos) é decodificado inteiro em sequência, e cada instrução vira bloco e entrada.
- **Saídas das regiões densas.** O assembly à mão também pula com `b` direto para o meio de outras funções
  (ex.: 0x269EE8 → 0x266BCC). Esses alvos viram entradas extras (e início de bloco) da função que contém a instrução;
  alvos fora do `.text` são dados lidos como instrução e são ignorados.
- **Código gerado.** Uma função Rust por função guest, com a assinatura
  `fn f_XXXXXXXX(c: &mut Cpu, pc: u32, ret: u32)` e um `loop { match pc { bloco => … } }`.
  - `ret` é o endereço de retorno explícito. Um retorno só sai da função se o destino for `ret`; senão continua no
    bloco local, o que trata os `adr lr` + salto do assembly.
  - Chamadas diretas usam o símbolo. Chamadas indiretas passam por `yw1_dispatch` (tabela endereço → função).
  - O código sai em 24 crates `game_NN` que compilam em paralelo e se ligam por `#[no_mangle]`, mais o crate
    `game_24` (só as regiões densas), o crate `dispatch` e o executável `app`.
  - **Funções gigantes são partidas** (`CHUNK_MAX` = 1024 blocos, pedaços de `CHUNK` = 384): cada pedaço é uma
    função `#[inline(never)]` com o próprio `loop/match`; um salto para fora do pedaço devolve o pc ao laço de cima, que
    escolhe o pedaço pela faixa de endereços (retorno = `DONE`). Motivo: o decodificador de vídeo (~19 mil blocos num
    único `match`) deixava o rustc superlinear — >80 min em opt-level 1, >25 min até em opt-level 0; partido, 19 s.
    **Regra: nunca resolver build lento baixando opt-level — partir o código.**
  - Preempção: `rt::poll(c)` na entrada de cada função e nos saltos para trás (menos nas regiões densas); quando
    `rt::PREEMPT` está ligado (uma interrupção acordou thread mais prioritária) o kernel troca de thread ali.

- **Shaders de vértice AOT** (`shaders.rs`, `pica_glsl.rs`, 08/11): ver Renderização. O gerador lê `fix.xr` do
  `yw1_a.fa` (`fa.rs` reaproveita `engine/src/assets/{l5,xpck}.rs` por `#[path]`; sem o `.fa` extraído, lê do RomFS
  do `.3ds`), traduz os 21 programas para `work/recomp/shaders/aot_NN.vert` + o registro `aot.txt`, e escreve o
  `build.rs` do executável (modelo em `recomp/src/app_build.rs`), que compila os `.vert` com o `glslangValidator` em
  paralelo (≈0,5 s) e embute o SPIR-V; o `main` gerado chama `yw1::render::register_aot`. Sem o jogo ou sem glslang,
  o executável fica só com o ubershader.

Medidas (2026-11-07, `work/recomp/stats.txt`):
- 18.376 funções, 1,31 milhão de instruções traduzidas e 177 mil blocos;
- 408 tabelas de salto e 3 regiões densas;
- 4 codificações não suportadas, todas fora dos caminhos executados (dados e instruções do VFPv3);
- build release completo em ≈3,5 min (16 threads; 2026-11-08, com as funções partidas).

## Runtime (`rt/`) e HLE (`engine/src/hle/`)
- **`rt`.** Define o `Cpu` (r0–r15, NZCVQ, GE, s0–s31, FPSCR, TLS) e a memória guest: uma reserva de 4 GB no host em
  que `va` corresponde a `base + va`. Implementa também a semântica de flags, shifts, SIMD de 8/16 bits, saturação e
  VFP, com testes unitários.
- **`hle::mem`.** Mapa de memória do 3DS antigo:
  - código em 0x100000, heap em 0x08000000, linear em 0x14000000 (PA 0x20000000);
  - VRAM em 0x1F000000;
  - páginas de configuração e compartilhada, TLS e RAM do DSP em 0x1FF00000.
- **`hle::kernel`.**
  - Implementa as 27 syscalls usadas.
  - As threads guest são threads do SO, mas só uma roda por vez, e a troca acontece nas syscalls bloqueantes (por
    prioridade).
  - Objetos: eventos, mutexes, semáforos, timers, árbitro de endereços, memória compartilhada e sessões.
- **Serviços (`hle::services`):**

  | Serviço | O que faz |
  |---|---|
  | `srv` | dá os handles dos outros serviços. |
  | APT | ciclo de vida: o Wakeup chega via ReceiveParameter. |
  | `fs` | a imagem RomFS (nível 3 do IVFC) vem do `.3ds`; SaveData e ExtSaveData ficam em `%APPDATA%/yw1/recomp`. |
  | `gsp` | fila de comandos, interrupções PSC/PDC/PPF/P3D, MemoryFill, DisplayTransfer e TextureCopy. |
  | `hid` | memória compartilhada de botões e toque. |
  | cfg, ptm | configuração do console e bateria. |
  | `y2r` | YUV → RGB. |
  | `dsp` | HLE de áudio como o do Citra: pipe de áudio, 24 fontes (PCM8/PCM16/ADPCM), mixers, saída cpal. |
  | cecd | StreetPass ocioso. |
  | demais | stubs registrados no log. |

- **Velocidade travada na normal.** O VBlank roda a 59,83 Hz, o tick do sistema a 268.111.856 Hz e o áudio a
  32728/160 Hz.
- **Diagnóstico.** O `c.func` guarda a função guest atual. O tratador de falha de acesso mostra o endereço guest e os
  registradores. No fim da execução sai o relatório das threads, com a espera de cada uma e os retornos prováveis na
  pilha.

## Renderização (`engine/src/pica/`)
- **Front-end.** Registradores com máscara de bytes, uploads de shader e de uniforms (f24/f32, na ordem w, z, y, x),
  atributos fixos, os 12 carregadores de vértice e a montagem strip/fan/list exatamente como no Citra.
- **`shader.rs`.** Executa na CPU o shader de vértice do PICA (backend de referência). Inclui a regra 0 × inf = 0,
  CALL/IF/LOOP/BREAK e o endereçamento relativo.
- **`raster.rs`.**
  - recorte nos 7 planos;
  - ponto fixo 12.4 com a regra de preenchimento do PICA;
  - interpolação com correção de perspectiva;
  - texturas: 14 formatos, Morton, wrap e bilinear;
  - TEV com 6 estágios e buffer de combinação;
  - alpha, stencil e depth test, blending e logic op;
  - escrita em blocos 8×8.
- **Falta** (marcado `[H]` no código): iluminação por fragmento, fog, procedural texture e geometry shader.
- **Validação.**
  - `cargo test --release --lib -- --ignored pica_frame --nocapture` (no diretório `engine/`).
  - O quadro real do quarto capturado no Azahar bate **98,8% dos pixels comparáveis** (tolerância ±8).
  - A captura não tem o estado anterior do GPU (DEPTHMAP_SCALE) nem os MemoryFills entre passadas; o teste
    reproduz os dois e marca como `[H]`.
- **Backend Vulkan (`engine/src/render/`, `YW_RENDERER=vulkan`).**
  - **Shader de vértice na GPU** (padrão desde 08/11; ideia do `shade.vert` do Zakuro): `pica/vs_gpu.rs` só traduz
    os registradores do draw — carregadores → (byte, passo, tipo, componentes) por registrador de entrada, atributos
    fixos/padrão, semânticas de saída, strip/fan → lista — e `render/shaders/pica_vs.vert` interpreta o programa do
    PICA na GPU com a semântica de `shader.rs` (0 × inf = 0, a0/aL, cc, CALL/IF/LOOP/BREAK, pilha de 16, limite de
    passos), lendo os arranjos crus de um storage buffer. Encaixe 12.4 no shader (arredondamento do soft), viewport do
    Vulkan = o do PICA, recorte pelo hardware (z −w..0 → 0..w, sem depth clamp), descarte por orientação no pipeline
    (modo 1 → BACK, 2/3 → FRONT com frente anti-horária), profundidade no pica.frag (`zoffset − zscale·gl_FragCoord.z`;
    com w-buffer, 0x6D bit 0 = 0, isso ÷ `gl_FragCoord.w` = (z/w·scale + offset)·w como no Citra/Zakuro — o mesmo no
    soft e no caminho da CPU; corrigido em 09/11, antes interpolava z·scale + offset linearmente na tela; o quadro do
    quarto não liga o w-buffer e o jogo avisa uma vez no log quando liga; na rota até o quarto nenhum draw ligou),
    plano do usuário (0x47–0x4B) em `gl_ClipDistance` [H: o soft não tem]. Teste `pica_frame_wbuffer`: o quadro do
    quarto com o w-buffer forçado (zscale −1/4096, zoffset 1/1024; `YW_WBUF_ZMAP` troca): Vulkan × soft com |Δ| ≤ 256
    (2⁻²⁴) em 98,9% (vértices na GPU) e 100% (CPU) — igual à precisão do z-buffer já aceito (98,6% / 100%); cores 100%;
    a fórmula antiga só batia em 1,8% (o fundo).
    Programas em cache por hash (64 vagas), set de descritores único com deslocamentos dinâmicos.
  - `YW_VK_CPU_VS=1` (reserva): shader de vértice na CPU; `render::prepare` recorta, projeta (12.4) e descarta por
    orientação igual ao soft. Nos dois caminhos o GPU rasteriza e roda um ubershader de fragmento (`render/shaders/pica.frag`: TEV de 6 estágios,
    buffer de combinação, alpha test, wrap/borda/bilinear com texelFetch, profundidade quantizada como inteiro).
  - Blending, logic op, máscaras, depth/stencil e scissor viram estado do pipeline (cache por estado).
  - A memória guest é a fonte da verdade: render targets são carregados quando a memória mudou e voltam para ela
    (blocos 8×8) no fim de cada lista com draws. Texturas são decodificadas na CPU e ficam em cache por conteúdo.
  - Os shaders são compilados pelo `engine/build.rs` com o `glslangValidator`; cópia pré-gerada em `render/spv/`.
  - Validação: `cargo test --release --lib -- --ignored pica_frame_vulkan --nocapture` (compara também os dois
    caminhos de vértice). No quadro do quarto (08/11, RX 5500 XT): vértices na GPU = soft 99,99% (±8; 17,4% bit a
    bit), = vértices na CPU 99,99% (±8; 99,0% bit a bit; o resto são pixels de borda), = Azahar 98,9% de 115 200
    comparáveis e 51,4% dos 1987 mudados (iguais ao caminho da CPU); 12,8 ms por quadro quente (vértices na CPU: 20 ms;
    soft ≈ 460 ms). A 1ª compilação do pipeline com o ubershader de vértice leva ≈ 1 s num driver novo; depois o cache
    do driver resolve (0,3 ms).
  - **Cache de pipelines próprio** (09/11): um `VkPipelineCache` usado pela thread principal e pela de compilação AOT,
    carregado no início (cabeçalho conferido: vendor, device, UUID) e gravado em `%APPDATA%/yw1/vk_pipelines.bin`
    (`YW_VK_PIPELINE_CACHE` = outro caminho, `0` desliga; nos testes `work/pica_test/`) no máximo a cada 10 s e no fim
    da rodada sem janela. Serve para drivers sem cache em disco; neste AMD o cache do driver já fazia o mesmo.
  - Os buffers do shader de vértice na GPU (≈ 73 MB mapeados) só são criados no 1º draw que os usa (nunca com
    `YW_VK_CPU_VS=1`). Avisos de compilação lenta (> 100 ms) têm cota própria (16) e não gastam a dos erros (20).
  - **Shaders de vértice AOT** (08/11; ideia do `glsl.rs` do Zakuro): os 21 programas do jogo (`fix.xr` →
    `SHD000..003.shdc`, DVLB com 3/7/5/6 DVLE) traduzidos pelo gerador (`recomp/src/pica_glsl.rs`). Cada shader AOT é
    o próprio `pica_vs.vert` com o trecho do interpretador (marcadores `@@AOT-BEGIN/END`) trocado pelo programa:
    `run()` com os campos de cada instrução embutidos (registradores, swizzles, máscaras, descritores), `loadInputs()`
    só com as entradas lidas e `outComp()` só com as saídas escritas — o resto (leitura dos atributos, auxiliares
    `pmul`/`dot3p`/`cond`/`cmpOp`, pilha, saídas, 12.4) é o mesmo texto. Formas: `straight` (sem fluxo, 8), `struct`
    (IFU/IFC e CALL bem aninhados → if/else, 13) e `dispatch` (qualquer outro fluxo: `switch` por trechos + a mesma
    pilha e o mesmo limite de passos do interpretador; nenhum programa do jogo precisa, mas o gerador escreve a
    variante dispatch dos 13 com fluxo para o teste).
    - Escolha no draw (`render/aot.rs` + `vulkan.rs`): o registro de cada AOT lista as palavras de código e os
      descritores de que a tradução depende (o que a execução alcança a partir da entrada); o AOT só é usado se elas
      forem iguais às carregadas no PICA (procura uma vez por (hash do programa, entrada)). Então AOT = ubershader por
      construção, e programa carregado que não bate com o `fix.xr` vai para o log uma vez ("programa de vértice sem
      AOT"). Na rota até o quarto nenhum apareceu: o jogo carrega exatamente os programas do `fix.xr`. O registro traz
      também o hash do `pica_vs.vert`: mudou o arquivo, o motor recusa os AOT até regerar.
    - Pipelines AOT compilam numa thread em segundo plano; até ficarem prontos o draw usa o ubershader (como os
      "ubershaders" do Dolphin) — num driver sem cache cada pipeline novo leva 0,1–1,2 s (medido: 130–1194 ms).
    - Validação (`pica_frame_vulkan`, quadro do quarto, 71 draws, todos AOT): AOT = ubershader **100% bit a bit**, a
      variante dispatch também (o teste confere que 0 draws caíram no ubershader e que 54 dos 71 usaram uma variante
      dispatch); = Azahar 98,9% de 115 200 (igual); listas em ≈ 8,4–9,1 ms (ubershader 11,9–13,0 ms).
    - Build do executável sem o glslang: aviso, só ubershader, e o `build.rs` gerado roda de novo quando o glslang
      aparece (caminho do MSYS2 vigiado, `PATH`); o motor avisa no log quando o executável não tem AOT.
- **Próximos passos.** Otimizar o Vulkan (conversões, fragment), DX12 como alternativa, iluminação por fragmento,
  pré-compilar em segundo plano os pipelines do ubershader dos estados comuns (hoje o 1º de cada estado ainda compila
  na hora). O `soft` fica como referência.

## Desempenho (meta: 30 quadros/s travados, como no 3DS)
Linha `[hle] Ns: X quadros/s do jogo · CPU do jogo ociosa · thread da GPU (% do relógio): lista PICA (shader · vk draw) ·
sincronismo · transfer · fill · alta resolução e interpolação`, a cada 5 s nas rodadas sem janela (`quadros/s` = trocas de
framebuffer da tela de cima).

Até 2026-11-10 a linha dizia "% do host: ocioso" e somava um "resto", o que estava errado. "Ociosa" é só a CPU do jogo; as outras
categorias rodam na thread "gpu", ao lado dela. A imagem em alta resolução e os intermediários não apareciam na linha.
- **DisplayTransfer em bloco** (2026-11-10, `gpu::transfer_buffered`):
  - uma cópia de cada lado em vez de um acesso à memória guest por byte de cada pixel;
  - idêntico byte a byte ao pixel a pixel (teste `display_transfer_igual_ao_pixel_a_pixel`, 1.500 casos);
  - "transfer" foi de 9–10 % para 7–9 %.
- **Telas pela GPU da janela** (2026-11-10, `fsr::Direta`):
  - sem FSR, a imagem vai uma vez para uma textura persistente;
  - o filtro (Nítido, Suave ou Misto = sharp bilinear) é feito no shader, sem clone, `ColorImage` nem `ampliar` na CPU;
  - medido na janela a 3×: de 1,4–2,9 ms para 0,13–0,18 ms por imagem (teste de GPU `tela_direta_filtros`).
- **Timestamps da GPU por lote** (2026-11-10): a linha de desempenho ganhou `vulkan: lotes/s, espera, GPU executando
  (nativo + sombra N×), convertendo`. Medido na rota (130–145 s, sem janela):

  | Escala | Nativo 1× | Sombra N× | Quadros/s |
  |---|---|---|---|
  | 1× | 134 ms/s (~4,5 ms/quadro) | — | 30 |
  | 2× | 134 ms/s | 405 ms/s | 30 |
  | 3× + aniso 16× | 85 ms/s | 480–530 ms/s | 27,6–29,4 (antes do item abaixo) |

  A sombra cresce bem menos que os pixels (4× → 9×): há um custo fixo grande por draw na GPU, mesmo a 1× (~4,5 ms
  por quadro para uma cena de 400×240), além do custo por pixel. A 3× a GPU fica no limite dos 33 ms. Em cena mais
  pesada o jogo cai para 18–24 quadros/s (log da janela do Paulo, 2026-11-10), alternando quadros de 33 e 50 ms — o
  "vai e volta" que se vê ao andar, com ou sem interpolação.
  - Alta resolução só para framebuffers que alguma tela mostra (`gpu::not_shown`). A cena vai também para 0x1F000000 a cada
    quadro, numa cópia que nenhuma tela mostra; antes ela ganhava FXAA, montagem e leitura a 3×. Com isso, 3× + aniso 16×
    passou para 29,8–30,0 no mesmo trecho.
  - TEV sem vetor local indexado (`tevSrc`): sem mudança mensurável (o compilador já tratava bem).
  - **O "custo fixo por draw" não existia** (2026-11-10, perfil por draw, abaixo). Na sombra 3× com aniso 16,
    o ajuste custo ≈ a + b·vértices + c·fragmentos dá 12 % fixo, 13 % vértice e **75 % fragmento** (~1 ns por
    fragmento no ubershader). Um quad de tela cheia com uma textura nearest custava 1,78 ms para 1,73 M fragmentos.
    Um draw minúsculo custa ~1 µs, e processar os vértices duas vezes custa ≤ 0,15 ms por quadro.
  - **Ferramentas.**
    - `YW_VK_DRAWPROF=arquivo.csv`: timestamps e pipeline statistics (invocações de vértice/fragmento) por draw.
      Análise: `tools/drawprof.py`.
    - `pica_frame_bench` (`cargo test --release --lib -- --ignored pica_frame_bench --nocapture`): o quarto capturado
      N vezes (`YW_BENCH_SCALE`, `YW_BENCH_ANISO`, `YW_BENCH_N`), com o tempo de GPU por quadro; ~15 s por medida. Também
      exige o fragmento especializado igual ao ubershader, bit a bit.
  - Isolamento no banco (quarto 3×, aniso 16, ubershader: 26,0 ms de GPU por quadro), com variantes descartáveis do
    pica.frag:

    | Variante | ms |
    |---|---|
    | sem `gl_FragDepth` | 25,5 |
    | TEV com 1 estágio | 17,3 |
    | sem TEV | 14,9 |
    | sem amostragem | 21,4 |
    | sem TEV e sem amostragem | 5,4 |

    O TEV genérico era o grosso. Tirar só a profundidade não muda: o jogo usa LESS com alpha test em ~80 % dos draws 3D,
    e o `discard` já impede o Z antecipado.
  - **Fragmento especializado** (`FsKey` em vulkan.rs; `YW_VK_FSPEC=0` desliga).
    - É o mesmo SPIR-V do pica.frag. O estado de controle (TEV, modos das texturas, flags, alpha test, origem da
      profundidade) vira constante de especialização, e o driver dobra tudo; valores continuam no uniform.
    - Compila em segundo plano (a thread dos AOT); até ficar pronto, o draw usa o ubershader.
    - O ubershader passou a parar no último estágio do TEV que não é passagem direta (`render::tev_active`).
    - Exatidão: especializado = ubershader, 0 bytes diferentes no quadro (1× com aniso, N× sem e com aniso).
      `pica_frame_vulkan` igual a antes: Vulkan = Azahar 98,9 %, AOT = ubershader 100 % exato.
  - **Sombra pulada no olho escondido** (`render::before_read_lcd`; `YW_VK_SKIP_HIDDEN=0` desliga).
    - A cena da tela de cima é desenhada duas vezes por quadro, uma por olho do 3D, com o slider em 0, e as duas
      ganhavam a sombra N×.
    - Quando o DisplayTransfer vai para um framebuffer que a janela não mostra (`top_hidden`/`not_shown`) e o lote só
      desenhou na sombra de alvos lidos por ele, o lote é enviado sem o buffer da sombra.
    - Essas sombras recarregam da memória no próximo uso, se a cena seguinte não começar com clear.
    - O nativo e a memória do jogo não mudam.
  - **Nativa sem aniso quando há sombra**: com o especializado pronto, a passada nativa usa a variante sem o filtro
    anisotrópico, que não existe no PICA. A memória do jogo fica igual ao hardware.
  - Resultado.
    - Banco (quarto 3×, aniso 16): 26,0 → **11,7 ms** de GPU por quadro (nativo 5,0 → 2,2; sombra 21,0 → 9,5).
    - Rota a 3× + aniso 16 + FXAA, 50–145 s: sombra 500–530 → **133–138 ms/s**; nativo 104–107 → **47–48 ms/s**;
      sincronismo 65–66 % → **23–26 %**; 29,8–30,2 quadros/s.
  - Próximos candidatos.
    - Por que o jogo desenha o olho direito com o slider 3D em 0 (página compartilhada +0x80 = 0,0): ler no code.bin.
      Se depender de algo que o HLE informa errado, a CPU e o nativo também cairiam pela metade.
    - Custo de parede de cada intermediário (~5,6 ms): espera síncrona e leitura de 3,5 MB em `present_image`, feitas no
      meio do quadro.
    - Buffers lidos pela GPU em DEVICE_LOCAL|HOST_VISIBLE.
    - Profundidade conservadora: exige a codificação +1 ((zi+1)·2⁻²⁴) e o viewport mapeado, porque o jogo usa LESS. Com
      exportação de Z e imagens em GENERAL, a AMD talvez só ganhe HiZ por bloco. No banco especializado, tirar
      profundidade e alpha test juntos economiza ~4,7 ms, mas tirar só a profundidade economiza ~1 ms.
- **GPU assíncrona** (`services/gsp.rs`): `TriggerCmdReqQueue` só copia os comandos (a lista PICA é copiada no
  despacho) e uma thread `gpu` do host os executa em ordem, disparando P3D/PPF/PSC/DMA ao terminar — como o hardware.
  Antes, renderizar dentro da thread do jogo fazia o quadro passar de 16,7 ms e a espera de VBlank virar 3 períodos
  (20 quadros/s). Draws cujos índices/vértices não estão mapeados são pulados com aviso (no console leriam lixo).
- **Devolução preguiçosa** (`render/vulkan.rs`): o lote Vulkan fica aberto entre listas; render targets só voltam para a
  memória guest quando DisplayTransfer/TextureCopy/MemoryFill/uma textura tocam a faixa (`before_read`/`before_write`);
  MemoryFill que cobre uma superfície inteira vira clear na GPU; conversão por bloco 8×8 em paralelo (`render/convert.rs`).
  [H] leitura de render target direto pela CPU do jogo não é detectada.
- **Shader de vértice na CPU em paralelo** (rayon) com deduplicação de índices.
- Medido (2026-11-08, RX 5500 XT): título/menus 30–57, quarto **30** (era 14), parque 17–23 (era 7; gargalo = shader de
  vértice na CPU, "shader" 40% do host).
- **Shader de vértice na GPU** (ubershader, ver Renderização): parque **29,8–30** (era 17–23), "shader" (agora só a
  tradução dos registradores na CPU) 0–1%, quarto 30. O custo passou para a GPU: "sincronismo GPU" (espera do lote)
  sobe para ≈ 60% no parque e ≈ 35% no quarto (eram 11% e 17%) — folga menor em cenas mais pesadas.
- **Shaders de vértice AOT** (ver Renderização; rodada `work/runs/aot1.log`, 08/11): 99,8% dos draws de vértice
  com AOT (o resto: draws enquanto o pipeline AOT compilava). Parque (95–155 s) **29,4–30**, "sincronismo GPU"
  19–27% (com o ubershader ≈ 60%); quarto (≥ 165 s) 30, sincronismo 21–32% (era ≈ 35%). Linha de 5 s ganhou
  `draws de vértice AOT x / ubershader y`; no fim sai o total e o tempo de compilação dos pipelines AOT.

## Marcos atingidos (rota até o capítulo 3 é a meta atual)
1. O boot completo roda o código real até o laço principal, com 17 threads guest.
2. O jogo desenha a tela LOADING, o **vídeo do logo da Level-5** (decodificador Mobiclip do próprio jogo, em assembly
   recompilado) e a **abertura** ("That day in the forest…", com legenda na tela de baixo). Execução sem janela, por
   60 s, sem cair.
3. (2026-11-08) Com roteiro de entrada sem janela: título → escolha de arquivo → gênero (prévia 3D) → teclado de nome
   (toque) → confirmação → avisos → céu/cidade → **cena do parque com diálogos** (Bear, Katie, Eddie), tudo pelo código
   real. Corrigidos no caminho: saídas das regiões densas (salto para 0x266BCC sem entrada) e a largura de saída do
   DisplayTransfer com redução 2×1 (listras verticais nas cenas 3D).
4. (2026-11-08) Roteiro com analógico: o Nate anda no quarto (Adams House – 2F, objetivo "Talk to Mom downstairs!").
   Quarto a 30 quadros/s com a GPU assíncrona. Janela nova com menu configurável.

## Teste visual em malha fechada (`tools/pilot.py`)
`python pilot.py NOME --secs 300` roda o jogo SEM janela e o guia olhando os quadros gravados: o roteiro fixo
(`work/runs/route_prefix.txt`) leva até o quarto e, daí em diante, o piloto lê o minimapa (seta do jogador = vermelho
escuro com ≥ 50 px; pino do objetivo = vermelho claro) e escreve a entrada em `work/runs/NOME.in` (`YW_INPUT_FILE`, uma
linha como `stick(0,1) a`, relida a cada 4 ms por `hle/script.rs`). Trilha em `NOME.pilot.txt`. Hoje: quarto → corredor →
escada → térreo → mãe (conversa). Entrada por tempo fixo (`YW_INPUT`) não serve para trechos longos: o tempo de
carregamento varia entre rodadas.

## Bugs de sincronização do write-back lazy achados por ele (corrigidos)
- **Hash de textura velho na mesma lista:** `tex_hash` guarda o hash por lista de comandos; quando a GPU devolve uma
  superfície à memória (`materialize`) as entradas que cobrem a faixa são invalidadas. Sintoma: balão e texto do diálogo
  da mãe translúcidos e vazios (só em modo lazy; `YW_VK_EAGER=1` não tinha o problema).
- **Faixas "escritas" maiores que a real:** `display_transfer` (com redução, saída ow/sx × oh/sy) e `texture_copy` (com
  intervalos entre linhas) declaravam ao renderizador mais memória do que escrevem → superfícies vizinhas pendentes eram
  descartadas em vez de devolvidas. Agora usam a área exata; com intervalo/parcial a faixa é devolvida, não descartada.
- **DMA do GSP sem ganchos:** `GxCmd::Dma` agora chama `before_read`/`before_write`.

## Pasta de logs (`work/logs/`) e vigia de travamento
Toda execução (janela ou sem janela) cria `work/logs/NNNN/` (ou `YW_LOG_DIR`; as ferramentas usam `work/logs/NOME/`) e
copia o stderr para lá sem tirar nada do console (`hle/logs.rs`): `game.log` (tudo) · `perf.log` (linha de 5 s) ·
`gfx.log` (`[vulkan]/[render]/[pica]/[gpu]`) · `dsp.log` (`[dsp]`, que sai do console; `YW_LOG_CONSOLE_ALL=1` mostra) ·
`input.log` · `stall.log` · `meta.txt` (modo, argumentos, variáveis `YW_*`). `work/logs/latest.txt` aponta para a última.
`YW_LOG=0` desliga. Leitura: `python tools/logs.py latest` (resumo: fps, travamento, avisos) ou `logs.py NNNN perf.log`.

O vigia (`hle/watchdog.rs`) roda em qualquer modo: 3 s sem quadro novo (fora da pausa do menu) → linhas `[vigia]` e, na
primeira vez, o relatório completo em `stall.log`: threads, registradores da thread em execução, memória que eles
apontam, análise de ciclo em listas ligadas (nó `+0x14` próximo, `+0x11` estado, `+0x12` sequência) e as últimas 120
mudanças do estado que o DSP devolveu ao jogo. `python tools/soak.py` repete o trecho título→quarto em paralelo para
reproduzir o travamento.

## Apresentação, latência e o que NÃO se aplica a este port (revisão do documento de otimização)
**Implementado** (menu Gráficos, `hle/window.rs`, `hle/settings.rs`; valem na hora, gravados em `settings.toml`):
- **Modo de apresentação**: V-Sync (FIFO) · Mailbox · Imediato. Os modos que a GPU oferece são consultados ao abrir a
  janela e logados (`[janela] modos de apresentação: …`); o que não existe cai no mais próximo (Mailbox → V-Sync,
  Imediato → o que o driver tiver sem V-Sync) e o menu avisa. **Nesta máquina (RX 5500 XT, Vulkan/wgpu): `Immediate`,
  `Fifo`, `FifoRelaxed` — sem Mailbox.** `YW_PRESENT=vsync|mailbox|imediato` manda sobre o arquivo (testes).
- **Quadros em voo (1–3)** = `desired_maximum_frame_latency` da superfície (1 = menor latência).
- **Teto de redesenhos** (desligados/60/120/144/240/sem limite) e **quadro novo do jogo apresentado na hora**:
  - `about_to_wait` acorda a cada 1 ms;
  - se `SWAPS` mudou, ou venceu o prazo de um passo da interpolação, redesenha sem esperar o tique do teto;
  - "desligados" = nenhum redesenho sem imagem nova (com o menu aberto, 60/s);
  - a entrada é lida a cada acordada (≈1 ms), não mais só no começo de cada redesenho.
- **Filtro "Nítido suave" (sharp bilinear)**: fator inteiro por vizinho mais próximo (`ampliar`) e bilinear no resto —
  bom para janelas de 1080p+ com escala fracionária.
- O log ganhou `[janela] N redesenhos/s · M quadros do jogo/s (modo)` a cada 5 s. Medido (título, ~60 quadros do jogo/s):
  ≈ 128 redesenhos/s em V-Sync e em Imediato, sem engasgo. A diferença de latência em si NÃO foi medida (precisaria
  de fotodiodo/câmera); só se verificou que os modos aplicam e a janela acompanha o jogo.

**Não implementado, de propósito:**
- **Desacoplar lógica e renderização com LERP/SLERP e buffer triplo de estado.** Aqui a "lógica" é o código ARM
  recompilado: as posições/rotações das entidades ficam em estruturas do próprio jogo, e a GPU só recebe listas de
  comandos PICA com matrizes já calculadas — não existe um `S_prev/S_curr` que o port possa ler e interpolar. O jogo gera
  imagem nova a 30 Hz na jogabilidade (60 em vídeos/menus) e isso só mudaria com geração de quadros por interpolação de
  imagem (fora do escopo). A velocidade do jogo já está travada no relógio do console (`console_now`, VBlank 59,83 Hz), e a
  comunicação jogo→janela já é sem bloqueio (contadores atômicos `SWAPS`/`BUTTONS`, sem `Mutex` no caminho do quadro).
- **Descritores bindless, push constants, multiview e dynamic rendering.** O gargalo não é a CPU do Vulkan: o backend
  já agrupa draws em lotes com um conjunto de descritores fixo (só muda o deslocamento) e o jogo roda a 30 quadros/s
  travados com ~25 % do tempo do host esperando a GPU. Sem ganho mensurável; reabrir se um profile mostrar o contrário.
- **FXAA/SMAA no passe de apresentação**: a imagem apresentada é um bitmap ampliado do 3DS (UI e texto incluídos); suavizar
  borra o texto sem tratar o serrilhado real, que está na renderização 3D. Anisotrópico também não se aplica: as texturas
  do PICA são amostradas à mão no shader (`texelFetch`, sem mipmaps) para igualar o hardware.
- **`VK_KHR_present_wait`**: o `wgpu` não expõe.
- **Resolução interna (1080p+ de verdade): marco próprio.** Exige alvos de render em N× e devolvê-los à resolução nativa
  em cada leitura do jogo (DisplayTransfer, texturas que vêm de alvos, MemoryFill, DMA), escala no viewport/scissor e no
  teste de exclusão por `gl_FragCoord`, e levar a imagem grande até a janela (outro dispositivo: wgpu). Projeto:
  manter o caminho 1× como referência (testes `pica_frame` e Azahar), comparar N× reduzido contra ele. Enquanto isso, o
  filtro "Nítido suave" melhora a ampliação. Segue como "em breve" no menu.

## Anisotrópico e antisserrilhado: como adaptar ao que o port tem
**Anisotrópico — implementado** (`pica.frag`, `sampleTex`; menu Gráficos; `YW_ANISO=0|2|4|8|16`; padrão desligado). As
texturas do PICA são amostradas à mão (uma amostra bilinear com `texelFetch`) e não têm mipmaps, então o filtro de
hardware não serve; em vez dele o shader mede a pegada do pixel em texels (`dFdx/dFdy` da coordenada) e, quando passa de
1 texel, cobre-a com uma grade de amostras bilineares (nx no eixo maior × ny no menor, no máximo 2ⁿ) e tira a média.
Pegada ≤ 1 texel ou opção desligada = o caminho de uma amostra de sempre (bit a bit). Medido: no quadro capturado do
quarto `pica_frame_aniso` — desligado é determinístico; 16× muda 9,8 % das palavras do alvo 3D (só 0,9 % por mais de 24
níveis: bordas de textura em superfícies inclinadas); na rota título→quarto o custo é invisível (30 quadros/s, espera da
GPU igual, ~29–35 %). Fica nos pixels de superfícies inclinadas/distantes: o ganho é maior em chão e paredes em
perspectiva (parque) que no quarto.

**Antisserrilhado — o que dá para fazer** (não implementado; decisão pendente):
1. *Onde aplicar:* nunca no bitmap final (borra o texto). O serrilhado está na cena 3D, dentro do backend Vulkan, entre
   o último draw 3D (profundidade ligada, shader de cena) e o primeiro draw de interface (profundidade desligada) do mesmo
   alvo. Ali dá para encerrar a passada, rodar um FXAA/SMAA de computação sobre a cor do alvo (cópia temporária →
   imagem, tudo já fica em layout GENERAL) e retomar a passada com LOAD. O resultado também vai para a memória do jogo
   (o `DisplayTransfer` seguinte copia o alvo já suavizado), a interface fica intacta.
2. *Limite honesto:* a 400×240 o FXAA tem pouca informação de subpixel — o resultado é mais borrão que antisserrilhado.
   O jogo já faz 2×1 de supersampling horizontal (alvo 480 → 240 no `DisplayTransfer`).
3. *Melhor resultado:* resolução interna N× (SSAA de verdade, descrita acima); aí FXAA/SMAA pós-cena passam a valer a
   pena. MSAA também é possível (alvo multiamostra + resolve), mas exige copiar o conteúdo 1× para as amostras toda vez
   que uma passada continua num alvo e manter a profundidade multiamostra — mais invasivo que o FXAA de cena.

## Resolução interna N×, FXAA de cena e FSR 1 (implementado)
**Resolução interna** (`engine/src/render/hires.rs`; menu Gráficos 1×–6×; `YW_SCALE`; só Vulkan). Os alvos NATIVOS (o que o
jogo lê de volta) ficam como são — é a referência dos testes e do Azahar. Cada alvo da tela de cima (cor 480×400 ou 240×400
RGBA8/RGB8 + a profundidade dele) ganha uma *sombra* N× que recebe os MESMOS draws (`draw` grava duas vezes: `cmd_main` e
`cmd_shadow`, viewport/scissor × N, push constant `n` para o scissor de exclusão do `pica.frag`; os pipelines são os mesmos).
Clears (MemoryFill) e uploads da memória reiniciam também a sombra (`ShadowSrc`; o upload decodifica e replica N×N). No
`DisplayTransfer` para o LCD, `present.comp` gira/inverte/reduz a sombra como o DisplayTransfer faz no nativo e publica a
imagem final (400N×240N) com o hash do framebuffer nativo; a janela só a usa se o hash ainda bate (`gpu::screen_hires`).
Mudar N/FXAA no meio do jogo executa o lote, devolve tudo à memória e recria as sombras (`hires_sync_config`).
- Validação (`pica_frame_hires`, quadro capturado + DisplayTransfer real do alvo 480×400, redução 2×1, RGB8, com e sem
  inversão): **N = 1 bate 100,00 % pixel a pixel com o framebuffer nativo** (orientação/redução exatas); N = 3: a média de
  cada bloco 3×3 fica a ±24 do nativo em 98,8 % dos pixels (diferença média 1,95). Caminho nativo (sem sombra) inalterado:
  98,9 % contra o Azahar, Vulkan = soft 99,99 %.
- Custo medido (RX 5500 XT, parque, `recomp_run`): nativo 30 quadros/s, espera da GPU 25–35 %; **3×: 27–30 quadros/s, ~60 %**;
  **5×: 22–28 quadros/s, ~70 %** (abaixo de 30). A tela de baixo não tem sombra (UI 2D; ganho pequeno).
- Limite conhecido: só o alvo de cor/profundidade da TELA; texturas renderizadas e lidas de volta pelo jogo (efeitos) seguem
  em 1×. Elegível = 400 linhas e largura 480/240, formato 0/1 (não-lossy).

**FXAA de cena** (`fxaa.comp`; menu; `YW_FXAA`): roda sobre a SOMBRA, entre o último draw com teste de profundidade e o primeiro
draw de interface (ou no `present` se não houve interface) — nunca no texto/HUD. Cópia da sombra → compute → de volta.
Medido: muda 6,6 % dos pixels (0,23 % por mais de 24 níveis). Visual: cena suave; texto e balão do diálogo sem borrão.

**FSR 1** (`hle/fsr.rs`; menu; `YW_FSR=1/0`): EASU + RCAS (algoritmo do FSR 1 da AMD, MIT) em WGSL, como `egui_wgpu::CallbackTrait`;
só entra quando a janela é maior que a imagem da tela. Testes (`fsr_easu_*`, GPU sem janela): 1:1 devolve a imagem
(diferença máx. 1); 2× de uma borda: transição em 2 pixels, sem halo. NÃO verificado: o aspecto na janela real (sem captura
de tela) — só o log (sem erros do wgpu, ~128 redesenhos/s).
**FSR 2/3 e FSR 4: não se aplicam.** FSR 2/3 (temporais; o 3 inclui geração de quadros) exigem vetores de movimento, jitter de
câmera e profundidade do motor do jogo; aqui as matrizes estão dentro do código recompilado. FSR 4 é um modelo de ML que só
roda em GPUs RDNA 4 pelo SDK fechado da AMD (esta máquina é RDNA 1).

## Fila de buffers de áudio: causa raiz do travamento/queda e correção (2026-11-09)
**Sintoma** (~6 % das execuções do trecho parque → quarto, mais com carga): `FUN_001420a8` girando para sempre (ciclo
`nó.próximo = nó`) ou, com a proteção `[guard]`, queda `FALHA de leitura 0x12` em `FUN_00141f40`.
**Causa:** o DSP HLE gerava um quadro a cada tick de 4,9 ms **sem esperar o jogo terminar de tratar o anterior**. O
`frame_counter` é do jogo (ele o incrementa ao publicar a resposta); se ele ainda não publicou, o HLE reescrevia o status
na MESMA região que a thread do jogo estava lendo. No hardware o tratador (`FUN_0030215c` → `FUN_00141f40` × 24 fontes)
leva < 1 ms com prioridade alta; aqui uma preempção/carga do host o alonga e a thread lia a fonte 0 de um quadro e a
fonte 1 do seguinte (status "rasgado": dirty de um, buffer atual de outro). A biblioteca de áudio então liberava e
reenfileirava um nó que ainda era a cabeça da fila da outra fonte (contagem 5 > tamanho da lista), daí o ciclo e a queda.
Evidência: o anel de rastreio mostrou todas as chamadas na mesma thread (TLS de t8) e a fonte 1 com `dirty 0` e buffer
atual já avançado junto da fonte 0 com `dirty 1`.
**Correção** (`dsp/mod.rs`, `guest_still_busy` + `generate_frame_ex`): contrapressão. Se a região de `frame_counter` mais novo
continua a do último quadro (o jogo não publicou resposta), o quadro gera e mixa o áudio normalmente (nada se perde) mas NÃO
escreve status/amostras/mixers na memória do jogo nem o interrompe; os pulsos `buffer_id_dirty` desses quadros ficam em
`pend_dirty` e saem no próximo quadro publicado (o tratador anda a fila até o buffer atual). Espera máx. `MAX_HELD_TICKS` = 8
ticks e depois publica mesmo assim (jogo parado).
**Prova (reprodução sob demanda):** `YW_AUDIO_JITTER=40` faz a thread do jogo dormir 2–9 ms no meio do tratador do status.
Sem a correção (`YW_DSP_NOHOLD=1`): 3 de 3 execuções com `[fila] INCONSISTENTE` (contagem 5 > lista 2–4) + `[guard]` + queda
`0x12`. Com a correção: 0 de 3. Sem jitter, 6 execuções em paralelo: 0 violações, 38 746 quadros de áudio gerados (taxa plena
de 204,5 Hz × 190 s); ~17 % dos quadros ficaram com a publicação adiada (carga de 3 instâncias), sem perda de áudio.
**Ferramentas deixadas:** `rt::trace`/`trace_dump` (anel das chamadas da fila, sondas em `TRACES` no gerador), o verificador
`rt::audio_check`/`audio_enqueue_check` (invariantes: sem ciclo, cabeça ≠ estado 3, contagem ≤ lista, sem reenfileirar nó
vivo) que imprime `[fila] INCONSISTENTE` + histórico, `YW_AUDIO_JITTER`, `YW_DSP_NOHOLD`. O `GUARDS` em `0x142118` segue como
rede de segurança. Diagnóstico de devolução (`[vulkan] CONFLITO …`) confirmou que NÃO era a causa (0 ocorrências).
Lição de processo: o comando em segundo plano engoliu um erro de compilação do gerador e o verificador ficou inativo em duas
rodadas — conferir `grep -c audio_check work/recomp/game_01/src/lib.rs` depois de regenerar.

## O jogo é por QUADRO, não por deltatime (medido; `YW_VBLANK_MULT`)
`YW_VBLANK_MULT=2` dobra a taxa de VBlank (relógio do sistema continua real; `vblank_hz()` converte a contagem de VBlanks em
segundos). Resultado: (1) introdução/menu sem entrada: a cena que aparece em 18 s aparece em 9 s, 24 s → 12 s, 30 s → 15 s —
tudo exatamente 2× mais rápido; (2) a rota título → nome → introdução → cutscene do parque → quarto → "Obtained Bug Net",
com todos os tempos de entrada pela metade, ficou em sincronia e terminou em ~97 s em vez de ~190 s, com 60 quadros/s
em jogo (30 normalmente). Logo o avanço da lógica (cutscenes, menus, jogabilidade) é por contagem de quadros: apresentar mais
quadros acelera o jogo na mesma proporção. Taxa de quadros maior com a mesma velocidade exigiria reescrever os incrementos por
quadro de cada sistema do jogo (ou interpolar imagem), não um ajuste de relógio. Mantida a velocidade travada.

## "Vai e volta" ao andar: troca de framebuffer antes da GPU (corrigido 2026-11-10)

**Sintoma.** Ao andar, a imagem parecia voltar um pouco e pular, com ou sem interpolação.

**Causa (medida).** A "GPU" é a thread `gpu`, que pode estar atrás do jogo. A troca de framebuffer é aplicada pela thread
`vblank`. Quando o VBlank chegava com o DisplayTransfer do quadro N ainda na fila para o buffer B, a tela trocava para B, que
ainda tinha o quadro N−2. Resultado: N−1 → N−2 → N.

Medido sem janela, a 3× com FXAA e aniso 16× (`YW_SWAP_EARLY=1` = comportamento antigo): 3–33 trocas assim a cada 5 s, de ~150.

**Correção (`gsp::update_framebuffer`).**
- Cada DisplayTransfer anota a saída como pendente ao entrar na fila e a tira ao terminar.
- Uma troca para um buffer ainda pendente fica para o VBlank seguinte. O quadro atrasa um VBlank, mas nunca volta.
- Os quadros/s não mudaram (29,6–30,2).

**Medidas permanentes na janela (linhas `[janela]` a cada 5 s):**
- "ritmo do jogo": VBlanks entre trocas da tela de cima e trocas adiadas;
- "ritmo da tela de cima": intervalos entre imagens novas, em faixas de 17/33/50 ms.

Ritmo medido com as opções do Paulo (V-Sync, 1 quadro em voo, 3×, interpolação 60): desvio de 0,7–3,6 ms. Quadros em voo
não eram a causa.

## Interpolação de quadros (menu Gráficos → "Interpolação de quadros"; `YW_INTERP=60|90|120|144|180`)

**Por que não "Delta Time Injection".** O jogo não tem Δt global. Medido: `YW_VBLANK_MULT=2` acelera tudo 2×, e 1/30 s aparece
em só 4 pontos do `code.bin`, para converter quadros em segundos. Trocar a constante não muda a velocidade.

**Como funciona.** Referências: RT64/Zelda 64 Recompiled e Ship of Harkinian; nada de misturar imagens.
- Cada draw da tela de cima é capturado: contexto PICA, 96 registradores float do shader de vértice, vértices, índices, programa.
- Entre os quadros reais N−1 e N, os draws são pareados por chave estrutural.
- A cena é re-renderizada pelo mesmo caminho (`render/interp.rs`), com os registradores interpolados.
- Matrizes rígidas (3 registradores com a parte 3×3 ortogonal, escala uniforme, sem espelho) são decompostas como no
  `gEXMatrixGroupDecomposed` do RT64: translação e escala por lerp, rotação por slerp de quatérnio.
- O resto é lerp. Chaves 0/1 e saltos não são interpolados.
- Sem pareamento seguro vale o quadro real: corte de cena, vídeo, menos de 70 % pareado ou mais de 20 % com salto.

**Qual framebuffer.** O que o LCD de cima mostra como olho esquerdo (`gpu::top_shown`, anotado na troca de framebuffer).
- Neste jogo é 0x1F300000/0x1F346500; 0x1F000000/0x1F046500 é o olho direito do 3D.
- Até 2026-11-10 o código interpolava o olho direito por engano. A interpolação **nunca chegava à tela** (teste com janela:
  0 grupos vistos) e só gastava a thread da GPU.
- Agora o olho escondido não monta imagem em alta resolução nenhuma (`top_hidden` → `present` retorna).

**Quantos intermediários.**
- Alvo (Hz) × período NOMINAL − 1, no máximo 5. O período nominal é o 10º percentil dos últimos 60 quadros, em VBlanks inteiros.
- Limite por ORÇAMENTO previsto: o tempo ocupado da thread "gpu" no último período (`gsp::GPU_BUSY_NS`, sem os intermediários)
  contra o período, com 15 % de folga.
- Rede de segurança: um quadro atrasado com intermediários reduz um. A volta ao orçamento acontece depois de 30 quadros no
  ritmo, e essa espera dobra a cada atraso, até 240.
- Antes, o número saía do período MEDIDO. A 3×, cada intermediário custava 5,7–8,6 ms, saíam ~5 por quadro e o período ia a
  51–54 ms (jogo a ~20 quadros/s). Esse é o "voltando" que o Paulo sentiu.

**Janela (`hle/window.rs`, `Cadencia`).**
- O relógio de cada grupo começa quando a janela o vê (a troca de framebuffer), não no DisplayTransfer. Medido: a troca chega
  8–19 ms depois, mais que um passo a 120 Hz. O esquema antigo pulava o 1º intermediário e deixava o real parado.
- Os passos são iguais (`período nominal / passos`) e o índice nunca volta.
- A janela redesenha no prazo de cada passo, qualquer que seja o teto de redesenhos.
- A interpolação é limitada à taxa do monitor (`aplicar_interp`; muda ao trocar de monitor).
- A troca de framebuffer (thread "vblank") pode chegar antes de a imagem em alta resolução estar publicada (thread "gpu"). Nesse
  caso a janela segura a imagem anterior por até 60 ms. A imagem nativa seria o quadro N antes dos intermediários de N−1→N.

**Medido** (RX 5500 XT; `tools/window_run.py` e rodadas sem janela, rota título → quarto):

| Cenário | Resultado |
|---|---|
| Janela, opções do Paulo (3×, FXAA, Imediato) + 120 Hz, monitor 180 Hz | 76–110 imagens interpoladas/s nas cenas leves; 0–3,5 % dos passos pulados; jogo em 29,8–30,0. "Segurou a anterior" 1–9× a cada 5 s em algumas cenas; "caiu na nativa" 0. |
| Quarto 3D a 3× com FXAA | A thread da GPU já fica ~90 % ocupada sem interpolação (linha de base 28,8–30,0 quadros/s). O orçamento não gera intermediários e o jogo fica igual à linha de base (antes das correções, 25,8–29,8). |
| Quarto 3D a 3×, depois do fragmento especializado e do olho escondido (2026-11-10) | Janela, opções do Paulo (3×, sem aniso, sem FXAA, Imediato, 120 Hz): 77–178 intermediários a cada 90 quadros (1–2 por quadro, 4,8–6,2 ms cada), 41–86 imagens interpoladas/s, 0–2,2 % dos passos pulados, jogo em 29,8–30,0. Sem janela com aniso 16 + FXAA: até 2 por quadro, mas a rede de segurança recua em rajadas (quadros com 1–2 intermediários chegando um VBlank atrasados). Os 120 Hz pedem 3 por quadro: o limite agora é o custo de parede de cada intermediário, não a GPU da cena. |
| Quarto a 1× | 1,6–2,8 intermediários por quadro (2,3–3 ms cada); jogo em 29,8–30,0. |
| Quarto a 2× | Intermediários em parte dos quadros (≈ 6 ms cada). Um intermediário conferido ao lado do quadro real ficou coerente: câmera no meio do movimento, sem fantasma. |

Qualidade (`YW_INTERP_VALIDATE=1`: prevê N a partir de N−1/N+1; diferença média por canal):
- Decomposição TRS contra elemento a elemento: 0,391 × 0,392. Muda a imagem em 30–51 de 60 previsões; ganho pequeno, nunca pior.
- Fidelidade α=1: 0,000.

**Diagnósticos que ficaram:**
- `YW_INTERP_STATS=1`: linha `[interp]` a cada 90 quadros (pareamento, intermediários, custo, período, GPU ocupada, teto).
- `YW_INTERP_VALIDATE=1`: a qualidade, como acima.
- `YW_SHOT_MIDS=1`: grava os intermediários em `work/runs/NOME/mids/` nas capturas sem janela.

**Para ter intermediários a 3× nas cenas pesadas.** (Até 2026-11-10; o fragmento especializado e o olho escondido
baixaram o sincronismo para 23–26 %, ver Desempenho.) O gargalo é o "sincronismo": 62–68 % do relógio na thread da GPU. O
DisplayTransfer na CPU espera a GPU terminar o quadro inteiro (nativo 1× + sombra N×) e devolve o alvo. Próximo passo medido:
timestamps da GPU no `flush` e DisplayTransfer na GPU, sem espera.

## 3 intermediários a 3×, texto da HUD e modo mono (2026-11-11, fase F1)

**Resultado (janela, opções do Paulo: 3×, sem aniso, sem FXAA, Imediato, 120 Hz; quarto, `window_run.py`).**

| | Antes | Depois |
|---|---|---|
| Imagens interpoladas/s | 41–86 | **120** |
| Intermediários por quadro | 1–2 | **3** (270 a cada 90 quadros) |
| Ritmo das imagens | — | 8,4 ms, desvio 0,9 ms |
| Passos pulados | até 2,2 % | 0 % |
| Jogo | 29,8–30 | 29,8–30 |

Sem janela, rota a 3×: thread da GPU ocupada 9 ms/quadro; sincronismo 16 %; 3,6 ms por intermediário. `soak.py --runs 9`
(3× + interpolação): 0 travamentos.

**Texto da HUD embaralhado nos intermediários.** Defeito antigo da interpolação, exposto quando passaram a sair
intermediários a 3×:
- O replay relia as texturas da memória do jogo no momento do replay. A thread "gpu" corre em paralelo com o jogo, que
  já podia estar reescrevendo a textura do texto do quadro seguinte.
- O replay também gravava no cache de hash de texturas (`tex_hash`) com o número da PRÓXIMA lista real. Isso contaminava
  a passada nativa do quadro seguinte, ou seja, a memória do jogo.

Correção:
- A captura guarda a `TexKey` (endereço, tamanho, formato, hash) de cada textura usada. O replay usa exatamente essa
  imagem da GPU, sem reler memória e sem tocar em `tex_hash`.
- As versões capturadas ficam pinadas contra o despejo (`tex_pinned`). Se faltar uma, o quadro não é interpolado.
- Uma escrita do GSP invalida `tex_hash` na faixa.
- Só entram os desenhos do alvo apresentado.
- Desenhos sem teste de profundidade (HUD) não interpolam os uniforms (`YW_INTERP_HUD=1` volta ao lerp).
- Teste `interp_nao_contamina_nativo`: com e sem replay no meio, a memória nativa sai 0 bytes diferente (a mudança das
  texturas altera 2,9 M bytes, então o teste prova).
- `YW_INTERP_STATS=1` mostra "texturas mudaram desde a captura" e "sem textura": 0 nas rodadas medidas.

**Modo mono (3D estéreo desligado; opção Gráficos → "3D estéreo", padrão desligada; `YW_STEREO=1` liga).**
- O jogo liga o estéreo por uma constante no boot: `FUN_00114588` faz `nngxSetDisplayMode(0x702)` quando o byte em
  `r0+0x14` não é zero. Slider e LED do HLE não mudam nada; com slider 0 os dois olhos saíam iguais.
- Uma sonda na TRACES (`recomp/src/main.rs`, 0x114588) zera esse byte quando `rt::stereo()` é falso. O jogo fica em 0x700
  e desenha uma passada só por quadro (menos CPU do jogo, tradução PICA e GPU nativa: nativo 63 → 43 ms/s com
  interpolação).
- Achado: no caminho mono, que nunca roda em console de varejo, o jogo transfere cada quadro da tela de cima para um de
  dois buffers (0x1F300000/0x1F346500, RGB8), mas NUNCA publica a troca. O bloco de atualização da tela de cima na
  memória compartilhada fica zerado, e o LCD só recebe a configuração do boot por registradores (endereço físico,
  formato 0x80340, sem stride).
  - Sem tratar isso, o contador de quadros ia a 0, a imagem nativa de reserva saía como lixo, e a cadência da janela e
    da interpolação (ancoradas na troca) quebravam.
  - O HLE sintetiza a troca: cada DisplayTransfer completo da tela de cima (240×400 depois da redução 2:1 do alvo
    480×400) vira a troca aplicada no VBlank seguinte, com o mesmo adiamento e contagem das trocas normais
    (`gpu::take_synth_top`).
  - Ainda não conferido: menus com efeito 3D (`FUN_00122b7c`). Abertura, título e quarto conferidos nas capturas.

**PPF adiantado (`YW_PPF_EARLY=0` volta à ordem antiga).**
- O DisplayTransfer foi dividido em parte nativa e parte "pós":
  - parte nativa: transfer, `present_hires_check`, que fecha o lote, confere a sombra e calcula o hash da saída;
  - parte "pós": `present_hires_finish`, que faz imagem N×, intermediários e publicação.
- O jogo recebe o PPF entre as duas. A troca de framebuffer continua esperando a publicação (`PENDING_OUT`).
- Toda troca da tela de cima sai 1 VBlank depois do pedido ("trocas adiadas" ~150 a cada 5 s com 3 intermediários),
  com ritmo regular.

**Memória e CPU (fáceis):**
- VBUF (32 MB) só com vértices da CPU;
- `snapshot` de cada alvo virou hash;
- pool de buffers para as imagens N× publicadas (antes ~300 MB/s de malloc com intermediários);
- `hires::expand` só escreve no destino;
- `mem::is_mapped` sem mutex (mapa de bits por página);
- `screen_hires` sem alocar.

**Ferramentas:**
- `window_run.py --input` agora funciona: antes o roteiro só rodava sem janela, e a janela sobrescrevia a entrada a
  cada ~1 ms (`script::ACTIVE`).
- `pica_ref`: referência bit a bit do renderizador (`YW_REF_MODE=save|cmp`, 24 imagens de cor e profundidade em 6
  configurações).
- `pica_frame_bench`.
- `tools/hud_diff.py`.

## Vulkan 1.4, renderização assíncrona e memória (2026-11-11, fases F2/F3)

**Vulkan 1.4 obrigatório.** Antes a instância pedia 1.1 e não usava nenhuma extensão. Sem 1.4 com `VK_KHR_push_descriptor`,
o motor cai no renderizador soft.

O que mudou, todo com equivalência bit a bit (`pica_ref`: 26 imagens de cor/profundidade em 6 configurações, 0 bytes;
`YW_REF_MODE=save` grava a referência):
- **Dynamic rendering:** sem VkRenderPass/VkFramebuffer nem os caches de framebuffer.
- **Push descriptors:** sem alocar e atualizar descriptor set por draw nem o flush forçado por pool cheio.
- **synchronization2:** as barreiras foram migradas.
- **Timeline semaphore** no lugar da fence.
- **`VK_EXT_memory_budget`** na linha de memória.
- **ReBAR:** buffers de upload em DEVICE_LOCAL|HOST_VISIBLE quando o heap tem ≥ 1 GB (senão só os ≤ 16 MB);
  readback continua HOST_CACHED. `YW_VK_REBAR=0` desliga.

Revisão adversarial (2 lentes + verificação de cada achado): 16 achados reais, 3 refutados.
- Corrigidos na hora:
  - FXAA gravava uma transição de layout num buffer de comandos que não estava gravando (bug antigo);
  - instância guardada pelo `mem_report` antes de um possível erro;
  - barreira de entrada no `cmd_pre` e no buffer do present;
  - limite do ReBAR sem heap grande.
- O resto virou a especificação da F3.

**Renderização assíncrona** (menu Gráficos → "Renderização", padrão Assíncrona; `YW_VK_ASYNC=0` = síncrona, como antes):
- Anel de lotes em voo (`YW_VK_SLOTS`, padrão 6; com 3 a vaga dava a volta dentro do quadro e a rede de segurança da
  interpolação recuava em rajadas).
- Anéis de upload com cauda por valor de timeline.
- Liberação adiada pela timeline.
- Readback esperando só o lote dono da faixa.
- Apresentação sem espera: anel de saídas; a thread "vk-publica" publica quando a GPU termina; a troca de framebuffer
  espera a publicação (`publish_pending`).
- Intermediários num envio só.
- Orçamento da interpolação por timestamps da GPU.

Medido (rota, 3× + interpolação):

| | Síncrono | Assíncrono |
|---|---|---|
| CPU por intermediário | 3,1 ms | 0,3 ms |
| GPU por intermediário | 2,7 ms | 2,7 ms |
| Tempo total da thread "gpu" por quadro (5 intermediários) | 26,7 ms | 24,4 ms |
| Experiência na janela | igual | igual |

Atenção ao comparar: a métrica "thread da GPU ocupada" da interpolação desconta o tempo de parede dos intermediários.
No modo assíncrono, a espera pela GPU deles aparece no quadro seguinte.

**Memória** (diagnóstico `YW_MEM_MAP=1`: mapa com VirtualQuery a cada 30 s + alocador contador; RAM privada):

| | Antes | Depois |
|---|---|---|
| Rota 3× + interpolação: RAM privada | ~473–490 MB | **~320–335 MB** |
| VRAM | 303 MB | **183 MB** |
| Buffers mapeados | 193 MB | **73 MB** |

- Os buffers mapeados contam inteiros na RAM privada, estejam na VRAM (ReBAR) ou não.
- Tamanhos pelos picos medidos:

  | Anel | Antes | Depois | Pico medido |
  |---|---|---|---|
  | ring | 96 MB | 32 MB | 16,5 MB |
  | vdbuf | 48 MB | 16 MB | 2,5 MB |
  | ibuf | 16 MB | 4 MB | 1,1 MB |
  | shbuf | 8 MB | 2 MB | 0,3 MB |
  | ubuf | 8 MB | 2 MB | KB |

  O ring cresce sob demanda (`grow_ring`), por exemplo para a sombra 6× carregada da memória, e nunca diminui. Picos
  no fim da rodada: linha "[hle] vulkan: anéis de upload".
- Commit sob demanda do espaço guest (o jogo só toca ~42 de 77 MB; `YW_MEM_LAZY=0` volta ao commit total; o VEH
  comita na falta de página).
- Espelho de registradores do GSP esparso: −4 MB.
- Pool de imagens publicadas 12 → 6.
- Cache de pipelines com hash dos SPIR-V no nome: os de versões antigas são apagados; antes crescia a cada mudança
  de shader, 24 MB para 45 pipelines vivos.
- Ainda grande: ~85–130 MB de memória mapeada interna do driver (hipótese: comandos/descritores) e ~46 MB do heap
  do driver.

**`window_run.py --input`**: só agora segue o roteiro (antes a janela não iniciava o roteiro e sobrescrevia a entrada).
Com as opções atuais do Paulo (3×, FXAA, mailbox, interpolação 180 Hz): **5 intermediários por quadro, 150 imagens
interpoladas/s**, jogo em 30. Os ~16 % de passos pulados vêm do limite de redesenhos de 120/s no settings, abaixo dos
180 Hz pedidos.

## Profundidade rápida na sombra N× e nos intermediários (2026-11-12, fase F4)

Decisão do Paulo: a profundidade é exata na passada nativa (memória do jogo, bit a bit) e rápida na sombra N× e nas
re-renderizações da interpolação.

**Como funciona.**
- A sombra e o replay usam o `pica_frag_fastz.spv` (pica.frag com `-DFAST_Z`), que não escreve `gl_FragDepth` nem
  declara DepthReplacing.
- O viewport mapeia a profundidade do PICA direto no rasterizador fixo: `min = zoffset·M·2⁻²⁴`,
  `max = (zoffset−zscale)·M·2⁻²⁴`.
- Condições: vértices na GPU, sem w-buffer, faixa em [0,1] e função LESS, GEQUAL, ALWAYS ou NEVER (exatas contra
  valores guardados quantizados). Fora disso, e enquanto o pipeline compila, usa o caminho exato.
- Diferença só em empates < 1 unidade de profundidade.
- `YW_VK_FASTZ=0` desliga.

**Medido.**
- Banco (quarto 3×): sombra 8,98 → 8,73 ms só com a profundidade rápida.
  - Com também a profundidade em DEPTH_STENCIL_ATTACHMENT_OPTIMAL durante a passada, 6,81 ms.
  - No JOGO a troca de layout piorou: sombra 328 ms/s sem ela contra 350 com ela, porque o jogo fecha e abre muito
    mais passadas da sombra por quadro e cada uma paga duas transições. Fica desligada (`YW_VK_DOPT=1` liga).
- Rota, mesma sessão: sombra 333 → 328 ms/s. Ganho pequeno: o alpha test (discard) em ~80 % dos draws 3D impede a
  escrita antecipada de profundidade.

**Exatidão.**
- `pica_ref`: nativo (cor e profundidade) 0 bytes; imagens da sombra 3 bytes em 3,4 M (o teste tolera 0,1 % só nos
  itens `*_hires`).
- `pica_frame_bench_interp`: intermediário = sombra real, 0 bytes.
- `pica_frame_fastz`: 62 de 71 draws pela profundidade rápida, 1 pixel diferente.

**Observação.** Os tempos de GPU absolutos variam entre sessões nesta máquina: o nativo foi de 29 para 48 ms/s com a
F4 desligada. Só comparar rodadas do mesmo momento.

**Janela.** A interpolação é limitada à taxa do monitor em que a janela abre: num 59 Hz sai 1 intermediário por
quadro, num 180 Hz saem 5.

## Janelas piscando nos intermediários (2026-11-12)

**Sintoma.** Com interpolação (mais visível a 120–180 Hz), os vidros das janelas das casas de Uptown piscavam. As
molduras e os vidros pareciam se desencontrar, e às vezes a janela sumia num intermediário.

**Causa (medida).**
- O pareamento marca como "pulo" o desenho em que algum uniform salta. Componentes em [−1, 1] não podem variar mais de
  0,8 num quadro.
- As casas têm uma linha de coordenada de textura do reflexo que acompanha a câmera e dá a volta. Exemplo: `c41.3` vai
  de −0,34 a 0,62, com xyz unitário e quase constante.
- Com isso a casa inteira ia para a posição do quadro novo já no 1º intermediário, enquanto o resto da cena (árvores,
  personagens e as peças vizinhas sem salto) interpolava.
- As camadas coplanares (parede × vidro × moldura) se desencontravam, e o teste de profundidade derrubava partes da
  janela.

**Correção.** Num desenho com salto, só as linhas de uniform que saltam (`jump_rows`) ficam com o valor do quadro novo.
A geometria interpola junto com a cena (`lerp_shading_snap`; um bloco rígido com linha fixa não é decomposto). O desenho
continua contando como salto no detector de corte de cena (`usable`).

**Prova.**
- A/B no mesmo grupo de quadros: o m2 com o comportamento antigo, m1/m3 com o novo. No antigo, a casa já está na posição
  do quadro seguinte e a janela grande some; m1 → m3 → real avançam juntos.
- Teste `salto_so_na_linha_que_salta`: a geometria do "vidro" sai igual à da "parede" sem salto.
- `interp_nao_contamina_nativo` e `pica_frame_bench_interp`: 0 bytes.
- Janela a 180 Hz: 177–180 imagens/s, 0–0,3 % de passos pulados.

`YW_INTERP_JUMP_DRAW=1` volta ao comportamento antigo.

**Diagnósticos novos.**
- `YW_SHOT_FROM=s`: primeira captura sem janela (para capturar todo quadro só num trecho). Com `YW_SHOT_EVERY` < 0,1 o
  nome usa centésimos de segundo.
- `YW_WIN_DUMP=pasta` (+ `YW_WIN_DUMP_FROM`/`_TO`): grava cada imagem nova da tela de cima exatamente como a janela a
  recebe, antes do FSR. Use caminho absoluto, porque o executável roda em outra pasta.
- Para reproduzir: `route_probe4.txt` até 205 s com `YW_SAVES` numa cópia (novo jogo → parque → rua das casas
  ~148–151 s), `YW_SHOT_MIDS=1`. Compare cada real com o intermediário anterior: região preenchida = defeito; só bordas
  = movimento.

## Ganchos no código do jogo (2026-11-12, fase G0)

Toda função recompilada aceita gancho na ENTRADA, ligado/desligado em tempo de execução, sem regerar:
- `antes` (pode mudar registradores), `substituir` (decide se chama a original) e `depois` (vê o retorno e uma cópia
  de r0..r3/sp/lr da entrada). API em `engine/src/hle/hooks.rs` (`registrar`, `remover`, `registrar_em`, `contagem`).
- Cabeçalho gerado, antes do `rt::poll` (nenhuma troca de thread entre o gancho e a original):
  `if pc == ENTRADA { if YW1_HOOKED[idx] != 0 && yw1_hook_entry(c, idx, ret) { return } } else if pc == ENTRADA|0x8000_0000 { pc = ENTRADA }`.
  "Chamar a original" entra com o bit 31 no pc: pula o gancho só naquela chamada (recursões voltam a passar por ele).
  `YW1_HOOKED`, a tabela idx → função e a lista de instruções instrumentadas ficam no crate `dispatch` (`yw1_hook_tables`).
- Entradas extras (salto para o meio de uma função, regiões densas) não passam pelo gancho de entrada: para elas, gancho
  em instrução. Esses são declarados em `recomp/hooks.toml` (`[[at]] va, nome, motivo`; exige regerar) e disparam ANTES
  da instrução, mesmo se ela for condicional.
- A sonda do modo mono (0x114588) virou um gancho `antes` essencial (`registrar_ganchos_essenciais`); a TRACES ficou só
  com as sondas de áudio. `YW_HOOKS=0` = só os essenciais; `YW_HOOKS_LOG=1` = contadores no fim.
- Custo medido (rota `route_probe4` sem janela, 1×): CPU do jogo ociosa 88,2 % → 89,0 % (ruído); executável
  54,8 → 56,1 MB (+2,4 %); compilação do zero 3 min 49 s; transferências iguais (mono preservado).

## Pré-compilação de pipelines (2026-11-12, fase G4)

- Toda pipeline criada entra numa lista com contagem de uso: `%APPDATA%/yw1/vk_pipekeys_<hash SPIR-V+AOT>.bin`
  (derivada dos shaders do jogo: nunca versionar). Formato próprio "YWPK" v1, campos explícitos, checksum; chave inválida
  ou tabela AOT diferente → descartada sem pânico. Gravada junto com o cache do driver (também ao fechar a janela).
- No boot, `vk-prewarm` compila a lista em segundo plano: primeiro as chaves que travariam o jogo (base, sem AOT/
  especialização), da mais usada para a menos. Pedidos do jogo passam na frente (`vk-pipelines` só atende o jogo);
  pedido síncrono de uma chave em compilação espera ELA, sem duplicar.
- Medido (mesma rota, cache do driver em arquivo novo): sem pré-compilação 6 compilações síncronas no jogo e 10 draws
  pelo ubershader; com ela 0 e 0 (45 de 45 pré-compiladas, 8 ms, 29 usadas). Aqui as compilações já eram rápidas
  (o driver da AMD tem cache próprio); o ganho grande é no 1º boot depois de atualizar o driver ou o jogo, que não foi
  medido (exigiria apagar o cache do driver).
- `tools/pipe_warm.py` enche a lista nas combinações de opções (1×, 3×+FXAA, 3×+interpolação, aniso 16).
- `YW_VK_PIPEKEYS=<arquivo>|0`, `YW_VK_PREWARM=0`. Resumo `[hle] vulkan: compilações síncronas no jogo …`.
