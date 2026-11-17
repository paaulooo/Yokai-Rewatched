# Mods do jogo recompilado

Existem dois tipos de mod: **mod de arquivos**, que troca arquivos do `yw1_a.fa` pelo caminho, e **mod de código**, uma
DLL que registra ganchos em funções do jogo.

Os mods ficam em `%APPDATA%/yw1/mods/<pasta>/`; `YW_MODS_DIR` troca essa pasta e `YW_MODS=0` desliga todos.
Eles são gerenciados no menu (Esc/F1) → aba **Mods**:
- liga, desliga e muda a ordem de cada mod;
- liga o "código nativo".

Tudo vale ao reiniciar.

> Mods contêm conteúdo derivado do jogo. Nada disso entra neste repositório: aqui só ficam a API (`modapi/`), o exemplo
> de código (`mods-exemplo/`) e as ferramentas (`tools/mod_acessos.py`, `tools/mod_make_test.py`).

## Estrutura de um mod
```
mods/meu_mod/
  mod.toml
  romfs/data/res/text/menu/xxx_en.cfg.bin   ← troca o arquivo de mesmo caminho do .fa
  meu_mod.dll                               ← opcional (mod de código)
```
```toml
nome = "Meu mod"
versao = "1.0"
autor = "..."
prioridade = 0          # maior = vence, quando a ordem do menu não decide
descricao = "..."

[codigo]                # opcional
dll = "meu_mod.dll"     # dentro da pasta do mod
```

**Caminhos:**
- valem minúsculas, barras `/` e nenhum `rom:/` no início;
- `..` é recusado.

**Ordem:** quando dois mods trocam o mesmo arquivo, vence o que vem primeiro na ordem do menu. Os mods que não estão
nessa ordem vêm depois, pela `prioridade`.

## Mods LayeredFS (traduções e outros mods de Luma3DS/Azahar)

Uma pasta no formato do Luma3DS/Azahar (`<pasta>/romfs/...`, por exemplo `mods/0004000000167700/romfs/yw1_a.fa`)
funciona direto. Basta copiá-la para a pasta de mods.
- **Sem `mod.toml`:** o mod usa o nome da pasta. Um `mod.toml` só serve para dar nome, prioridade e descrição.
- **Arquivo inteiro da RomFS:** quando o caminho em `romfs/` existe na RomFS do jogo, o arquivo inteiro é trocado.
  Isso vale para `yw1_a.fa`, os vídeos `mov/*.moflex`, as músicas `snd/product/stream/*.bcstm` e outros.
  - O motor monta uma RomFS virtual (`engine/src/hle/services/romfs.rs`): só a tabela de arquivos é corrigida, e os
    arquivos do mod são lidos direto do disco. A imagem do `.3ds` não é copiada.
  - O log mostra `[mods] RomFS: <caminho> trocado (antes → depois bytes)`.
- **Arquivo de dentro do `.fa`:** os outros caminhos em `romfs/` são tratados como arquivos de dentro do `.fa`, como
  descrito abaixo.
- **`exefs/`:** não vale (o `code.bin` é recompilado). Para mudar código, use um mod de código.
- **Testado** com uma tradução PT-BR que troca `yw1_a.fa`, os 2 vídeos e as 2 músicas: o objetivo, o mapa e o nome
  da área saem em português.

## Mod de arquivos: o que dá para trocar
- **Como funciona:** o motor intercepta `FUN_0047359c(gerenciador, caminho, &buf, &tam, &achou, heap)`, a função que
  abre um arquivo do `.fa` pelo nome. Ele também intercepta a variante que descomprime (`FUN_0047365c`) e a consulta de
  existência (`FUN_00473728`).
- **Alocação:** o arquivo do mod é copiado para memória alocada pelo **próprio alocador do jogo** (`FUN_0030f4e0`, no
  mesmo heap e com o mesmo alinhamento da original).
- **Compressão:** na variante que descomprime, ponha no mod o arquivo **já descomprimido**.
- **Medido na rota até o campo** (`work/mods/acessos.csv`):
  - todas as 387 aberturas passaram por `0x47359c`: `.bin`, `.xc`, `.npcbin`, `.xk`, `.xa`, `.xq`, `.xr`, `.xi`, `.xf`;
  - a variante que descomprime e o streaming (`0x1dbaa8`) não foram chamados nessa rota.
- **Texturas:** a maioria está dentro de pacotes (`.xa`/`.xc`/`.xk`). Para trocar uma delas, troque o pacote inteiro.
- **Fora do alcance por enquanto:** os arquivos soltos da RomFS (`snd/`, `mov/`), que são lidos pelo serviço FS.

**Para começar:**
1. `python tools/mod_acessos.py` gera a lista de arquivos que o jogo abre numa rota.
2. `python tools/fa_list.py --extract` extrai o arquivo para editar.
3. `python tools/mod_make_test.py` mostra como montar um mod de teste: texto de menu e minimapa recolorido.

## Mod de código (API C, versão 1, `modapi/`)
A DLL exporta `yw1_mod_api_versao() -> u32` e `yw1_mod_init(api: *const Yw1Api, versao: u32) -> bool`.
- **Versão:** o motor confere a versão antes de chamar o init e recusa as incompatíveis.
- **Falha no init:** se o init devolve `false`, os ganchos que ele registrou são desfeitos.
- **Em Rust:** é mais simples usar o invólucro do crate `yw1-modapi`, com a macro `yw1_mod!(iniciar)` e os tipos `Api`
  e `Ctx`. Um pânico dentro de um callback é contido.

| Função | O que faz |
|---|---|
| `registrar_gancho(va, tipo, prioridade, nome, cb, user)` | Gancho na entrada da função `va`. `tipo`: `GANCHO_ANTES`, `GANCHO_SUBSTITUIR` ou `GANCHO_DEPOIS`. Devolve um id |
| `remover_gancho(id)` | Desfaz o gancho |
| `chamar_original(ctx)` | Dentro de um Substituir, executa a função original |
| `ler_reg` / `escrever_reg(ctx, i, v)` | Lê ou escreve os registradores r0..r15 |
| `ler_entrada(ctx, i)` | Num "depois", lê o r0..r3 da entrada |
| `ler_mem` / `escrever_mem(va, buf, len)` | Lê ou escreve a memória do jogo (confere se está mapeada) |
| `chamar_funcao(ctx, va, args, n)` | Chama uma função do jogo na mesma thread. Devolve `r0 \| r1 << 32` |
| `substituir_arquivo(caminho, dados, len)` | Troca um arquivo do `.fa` em tempo de execução |
| `log(nivel, msg)`, `vblank()` | Escreve no log do jogo; devolve o VBlank atual |

**Limites:**
- Os ganchos valem só na **entrada** de funções. Gancho no meio de uma função exige `recomp/hooks.toml` e regerar o código.
- Um Substituir é exclusivo por função. Com mods de arquivos ativos, o motor já ocupa o `0x47359c`.
- As DLLs não são descarregadas.

**Segurança:** a DLL é código nativo com acesso total à máquina. Por isso os mods de código vêm desligados por padrão
e o menu pede confirmação para ligá-los. Instale só mods de quem você confia.

O exemplo `mods-exemplo/registro_arquivos` registra cada arquivo aberto no log.
