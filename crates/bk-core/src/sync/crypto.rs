//! End-to-end encryption of sync snapshots.
//!
//! File: `CLBSYNC1 | salt (16) | nonce (24) | XChaCha20-Poly1305(gzip(json))`. The key is
//! Argon2id(passphrase, salt); the salt travels in the file so every device derives the same
//! key from the same passphrase. Only the derived key is stored on a device.

use std::io::{Read, Write};

use anyhow::{Context, Result, bail};
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};

const MAGIC: &[u8; 8] = b"CLBSYNC1";
pub const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;

/// A derived key and the salt it came from.
#[derive(Clone)]
pub struct Key {
    pub salt: [u8; SALT_LEN],
    key: [u8; 32],
}

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Key(..)")
    }
}

fn random<const N: usize>() -> Result<[u8; N]> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|e| anyhow::anyhow!("no system randomness: {e}"))?;
    Ok(b)
}

impl Key {
    /// Derive from a passphrase; `salt = None` makes a fresh one (the first device).
    pub fn derive(passphrase: &str, salt: Option<[u8; SALT_LEN]>) -> Result<Key> {
        if passphrase.chars().count() < 8 {
            bail!("use a passphrase of at least 8 characters");
        }
        let salt = match salt {
            Some(s) => s,
            None => random()?,
        };
        let mut key = [0u8; 32];
        argon2::Argon2::default()
            .hash_password_into(passphrase.as_bytes(), &salt, &mut key)
            .map_err(|e| anyhow::anyhow!("deriving the key: {e}"))?;
        Ok(Key { salt, key })
    }

    /// For storing in this device's sync config.
    pub fn to_base64(&self) -> (String, String) {
        let b = base64::engine::general_purpose::STANDARD;
        (b.encode(self.salt), b.encode(self.key))
    }

    pub fn from_base64(salt: &str, key: &str) -> Result<Key> {
        let b = base64::engine::general_purpose::STANDARD;
        let salt: [u8; SALT_LEN] =
            b.decode(salt)?.try_into().map_err(|_| anyhow::anyhow!("bad salt in the sync config"))?;
        let key: [u8; 32] = b.decode(key)?.try_into().map_err(|_| anyhow::anyhow!("bad key in the sync config"))?;
        Ok(Key { salt, key })
    }
}

/// The salt of an encrypted snapshot (to derive the key on a new device).
pub fn salt_of(file: &[u8]) -> Result<[u8; SALT_LEN]> {
    if file.len() < MAGIC.len() + SALT_LEN + NONCE_LEN || &file[..MAGIC.len()] != MAGIC {
        bail!("this isn't a Clibooker sync file");
    }
    Ok(file[MAGIC.len()..MAGIC.len() + SALT_LEN].try_into().expect("length checked"))
}

pub fn encrypt(key: &Key, plaintext: &[u8]) -> Result<Vec<u8>> {
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(plaintext)?;
    let packed = gz.finish()?;
    let nonce: [u8; NONCE_LEN] = random()?;
    let cipher = XChaCha20Poly1305::new((&key.key).into());
    let sealed =
        cipher.encrypt(&XNonce::from(nonce), packed.as_slice()).map_err(|_| anyhow::anyhow!("encryption failed"))?;
    let mut out = Vec::with_capacity(MAGIC.len() + SALT_LEN + NONCE_LEN + sealed.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&key.salt);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&sealed);
    Ok(out)
}

pub fn decrypt(key: &Key, file: &[u8]) -> Result<Vec<u8>> {
    let salt = salt_of(file)?;
    if salt != key.salt {
        bail!("the synced data was encrypted with a different passphrase; run `bk sync setup` again");
    }
    let body = &file[MAGIC.len() + SALT_LEN..];
    let (nonce, sealed) = body.split_at(NONCE_LEN);
    let nonce = XNonce::try_from(nonce).map_err(|_| anyhow::anyhow!("damaged sync file"))?;
    let cipher = XChaCha20Poly1305::new((&key.key).into());
    let packed = cipher
        .decrypt(&nonce, sealed)
        .map_err(|_| anyhow::anyhow!("wrong passphrase (or the synced data is damaged)"))?;
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(packed.as_slice()).read_to_end(&mut out).context("unpacking the synced data")?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_wrong_passphrase() {
        let k = Key::derive("correct horse battery", None).unwrap();
        let file = encrypt(&k, b"{\"hello\":\"world\"}").unwrap();
        assert_eq!(&file[..8], MAGIC);
        // Another device derives the same key from the passphrase and the file's salt.
        let k2 = Key::derive("correct horse battery", Some(salt_of(&file).unwrap())).unwrap();
        assert_eq!(decrypt(&k2, &file).unwrap(), b"{\"hello\":\"world\"}");
        let wrong = Key::derive("wrong horse battery", Some(salt_of(&file).unwrap())).unwrap();
        assert!(decrypt(&wrong, &file).unwrap_err().to_string().contains("wrong passphrase"));
        // Stored form round-trips.
        let (s, key) = k.to_base64();
        assert_eq!(decrypt(&Key::from_base64(&s, &key).unwrap(), &file).unwrap(), b"{\"hello\":\"world\"}");
        assert!(Key::derive("short", None).is_err());
        assert!(salt_of(b"not a sync file at all, definitely not").is_err());
    }
}
