//! Compiles the client's Tauri-independent modules directly so their
//! in-file `#[cfg(test)]` modules run as part of this test crate.
#![allow(dead_code)]

#[path = "../../src-tauri/src/util.rs"]
mod util;
#[path = "../../src-tauri/src/crypto.rs"]
mod crypto;
#[path = "../../src-tauri/src/db.rs"]
mod db;
