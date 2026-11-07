# Yo-kai Watch 1 (3DS) — port nativo para PC

Projeto de **aprendizado** de engenharia reversa do Yo-Kai Watch 1 (USA, title id `0004000000167700`), a partir do
CIA do próprio dono, com a meta final de rodar nativamente no PC (recompilação estática + HLE dos serviços do 3DS +
renderizador PICA200 próprio) e com suporte a mods. Nada aqui contém assets do jogo: eles são gerados localmente
a partir do RomFS do usuário (`work/`, ignorado pelo git).

Dono: Paulo (usuário "dreavy", São Paulo). **Responda sempre em português do Brasil.**

## Ambiente do usuário
- Windows 11; Azahar 2126.2 (emulador 3DS, tem GDB stub na porta 24689 e depurador PICA); MSYS2 UCRT64 com `gdb-multiarch`;
  PowerShell; ctrtool 1.1.0, makerom 0.19.0; Ghidra para o `code.bin`. Python 3 com `numpy` e `pillow`.
- Pasta base (BASE): `C:\Users\Admin\Documents\makerom-v0.19.0-win_x86_64\ctrtool-v1.1.0-win_x64`
  (em MSYS2: `/c/Users/Admin/Documents/makerom-v0.19.0-win_x86_64/ctrtool-v1.1.0-win_x64`).
  Este projeto fica em `BASE/yokai-port/`; o RomFS em `BASE/romfs/yw1_a.fa`; o dump de texturas do Azahar em `BASE/Dump Texturas/`.
- `PowerShell` e `MSYS2` são programas diferentes (já houve confusão): comandos `gdb-multiarch` rodam no **MSYS2 UCRT64**.

## Mapa do projeto
```
yokai-port/
  CLAUDE.md  README.md
  docs/        STATUS.md (comece aqui) · FORMATS.md · GPU-PICA.md · IPC-SERVICES.md · ENVIRONMENT.md · img/
  engine/      RUNTIME DO JOGO em Rust (crate `yw1`): janela 2 telas, boot (logo → abertura → menu), áudio BCSTM
  tools/       scripts Python de extração/análise (paths.py centraliza caminhos; sobrescreva com YW_* )
  gdb/         scripts GDB para o stub do Azahar (captura de IPC e de quadros)
  data/
    ipc/       inventário dos 276 sites SendSyncRequest, mapa função→serviço, nomes confirmados
    gpu/       shader_code.bin (441 palavras), shader_opdescs.bin, lista de registradores
    romfs/     yw1_a_files.csv (índice dos 20.835 arquivos do .fa)
    capture/   capturas de quadros (cap/ = 1 quadro consistente), cmdlists antigas
    ghidra/    code.bin e export .c do Ghidra (ver README lá)
  work/        TUDO que é gerado (fa_out, xi_png, model_out, texdump...) — não versionar
```

## Recompilação estática (rumo atual — docs/RECOMP.md)
```
cd recomp && cargo run --release                         # gera work/recomp/ a partir de data/ghidra/code.bin
cd work/recomp && cargo build --release -p yw1-recomp-app # executável yw1r (código do jogo + HLE + PICA)
YW_HEADLESS_SECS=60 work/recomp/target/release/yw1r.exe   # sem janela; telas em work/recomp_frames/
cd engine && cargo test --release --lib -- --ignored pica_frame --nocapture   # renderizador vs captura do Azahar
```
Código gerado = derivado do jogo → só em `work/`, nunca versionar. O motor manual abaixo está congelado.

