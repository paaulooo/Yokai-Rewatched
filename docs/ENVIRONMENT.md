# Ambiente, ferramentas e como capturar dados

## Arquivos do jogo
* Jogo: `Yo-Kai Watch (USA).cia` (e `.3ds`) na pasta BASE. Título `0004000000167700`.
* ExeFS: `exefs.bin` / `exefs_dir/` (extraídos com ctrtool; ver `ctrtool --help`); o `.code` do ExeFS está **descompactado** como `code.bin` (5.230.592 = 0x4FD000 bytes).
  O exheader também foi lido com ctrtool (segmentos abaixo).
* RomFS: `BASE/romfs/` (extraído pelo Azahar): `yw1_a.fa` (386 MB, todo o conteúdo), `mov/*.moflex` (vídeos), `snd/product/yw_sound_en.bcsar` + `snd/stream`.
  Há uma cópia em `romfs/0004000000167700/` (duplicata).

## `code.bin` no Ghidra
* Loader **Raw Binary**, linguagem `ARM:LE:32:v6` (ARM11, ARMv6K), base **0x00100000**. Compilador RVCT; stdlib Rogue Wave.
* Regra: **VA = offset no arquivo + 0x100000**. Segmentos do exheader (page-aligned) como blocos separados (o erro "Add Memory Block overlap" aconteceu ao
  tentar criar blocos sobre o bloco único inicial: apague o bloco único antes):
  `.text` file `0x000000` → VA `0x100000` (tam `0x3C0000`, R+X) · `.rodata` file `0x3C0000` → VA `0x4C0000` (R) · `.data` file `0x4BE000` → VA `0x5BE000` (RW, 0x3F000 B)
  · `.bss` VA `0x5FCCD4`, tam `0xA8408` (RW, não inicializado).
* Nomes `FUN_xxxxxxxx`/`DAT_xxxxxxxx` = endereços virtuais. O export `data/ghidra/code.bin.c` (≈20 MB, 656 mil linhas) é a fonte de tudo que está citado nos docs.
  Dica: `grep -n "FUN_00318bf0" code.bin.c`. Funções importantes estão em GPU-PICA.md, IPC-SERVICES.md e FORMATS.md.

## Azahar (emulador) — usado como "laboratório"
* Versão 2126.2. Nas configurações de **Depuração**: ligar o **GDB Stub** (porta **24689**). Menu **Exibir → Depuração**: depurador **PICA** (lista de comandos; "Copiar tudo"
  dá os nomes oficiais de registrador), **gravador de IPC** (mostra serviço/nome/parâmetros de cada requisição, filtrável por serviço).
* **CiTrace** (gravador `.ctf`) estava desabilitado/não salvou arquivo → não use; substituído por capturas com GDB.
* **Dump de texturas**: opção `DumpTextures` (Utilitários); gera `Dump Texturas/tex1_<LxA>_<hash>_<fmt>_mip0.png` (+ `pack.json`) com 1.973 arquivos nesta máquina.
  `<fmt>` segue a numeração do PICA (0 RGBA8 … 12 ETC1, 13 ETC1A4). Os PNGs são invertidos verticalmente (`flip_png_files: true`).
* O log do Azahar em nível Info não mostra IPC; nível Trace não saiu nesta build. Use o gravador de IPC.

## Capturas com GDB (rodar no **MSYS2 UCRT64**, não no PowerShell)
Pré-requisitos: Azahar com o jogo rodando e GDB Stub ligado; o jogo **numa cena estável** (ex.: o quarto do Nate).
```
cd /c/Users/Admin/Documents/makerom-v0.19.0-win_x86_64/ctrtool-v1.1.0-win_x64/yokai-port/data/capture
gdb-multiarch -x ../../gdb/dump_frame2.gdb        # -> ./cap/ (linear.bin, vram_start.bin, vram_end.bin, list_00..13.bin, index.txt)
```
* `gdb/dump_frame2.gdb`: ignora `SKIP`=20 listas, grava linear+VRAM, depois 14 listas (`WINDOW`) e a VRAM final; para ao terminar. Break em `0x138f44` (ProcessCommandList).
  Se vier sem draws, aumente `SKIP`. Hoje 1 quadro = `list_01..list_07` (setup 0x270 B … fim 0x50 B).
* `gdb/dump_frame.gdb` (antigo, uma lista + VRAM + linear), `gdb/capture_cmdlist.gdb` (grava até 300 listas, `cmdlist_NNNN.bin` + `cmdlists_index.txt`).
* `gdb/ipc_trace.gdb`: breakpoints em todos os `svc 0x32` (`svc32_addrs.txt`) + nome/handle de serviço; saída em `ipc_trace.txt` (gdb `set logging`). Depois:
  `python tools/parse_trace.py ipc_trace.txt data/ipc/ipc_commands.csv data/ipc/ipc_by_service.csv`.
* Avisos do GDB ("unknown osabi 3DS", "No executable has been specified") são normais.
* Binários grandes: trocar por pasta conectada/zip — anexos binários no chat chegam corrompidos.

## Ferramentas externas úteis
* Kuriimu2 (código-fonte) para ARC0/XPCK/IMGC/compressão L5: `plugins/Level5/plugin_level5` (clonar com `git clone --depth 1 --filter=blob:none --sparse ...`).
* 3dbrew.org (GPU, shader ISA, IPC), Azahar/Citra `video_core` como implementação de referência do PICA.
* Não encontrados publicamente: formato `XMPR` (`.prm`), `MTRC` (`.mtr`), `.mbn`, `.mtn2` — decifrar com `code.bin` + dados.

## Notas para a recompilação ARM (fase futura)
CPU alvo: ARM11 MPCore (ARMv6K) em **modo ARM** (nenhum site `svc` em Thumb). VFPv2. TLS em `TLS+0x80` para IPC. Syscalls via `svc` (0x32 = SendSyncRequest,
outros: ver 3dbrew "SVC"). O jogo é C++ com vtables ("(**(code **)(*param_1 + 8))(param_1)"); `.cro` (módulos) são carregados via `ldr:ro` — **ainda não extraídos do RomFS** (não vistos no `.fa`).

## REA (reverse-engineer-anything) com o code.bin
* Ghidra 12.1.4 em `yokai-port/ghidra_12.1.4_PUBLIC/` (fora do git). O registro MCP do REA em `~/.claude.json` recebeu
  `GHIDRA_INSTALL_DIR` (essa pasta) e `JAVA_HOME` (JDK 21); backup em `~/.claude.json.bak-rea`. Requer reiniciar o Claude Code.
* No Windows o REA só aceita PE: `python tools/codebin_pe.py` gera `work/code_arm.exe` (PE ARMNT 0x01C4 com os endereços reais).
  Validado: `rea decompile work/code_arm.exe 0x329bc8` = mesmo pseudocódigo do `code.bin.c`.
