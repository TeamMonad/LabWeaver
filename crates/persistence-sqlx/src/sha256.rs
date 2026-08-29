#![allow(missing_docs)]
use std::fmt::{Display, Formatter};
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

/// Canonical lowercase SHA-256 digest. Private to persistence-sqlx after ARC-09
/// hash deletion from contracts; retained here only for migration/catalog
/// identity and idempotency payload hashing which remain infrastructure-internal.
#[derive(Clone, Copy, Debug, Eq, Hash, JsonSchema, Ord, PartialEq, PartialOrd)]
#[schemars(
    with = "String",
    extend("pattern" = "^[0-9a-f]{64}$", "minLength" = 64, "maxLength" = 64)
)]
pub struct Sha256Digest([u8; 32]);

impl Sha256Digest {
    #[must_use]
    pub fn of_bytes(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }

    pub fn of_canonical<T: Serialize>(value: &T) -> Result<Self, Sha256Error> {
        let bytes = serde_jcs::to_vec(value).map_err(|error| Sha256Error::CanonicalJson(error.to_string()))?;
        Ok(Self::of_bytes(&bytes))
    }
}

impl Display for Sha256Digest {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl FromStr for Sha256Digest {
    type Err = Sha256Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Sha256Error::InvalidSha256);
        }
        if value.bytes().any(|b| b.is_ascii_uppercase()) {
            return Err(Sha256Error::InvalidSha256);
        }
        let mut bytes = [0_u8; 32];
        for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
            let part = std::str::from_utf8(chunk).map_err(|_| Sha256Error::InvalidSha256)?;
            bytes[index] = u8::from_str_radix(part, 16).map_err(|_| Sha256Error::InvalidSha256)?;
        }
        Ok(Self(bytes))
    }
}

impl Serialize for Sha256Digest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Sha256Digest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Sha256Error {
    #[error("invalid lowercase SHA-256 digest")]
    InvalidSha256,
    #[error("canonical JSON serialization failed: {0}")]
    CanonicalJson(String),
}
