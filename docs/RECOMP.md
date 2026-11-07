# Recompilação estática do `code.bin` (ARM11 → Rust)

Arquitetura decidida em 2026-11-07. O jogo roda o **próprio código**, traduzido para Rust nativo, sem interpretador de
CPU. Os serviços do 3DS são HLE e o desenho é feito por um renderizador PICA próprio. O motor manual antigo
(`engine/src/main.rs`, executável `yw1`) fica congelado.

## Como rodar
```
cd recomp && cargo run --release            # gera work/recomp/ (≈5 s); --stats só mostra as contagens
cd work/recomp && cargo build --release -p yw1-recomp-app    # ≈2–8 min do zero; incremental depois
YW_HEADLESS_SECS=60 ./target/release/yw1r.exe                # roda sem janela e grava as telas em work/recomp_frames/
```
Variáveis de ambiente:

| Variável | Para que serve |
|---|---|
| `YW_GAME` | o `.3ds`; padrão `BASE/Yo-Kai Watch (USA).3ds`. O RomFS é lido direto dele. |
| `YW_CODEBIN` | o `code.bin`. |
| `YW_RECOMP` | a pasta gerada. |
| `YW_TRACE_SVC` | registra as syscalls. |
| `YW_TRACE_IPC` | registra os comandos de serviço. |
| `YW_PICA_DEBUG` | estatísticas por draw. |
| `YW_RENDERER` | `soft` (padrão), `vulkan` (GPU; cai para `soft` se não inicializar) ou `none` (não desenha). |
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
- **Código gerado.** Uma função Rust por função guest, com a assinatura
  `fn f_XXXXXXXX(c: &mut Cpu, pc: u32, ret: u32)` e um `loop { match pc { bloco => … } }`.
  - `ret` é o endereço de retorno explícito. Um retorno só sai da função se o destino for `ret`; senão continua no
    bloco local, o que trata os `adr lr` + salto do assembly.
  - Chamadas diretas usam o símbolo. Chamadas indiretas passam por `yw1_dispatch` (tabela endereço → função).
  - O código sai em 24 crates `game_NN` que compilam em paralelo e se ligam por `#[no_mangle]`, mais o crate
    `dispatch` e o executável `app`.

Medidas (2026-11-07, `work/recomp/stats.txt`):
- 18.376 funções, 1,31 milhão de instruções traduzidas e 177 mil blocos;
- 408 tabelas de salto e 3 regiões densas;
- 4 codificações não suportadas, todas fora dos caminhos executados (dados e instruções do VFPv3);
- build release completo em ≈2–8 min (16 threads).

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
  - O shader de vértice continua na CPU. `render::prepare` recorta, projeta (12.4) e descarta por orientação igual
    ao soft; o GPU rasteriza e roda um ubershader de fragmento (`render/shaders/pica.frag`: TEV de 6 estágios,
    buffer de combinação, alpha test, wrap/borda/bilinear com texelFetch, profundidade quantizada como inteiro).
  - Blending, logic op, máscaras, depth/stencil e scissor viram estado do pipeline (cache por estado).
  - A memória guest é a fonte da verdade: render targets são carregados quando a memória mudou e voltam para ela
    (blocos 8×8) no fim de cada lista com draws. Texturas são decodificadas na CPU e ficam em cache por conteúdo.
  - Os shaders são compilados pelo `engine/build.rs` com o `glslangValidator`; cópia pré-gerada em `render/spv/`.
  - Validação: `cargo test --release --lib -- --ignored pica_frame_vulkan --nocapture`. No quadro do quarto:
    100% dos pixels iguais ao soft (±8; 17,6% bit a bit, quase todo o resto com diferença 1), 98,9% iguais ao
    Azahar, ≈30 ms por quadro contra ≈460 ms do soft (RX 5500 XT).
- **Próximos passos.** Otimizar o Vulkan (shader de vértice/conversões), DX12 como alternativa, iluminação por
  fragmento. O `soft` fica como referência.

## Marcos atingidos
1. O boot completo roda o código real até o laço principal, com 17 threads guest.
2. O jogo desenha a tela LOADING, o **vídeo do logo da Level-5** (decodificador Mobiclip do próprio jogo, em assembly
   recompilado) e a **abertura** ("That day in the forest…", com legenda na tela de baixo). Execução sem janela, por
   60 s, sem cair.
