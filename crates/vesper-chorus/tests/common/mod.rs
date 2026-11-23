//! The Chorus test PKI (`tests/fixtures/pki`, see `gen.sh`): one CA and a
//! certificate per host `node{i}.chorus.test`, plus a rogue CA.

#![allow(dead_code)]

use std::time::Duration;
use vesper_chorus::warpnet::{HostCredential, WarpTls};
use vesper_warp::handshake::tls::rustls::pki_types::{
    CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, UnixTime,
};

pub const CA: &[u8] = include_bytes!("../fixtures/pki/chorus-ca.der");
pub const ROGUE_CA: &[u8] = include_bytes!("../fixtures/pki/rogue-ca.der");
pub const ROGUE_NODE1: (&[u8], &[u8]) = (
    include_bytes!("../fixtures/pki/rogue-node1.der"),
    include_bytes!("../fixtures/pki/rogue-node1.key.der"),
);

macro_rules! hosts {
    ($($i:literal),*) => {
        [$((
            include_bytes!(concat!("../fixtures/pki/node", $i, ".der")).as_slice(),
            include_bytes!(concat!("../fixtures/pki/node", $i, ".key.der")).as_slice(),
        )),*]
    };
}

pub const HOSTS: [(&[u8], &[u8]); 16] =
    hosts!(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15);

/// 2030-01-01, inside the fixtures' validity (2026-11-14 to 2126-10-21).
pub const NOW_SECS: u64 = 1_893_456_000;

pub fn credential(name: &str, cert: &[u8], key: &[u8]) -> HostCredential {
    HostCredential {
        name: name.to_owned(),
        chain: vec![CertificateDer::from(cert.to_vec())],
        key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.to_vec())),
    }
}

/// The honest PKI for 16 hosts at a fixed time.
pub fn pki() -> WarpTls {
    WarpTls {
        roots: vec![CertificateDer::from(CA.to_vec())],
        hosts: HOSTS
            .iter()
            .enumerate()
            .map(|(i, (c, k))| credential(&format!("node{i}.chorus.test"), c, k))
            .collect(),
        time: Some(UnixTime::since_unix_epoch(Duration::from_secs(NOW_SECS))),
    }
}
