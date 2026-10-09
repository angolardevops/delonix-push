//! Cifra em repouso das credenciais dos fornecedores (AES-256-GCM). O texto cifrado leva o *nonce* à frente e liga-se
//! ao projecto e ao fornecedor (dados associados): copiar uma linha para outro projecto não a torna legível.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use rand::RngCore;

pub fn seal(key: &[u8; 32], aad: &str, plain: &[u8]) -> Vec<u8> {
    let c = Aes256Gcm::new(key.into());
    let mut n = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut n);
    let ct = c
        .encrypt(
            Nonce::from_slice(&n),
            Payload {
                msg: plain,
                aad: aad.as_bytes(),
            },
        )
        .expect("AES-GCM não falha ao cifrar");
    let mut out = n.to_vec();
    out.extend(ct);
    out
}

pub fn open(key: &[u8; 32], aad: &str, sealed: &[u8]) -> Option<Vec<u8>> {
    if sealed.len() < 12 + 16 {
        return None;
    }
    let (n, ct) = sealed.split_at(12);
    Aes256Gcm::new(key.into())
        .decrypt(
            Nonce::from_slice(n),
            Payload {
                msg: ct,
                aad: aad.as_bytes(),
            },
        )
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_binding() {
        let k = [7u8; 32];
        let s = seal(&k, "projecto-a:fcm", b"segredo");
        assert_eq!(
            open(&k, "projecto-a:fcm", &s).as_deref(),
            Some(&b"segredo"[..])
        );
        assert!(
            open(&k, "projecto-b:fcm", &s).is_none(),
            "outro projecto não abre"
        );
        assert!(
            open(&[8u8; 32], "projecto-a:fcm", &s).is_none(),
            "outra chave não abre"
        );
        let mut t = s.clone();
        *t.last_mut().unwrap() ^= 1;
        assert!(
            open(&k, "projecto-a:fcm", &t).is_none(),
            "adulterado não abre"
        );
        assert_ne!(
            seal(&k, "x", b"a"),
            seal(&k, "x", b"a"),
            "nonce novo a cada vez"
        );
    }
}
