//! The frozen Rust AEAD, X25519MLKEM768 KEM, and constant-time 16-byte tag
//! verification that the Vesper port replaced.
//!
//! `vesper-vault-seed` runs this engine directly (`cfg(not(vesper_port))`); the
//! port-backed `vesper-vault` compiles it only under `cfg(test)` as the oracle
//! for `src/differential.rs`. Bug fixes only.

#![allow(dead_code)]

pub mod aead;
pub mod kem;
pub mod secret;
