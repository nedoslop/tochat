# Test server
```sh
cd server
cargo run
```
# Test client
First install `tauri-cli`:
```sh
cargo install tauri-cli --version "^2.0.0" --locked
```
Then run (set __NV_DISABLE_EXPLICIT_SYNC=1 when using Linux + Wayland + NVIDIA):
```sh
cd client
cargo tauri dev
```
