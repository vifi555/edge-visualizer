@echo off
cd /d "%~dp0"
title Edge Visualizer (Rust) build
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
where cargo >nul 2>nul
if not errorlevel 1 goto build
echo  Rust is not installed. Installing it with winget...
winget install -e --id Rustlang.Rustup
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
rustup default stable

:build
echo.
echo  Building Edge Visualizer. The first build downloads libraries and takes a few minutes...
echo.
taskkill /f /im "Edge Visualizer.exe" >nul 2>nul
cargo build --release > build.log 2>&1
if errorlevel 1 goto fail
copy /y "target\release\edge-visualizer.exe" "Edge Visualizer.exe" >nul
echo.
echo  Done! "Edge Visualizer.exe" is in this folder. That single file is the whole app.
start "" "Edge Visualizer.exe"
timeout /t 4 /nobreak >nul
exit /b

:fail
echo.
echo  Build failed. The details are in build.log - send it to Claude.
pause
exit /b
