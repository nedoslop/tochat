@echo off
REM Release build script that statically links the MSVC CRT.
REM Debug builds (plain `cargo build` or `cargo tauri dev`) are unaffected.
setlocal
set RUSTFLAGS=-C target-feature=+crt-static
cargo build --release %*