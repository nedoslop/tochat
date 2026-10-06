@echo off
REM Release build script that statically links the MSVC CRT, remaps paths, and strips debug info.
REM Debug builds (plain `cargo build` or `cargo tauri dev`) are unaffected.
setlocal

set RUSTFLAGS=--remap-path-prefix =/ -C link-arg=/DEBUG:NONE
cargo tauri build %*
