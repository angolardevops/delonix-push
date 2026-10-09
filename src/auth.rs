use axum::http::HeaderMap;
use rand::RngCore;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Segredo novo com prefixo (`dpk_` chave de servidor, `dpd_` segredo de aparelho).
pub fn new_secret(prefix: &str) -> String {
    let mut b = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut b);
    format!("{prefix}_{}", hex::encode(b))
}

pub fn hash(secret: &str) -> Vec<u8> {
    Sha256::digest(secret.as_bytes()).to_vec()
}

pub fn bearer(h: &HeaderMap) -> Option<&str> {
    let v = h.get("authorization")?.to_str().ok()?;
    v.strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

pub fn admin_ok(h: &HeaderMap, expected: &str) -> bool {
    if expected.is_empty() {
        return false;
    }
    match h.get("x-admin-token").and_then(|v| v.to_str().ok()) {
        Some(got) => got.as_bytes().ct_eq(expected.as_bytes()).into(),
        None => false,
    }
}
