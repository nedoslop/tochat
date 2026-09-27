@echo off
REM Release build script that statically links the MSVC CRT.
setlocal
set RUSTFLAGS=-C target-feature=+crt-static
cargo build --release %*