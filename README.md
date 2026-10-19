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
Then run (set `__NV_DISABLE_EXPLICIT_SYNC`=`1` when using Linux + Wayland + NVIDIA):
```sh
cd client
cargo tauri dev
```
# Test android
Install Android Studio, Android NDK, JDK 17. Then add targets:
```sh
rustup target add aarch64-linux-android armv7-linux-androideabi i686-linux-android x86_64-linux-android
```
Then run:
```sh
cd client
cargo tauri android dev
```
