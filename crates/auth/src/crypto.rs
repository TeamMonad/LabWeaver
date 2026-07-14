//! AEAD key-ring support for state held outside the browser.

use std::collections::BTreeMap;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chacha20poly1305::{
    ChaCha20Poly1305, KeyInit,
    aead::{Aead, OsRng, rand_core::RngCore},
};

/// Versioned authenticated-encryption key material.
#[derive(Clone)]
pub struct KeyRing {
    active_key_id: String,
    keys: BTreeMap<String, [u8; 32]>,
}

impl KeyRing {
    /// Parses `key-id:base64url-32-byte-key` lines from a controlled secret source.
    pub fn parse(active_key_id: String, material: &str) -> Result<Self, CryptoError> {
        if active_key_id.trim().is_empty() {
            return Err(CryptoError::ActiveKeyMissing);
        }
        let mut keys = BTreeMap::new();
        for line in material
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            let (key_id, encoded) = line
                .split_once(':')
                .ok_or(CryptoError::InvalidKeyMaterial)?;
            if key_id.trim().is_empty() || keys.contains_key(key_id) {
                return Err(CryptoError::InvalidKeyMaterial);
            }
            let decoded = URL_SAFE_NO_PAD
                .decode(encoded)
                .map_err(|_| CryptoError::InvalidKeyMaterial)?;
            let key: [u8; 32] = decoded
                .try_into()
                .map_err(|_| CryptoError::InvalidKeyMaterial)?;
            keys.insert(key_id.to_owned(), key);
        }
        if keys.is_empty() || !keys.contains_key(&active_key_id) {
            return Err(CryptoError::ActiveKeyMissing);
        }
        Ok(Self {
            active_key_id,
            keys,
        })
    }

    /// Encrypts a secret payload with a random nonce and authenticated context.
    pub fn encrypt(&self, plaintext: &[u8], aad: &[u8]) -> Result<EncryptedValue, CryptoError> {
        let key = self
            .keys
            .get(&self.active_key_id)
            .ok_or(CryptoError::ActiveKeyMissing)?;
        let cipher = ChaCha20Poly1305::new(key.into());
        let mut nonce = [0_u8; 12];
        OsRng.fill_bytes(&mut nonce);
        let mut payload = nonce.to_vec();
        let ciphertext = cipher
            .encrypt(
                (&nonce).into(),
                chacha20poly1305::aead::Payload {
                    msg: plaintext,
                    aad,
                },
            )
            .map_err(|_| CryptoError::EncryptionFailed)?;
        payload.extend(ciphertext);
        Ok(EncryptedValue {
            key_id: self.active_key_id.clone(),
            payload,
        })
    }

    /// Decrypts and authenticates a value written by this key ring.
    pub fn decrypt(&self, value: &EncryptedValue, aad: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let key = self
            .keys
            .get(&value.key_id)
            .ok_or(CryptoError::UnknownKey)?;
        let (nonce, ciphertext) = value
            .payload
            .split_at_checked(12)
            .ok_or(CryptoError::InvalidCiphertext)?;
        let cipher = ChaCha20Poly1305::new(key.into());
        cipher
            .decrypt(
                nonce.into(),
                chacha20poly1305::aead::Payload {
                    msg: ciphertext,
                    aad,
                },
            )
            .map_err(|_| CryptoError::AuthenticationFailed)
    }
}

/// Ciphertext and its key identifier, suitable for separate database columns.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EncryptedValue {
    /// Key identifier used to encrypt the value.
    pub key_id: String,
    /// Twelve-byte nonce followed by AEAD ciphertext and tag.
    pub payload: Vec<u8>,
}

/// Encryption failures fail closed and must never be replaced with plaintext storage.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CryptoError {
    /// No active key is configured.
    #[error("LW_AUTH_KEYRING_ACTIVE_KEY_MISSING")]
    ActiveKeyMissing,
    /// Key source syntax or key length is unsafe.
    #[error("LW_AUTH_KEYRING_MATERIAL_INVALID")]
    InvalidKeyMaterial,
    /// A retired key needed to decrypt a live record is unavailable.
    #[error("LW_AUTH_KEYRING_KEY_UNKNOWN")]
    UnknownKey,
    /// Ciphertext was structurally malformed.
    #[error("LW_AUTH_KEYRING_CIPHERTEXT_INVALID")]
    InvalidCiphertext,
    /// Encryption failed.
    #[error("LW_AUTH_KEYRING_ENCRYPTION_FAILED")]
    EncryptionFailed,
    /// Ciphertext or associated data did not authenticate.
    #[error("LW_AUTH_KEYRING_AUTHENTICATION_FAILED")]
    AuthenticationFailed,
}

#[cfg(test)]
mod tests {
    use super::KeyRing;

    #[test]
    fn key_rotation_reads_retained_key_and_rejects_wrong_context()
    -> Result<(), Box<dyn std::error::Error>> {
        let old = "old:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let ring = KeyRing::parse("old".to_owned(), old)?;
        let value = ring.encrypt(b"secret", b"session:1")?;
        assert_eq!(ring.decrypt(&value, b"session:1")?, b"secret");
        assert!(ring.decrypt(&value, b"session:2").is_err());
        Ok(())
    }
}
