@echo off
rem Builds Monitor Input Switcher into target\release\ (or target\debug\ with --debug). Copy the
rem exe wherever you like and start it once.
setlocal

set "ROOT=%~dp0.."

if "%~1"=="" (
    set "PROFILE=release"
    set "FLAGS=--release"
) else if /i "%~1"=="--debug" (
    set "PROFILE=debug"
    set "FLAGS="
) else (
    echo Usage: %~nx0 [--debug]
    exit /b 2
)

where cargo >nul 2>&1
if errorlevel 1 (
    echo cargo not found: install Rust from https://rustup.rs
    exit /b 1
)

cargo build %FLAGS% --manifest-path "%ROOT%\Cargo.toml"
if errorlevel 1 exit /b 1

for %%I in ("%ROOT%") do set "ROOT=%%~fI"
echo Built %ROOT%\target\%PROFILE%\monitor-input-switcher.exe
echo Copy it wherever you like and start it once: it adds itself to the tray and starts with Windows.
