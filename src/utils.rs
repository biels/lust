use sha2::{Digest, Sha256};
use std::hash::Hash;

pub fn crc_hash<H: Hash>(v: H) -> u32 {
    let mut hasher = crc32fast::Hasher::default();
    v.hash(&mut hasher);
    hasher.finalize()
}

pub fn sha256_hash(v: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(v);
    hex::encode(hasher.finalize())
}
