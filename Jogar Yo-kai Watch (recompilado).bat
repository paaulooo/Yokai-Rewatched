@echo off
rem Abre o Yo-kai Watch recompilado (código do próprio jogo traduzido para Rust + HLE do 3DS).
rem Gerado em work\recomp (não versionado). Se o .exe não existir, gera e compila primeiro (alguns minutos).
cd /d "%~dp0"
if not exist "work\recomp\target\release\yw1r.exe" (
  echo Gerando o codigo recompilado...
  pushd recomp
  cargo run --release || goto erro
  popd
  echo Compilando ^(pode levar varios minutos^)...
  pushd work\recomp
  cargo build --release -p yw1-recomp-app || goto erro
  popd
)
"work\recomp\target\release\yw1r.exe"
exit /b
:erro
echo Falhou. Veja as mensagens acima.
pause
