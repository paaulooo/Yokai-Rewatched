# yokai-port

Engenharia reversa do Yo-Kai Watch 1 (3DS, USA) para estudar o processo de um port nativo para PC.
É um projeto de aprendizado; os assets do jogo não fazem parte do repositório.

* **Para continuar o trabalho:** abra esta pasta no Claude Code (`claude` dentro de `yokai-port/`). Ele lê o `CLAUDE.md`
  sozinho. Depois peça: *"leia docs/STATUS.md e continue pelo próximo passo"*.
* **Gerar os assets locais:** `cd tools && pip install -r ../requirements.txt && python build_assets.py`
  (precisa de `../romfs/yw1_a.fa`).
* **Documentação:** `docs/STATUS.md` (visão geral e roadmap), `docs/FORMATS.md` (formatos de arquivo),
  `docs/GPU-PICA.md` (GPU/GX/PICA), `docs/IPC-SERVICES.md` (serviços do 3DS), `docs/ENVIRONMENT.md` (ferramentas e capturas).

Resultados já obtidos (imagens em `docs/img/`): frame reconstruído da VRAM, texturas do quadro, 1.233 texturas do jogo,
1.590 malhas 3D lidas direto do `.fa`.
