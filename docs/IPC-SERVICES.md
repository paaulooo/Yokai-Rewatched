# Serviços do 3DS e IPC

* IPC: `cabeçalho = (cmd_id<<16) | (normal_params<<6) | translate_params`; buffer de comando em `TLS+0x80`; `svc 0x32` = `SendSyncRequest`.
  Só há `svc` em **modo ARM** (nenhum Thumb). Obtenção de handle de serviço: wrapper `FUN_00310770` (`srv:GetServiceHandle`, header `0x50100`).
  Para ver nome e handle no GDB: breakpoints em `0x3107a8` (nome = `(char*)($r4+4)`) e `0x3107b8` (handle = `$r0`).
* Inventário: `data/ipc/ipc_commands.csv` (276 funções: func, header, cmd_id, nº de parâmetros). `ipc_scan.py` gera isso a partir do `.c` do Ghidra.
* Handles globais → serviço (`data/ipc/handle2svc.json`): `fs:USER` 005c5d7c · `dsp::DSP` 005c60ec · `hid:USER` 005c6198 · `cam:u` 005c7db4 · `ndm:u` 005d4118 · `y2r:u` 005d48d4 ·
  `ldr:ro` 005d7534 · `ac:u` 005d78e0 · `cfg:u` 005d791c · `ptm:u` 005da5d8 · `cecd:u` 005daffc · `cfg:i` 005db004 · `cfg:s` 005db008.
  (Outros serviços vistos em runtime: `APT:U`, `gsp::Gpu`, `boss:U`.)
* Mapa função → serviço (`data/ipc/ipc_by_service.csv`, 64 funções): y2r 17, APT:U 9, fs:USER 8, gsp::Gpu 5, dsp::DSP 5, boss:U 4, cfg:u 2, ndm:u 2, cecd:u 2, hid 1, ptm 1 e alguns handles não resolvidos.
  `*_orphans.csv` = sites `svc 0x32` atingidos que não caem em nenhuma função do CSV (distância > 0x100).
* **Nomes de comando confirmados** (Azahar → depurador IPC, `data/ipc/ipc_names_confirmed.csv`): `cfg:u` GetConfig `00116bcc`; `y2r:u` SetTransferEndInterrupt `00121ed4`, SetRotation `0023a86c`,
  SetSendingU `0023a8a8`, SetSendingV `0023a904`, SetSendingY `0023a960`, SetInputLines `0023aa18`, SetInputFormat `0023aab0`, SetOutputFormat `0023ab1c`, StartConversion `0023ab58`,
  IsBusyConversion `0023ab88`, SetBlockAlignment `0023abc4`, SetInputLineWidth `0023ac00`, SetStandardCoefficient `0023ac3c`, SetAlpha `0023ac78`; `gsp::Gpu` TriggerCmdReqQueue `0013d60c`;
  `dsp::DSP` FlushDataCache `00141160`; `fs:USER` OpenFile `0022f694`. O jogo abre `rom:/yw1_a.fa` em `FUN_00334198`.
* Lacunas: só 73 dos 276 sites `svc 0x32` foram executados nas capturas; ~200 funções sem serviço/nome. Para completar: mais sessões GDB (menus, batalha, salvar, câmera) ou
  filtrar o depurador IPC do Azahar por serviço (hid, APT, ptm, boss, ndm, cecd, gsp).
* Para o port (HLE): precisa implementar pelo menos APT (ciclo de vida/ aplicativo), FS (`rom:` → `yw1_a.fa`, save), GSP (substituído por renderizador próprio), HID, DSP (áudio), CFG, PTM, e stubs para y2r/cam/ndm/cecd/boss/ac.
* Scripts: `ipc_scan.py <code.bin.c> <csv>`, `svc_map.py <code.bin.c> <handle2svc.json>`, `svc_resolve.py <code.bin.c> <handle2svc.json> <ipc_commands.csv> <saida.csv>`,
  `parse_trace.py ipc_trace.txt ipc_commands.csv ipc_by_service.csv` (traço produzido por `gdb/ipc_trace.gdb`).