## Runtime manual antigo (Rust, dentro de `engine/`, congelado)
```
cargo run --release [-- caminho/do/romfs]   # acha BASE/romfs/0004000000167700 sozinho; YW_ROMFS / YW_FFMPEG sobrescrevem
YW_START=title cargo run --release           # pula os vídeos e vai direto ao menu de título
YW_START=newgame cargo run --release         # New Game pelos scripts reais: slot → menino/menina → nome (Start) → quarto
X no campo = menu principal (B fecha) · nome digitado no teclado (Enter confirma) · Start pula a cutscene
F5 no campo = salvar no diário · saves em %APPDATA%/yw1/saves (YW_SAVES sobrescreve)
Hora do jogo = contador próprio do jogo (como no 3DS; não segue o relógio do PC)
YW_START=chapter1 / wild cargo run --release  # capítulo 1 do pé da escada / Mt. Wildwood livre (Y parado = lente)
Batalha: L/R gira o anel, ↑/↓ alvo, A Soultimate (gauge cheio), Y purificar, B fugir
YW_START=room cargo run --release            # novo jogo direto no prólogo → quarto 3D (t101i02); F1/F2 = resolução interna do 3D (salva em settings.ini)
cargo test --release                         # BCSTM vs ffmpeg, CRC32, layouts do título
cargo test --release -- --ignored            # gera snapshots/compara IMGC com tools/imgc.py (precisa de work/title/)
```
Controles (detalhes em `engine/src/input.rs`): teclado (setas/WASD, Z/Espaço = A, X/Backspace = B, Enter = Start,
Esc sai), mouse na tela de baixo = toque, gamepads Xbox/PS/Switch/genéricos via `gilrs` com "confirmar" no botão de
baixo (`YW_PAD_LAYOUT=nintendo` inverte para o layout do 3DS). Inspecionar o menu sem janela: os testes `--ignored`
renderizam pelo mesmo código do runtime (não usar captura de tela — pode pegar outras janelas do usuário).

## Comandos principais de extração (Python, rodar de dentro de `tools/`)
```
python build_assets.py            # .fa -> arquivos -> PNG -> OBJ (≈2 min, ≈1 GB em work/)
python fa_list.py [--extract]     # indice do yw1_a.fa (+ extrai)
python frame_sim.py               # simula 1 quadro capturado: 1 linha por draw (textura, nº de vértices)
python pica_decode.py LISTA.bin [--all]   # decodifica uma lista de comandos PICA
python xq32_disasm.py --fa seq/menu/TitleMenu.xq   # desmonta um script XQ32 direto do .fa
python imgc_validate.py           # (opcional) compara .xi com o dump do Azahar
```
Variáveis: `YW_BASE, YW_FA, YW_CAP, YW_WORK, YW_DUMPTEX, YW_VRAM` (ver `tools/paths.py`).

## Regras de trabalho
1. **Não adivinhe formatos**: leia a função equivalente no `code.bin` (`data/ghidra/code.bin.c`) ou valide contra dados reais
   (capturas em `data/capture/cap/`). Foi assim que o XPVB/XPVI foi decifrado. Marque o que é hipótese.
2. Funções do Ghidra são `FUN_<endereço virtual>`; VA = offset no arquivo + 0x100000 na região .text (ver docs/ENVIRONMENT.md).
3. Binários grandes não vão por anexo de chat (a conversão corrompe); use a pasta/zip.
4. Scripts devem rodar de qualquer pasta (use `paths.py`). Saídas geradas só em `work/`.
5. Conteúdo do jogo (texturas, malhas, `.fa`) é do usuário e **nunca** deve ser commitado/publicado.
6. **O jogo em si (runtime, renderizador, HLE, áudio/vídeo) é escrito em Rust** (`engine/`), por desempenho.
   Python é só para extração, análise e protótipos de validação — nada de runtime em Python.

## Estado e próximos passos
Leia `docs/STATUS.md`. Resumo: formatos de arquivos (ARC0, XPCK, IMGC, XMPR) e texturas/vértices do PICA estão decifrados;
falta o **vertex shader PICA → GLSL** (começar por um desassemblador de `data/gpu/shader_code.bin`), materiais (`.mtr`),
esqueleto/animações (`.mbn`, `.mtn2`), e o desenho do HLE de serviços.
