use std::fmt::{Display, Formatter};
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, UtcOffset};
use uuid::Uuid;

macro_rules! typed_id {
    ($name:ident) => {
        #[doc = concat!("Strongly typed UUIDv7 identifier for `", stringify!($name), "`.")]
        #[derive(Clone, Copy, Debug, Eq, Hash, JsonSchema, Ord, PartialEq, PartialOrd)]
        pub struct $name(#[schemars(with = "String")] Uuid);

        impl $name {
            /// Generates a time-ordered UUIDv7 identity.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            /// Returns the underlying UUID.
            #[must_use]
            pub const fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl Display for $name {
            fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
                Display::fmt(&self.0, formatter)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(value).map(Self)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.0.hyphenated().to_string())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                let uuid = Uuid::parse_str(&value).map_err(serde::de::Error::custom)?;
                if uuid.get_version_num() != 7 {
                    return Err(serde::de::Error::custom("public identifier must be UUIDv7"));
                }
                Ok(Self(uuid))
            }
        }
    };
}

typed_id!(AccessGrantId);
typed_id!(ActorId);
typed_id!(AgentRunId);
typed_id!(ApprovalId);
typed_id!(ArtifactId);
typed_id!(BuildRequestId);
typed_id!(BffSessionId);
typed_id!(CandidateId);
typed_id!(ConsoleCapabilityId);
typed_id!(ConsoleSessionId);
typed_id!(CourseId);
typed_id!(EndpointGrantId);
typed_id!(EndpointId);
typed_id!(EnvironmentId);
typed_id!(EvaluationReleaseId);
typed_id!(EvaluationRunId);
typed_id!(EvaluationStepRunId);
typed_id!(EventId);
typed_id!(FrozenSubmissionId);
typed_id!(GatewaySessionId);
typed_id!(ImageArtifactId);
typed_id!(LeaseId);
typed_id!(OperationId);
typed_id!(PolicyId);
typed_id!(ProblemPackageId);
typed_id!(ProjectId);
typed_id!(ReleaseId);
typed_id!(ResourceApprovalId);
typed_id!(ResourceRequestId);
typed_id!(CapacityClaimId);
typed_id!(SshPublicKeyId);
typed_id!(UploadSessionId);

/// Monotonic aggregate revision. Zero is never a persisted revision.
#[derive(Clone, Copy, Debug, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct Revision(u64);

impl Revision {
    /// Creates a non-zero revision.
    pub fn new(value: u64) -> Result<Self, FoundationError> {
        if value == 0 {
            return Err(FoundationError::ZeroRevision);
        }
        Ok(Self(value))
    }

    /// Returns the numeric revision.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for Revision {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(u64::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Aggregate-local ordering sequence. This value is not an SSE resume cursor.
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(transparent)]
pub struct Sequence(pub u64);

/// Monotonic cursor in a scoped public event stream.
///
/// The public wire representation is a canonical decimal string. This preserves every `u64`
/// value in JavaScript clients while retaining an efficient numeric representation internally.
/// A distinct wire type prevents an aggregate-local sequence from being accepted as a
/// course-stream resume position.
pub const STREAM_SEQUENCE_PATTERN: &str = concat!(
    r"^(0|[1-9][0-9]{0,18}|1[0-7][0-9]{18}|18[0-3][0-9]{17}|",
    r"184[0-3][0-9]{16}|1844[0-5][0-9]{15}|18446[0-6][0-9]{14}|",
    r"184467[0-3][0-9]{13}|1844674[0-3][0-9]{12}|184467440[0-6][0-9]{10}|",
    r"1844674407[0-2][0-9]{9}|18446744073[0-6][0-9]{8}|",
    r"1844674407370[0-8][0-9]{6}|18446744073709[0-4][0-9]{5}|",
    r"184467440737095[0-4][0-9]{4}|18446744073709550[0-9]{3}|",
    r"18446744073709551[0-5][0-9]{2}|1844674407370955160[0-9]|",
    r"1844674407370955161[0-5])$"
);
pub const STREAM_SEQUENCE_MAX_LENGTH: u32 = 20;

#[derive(Clone, Copy, Debug, Eq, Hash, JsonSchema, Ord, PartialEq, PartialOrd)]
#[schemars(
    with = "String",
    extend("pattern" = STREAM_SEQUENCE_PATTERN, "maxLength" = STREAM_SEQUENCE_MAX_LENGTH, "format" = "uint64-decimal")
)]
pub struct StreamSequence(pub u64);

impl StreamSequence {
    /// Returns the numeric stream position for persistence and ordering.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl Display for StreamSequence {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.0, formatter)
    }
}

impl FromStr for StreamSequence {
    type Err = FoundationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty()
            || (value.len() > 1 && value.starts_with('0'))
            || !value.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(FoundationError::InvalidStreamSequence);
        }
        value
            .parse::<u64>()
            .map(Self)
            .map_err(|_| FoundationError::InvalidStreamSequence)
    }
}

impl Serialize for StreamSequence {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for StreamSequence {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// Canonical lowercase SHA-256 digest.
#[derive(Clone, Copy, Debug, Eq, Hash, JsonSchema, Ord, PartialEq, PartialOrd)]
#[schemars(with = "String")]
pub struct Sha256Digest([u8; 32]);

impl Sha256Digest {
    /// Hashes raw bytes.
    #[must_use]
    pub fn of_bytes(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }

    /// Hashes an RFC 8785 canonical JSON representation.
    pub fn of_canonical<T: Serialize>(value: &T) -> Result<Self, FoundationError> {
        let bytes = serde_jcs::to_vec(value)
            .map_err(|error| FoundationError::CanonicalJson(error.to_string()))?;
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
    type Err = FoundationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(FoundationError::InvalidSha256);
        }
        if value.bytes().any(|byte| byte.is_ascii_uppercase()) {
            return Err(FoundationError::InvalidSha256);
        }
        let mut bytes = [0_u8; 32];
        for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
            let part = std::str::from_utf8(chunk).map_err(|_| FoundationError::InvalidSha256)?;
            bytes[index] =
                u8::from_str_radix(part, 16).map_err(|_| FoundationError::InvalidSha256)?;
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

/// UTC timestamp serialized with a literal `Z` and millisecond precision.
#[derive(Clone, Copy, Debug, Eq, JsonSchema, Ord, PartialEq, PartialOrd)]
#[schemars(with = "String")]
pub struct UtcTimestamp(OffsetDateTime);

impl UtcTimestamp {
    /// Creates an exact UTC millisecond timestamp without a string round trip.
    pub fn from_utc(value: OffsetDateTime) -> Result<Self, FoundationError> {
        if value.offset() != UtcOffset::UTC || value.nanosecond() % 1_000_000 != 0 {
            return Err(FoundationError::InvalidTimestamp);
        }
        Ok(Self(value))
    }

    /// Returns the timestamp value.
    #[must_use]
    pub const fn get(self) -> OffsetDateTime {
        self.0
    }
}

impl Display for UtcTimestamp {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
            self.0.year(),
            u8::from(self.0.month()),
            self.0.day(),
            self.0.hour(),
            self.0.minute(),
            self.0.second(),
            self.0.nanosecond() / 1_000_000
        )
    }
}

impl FromStr for UtcTimestamp {
    type Err = FoundationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let bytes = value.as_bytes();
        if bytes.len() != 24
            || bytes[19] != b'.'
            || bytes[23] != b'Z'
            || !bytes[20..23].iter().all(u8::is_ascii_digit)
        {
            return Err(FoundationError::InvalidTimestamp);
        }
        let parsed = OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
            .map_err(|_| FoundationError::InvalidTimestamp)?;
        if parsed.nanosecond() % 1_000_000 != 0 {
            return Err(FoundationError::InvalidTimestamp);
        }
        Ok(Self(parsed))
    }
}

impl Serialize for UtcTimestamp {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for UtcTimestamp {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// Immutable object-store identity without a machine-local path or credential.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArtifactRef {
    /// Stable metadata identity resolved by the owning service.
    pub artifact_id: ArtifactId,
    /// Explicit object-store binding from deployment configuration.
    pub store_binding: String,
    /// Immutable backend object version.
    pub object_version: String,
    /// Exact content digest.
    pub sha256: Sha256Digest,
    /// Raw object length.
    pub size_bytes: u64,
    /// Registered media type.
    pub media_type: String,
}

/// Frozen data-retention decision for an immutable resource.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RetentionSnapshot {
    /// Policy identity.
    pub policy_id: PolicyId,
    /// Exact policy revision used at creation.
    pub policy_revision: Revision,
    /// Stable retention class.
    pub class: RetentionClass,
    /// Absolute retention boundary.
    pub retain_until: UtcTimestamp,
    /// Required terminal disposition.
    pub disposition: RetentionDisposition,
}

/// Retention classes with distinct privacy and recovery requirements.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionClass {
    CourseMaterial,
    BuildEvidence,
    RunEvidence,
    StudentSubmission,
    SecurityAudit,
}

/// Required action after retention expires.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionDisposition {
    Delete,
    PurgeAfterExport,
    RetainSanitizedReceipt,
}

/// Strict safe path selector shared by packages, collectors, and LLM allowlists.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum PathRule {
    ExactFile { path: String },
    DirectoryTree { path: String },
}

impl PathRule {
    /// Returns the normalized relative path.
    #[must_use]
    pub fn path(&self) -> &str {
        match self {
            Self::ExactFile { path } | Self::DirectoryTree { path } => path,
        }
    }

    /// Validates portable path syntax.
    pub fn validate(&self) -> Result<(), FoundationError> {
        validate_relative_path(self.path())
    }
}

/// Validates a slash-separated, normalized, non-empty relative path.
pub fn validate_relative_path(path: &str) -> Result<(), FoundationError> {
    if path.is_empty()
        || path.len() > 1_024
        || path.starts_with('/')
        || path.starts_with('\\')
        || path.contains('\\')
        || path.contains('\0')
        || path.contains('*')
        || path.contains('?')
        || path.contains('[')
        || path.contains('{')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || (path.len() >= 2 && path.as_bytes()[1] == b':')
    {
        return Err(FoundationError::UnsafePath(path.to_owned()));
    }
    Ok(())
}

/// Parses a JSON boundary document while rejecting duplicate object keys before typed decoding.
///
/// Service adapters and event consumers must use this entry point instead of decoding directly
/// with `serde_json`, whose default map representation cannot report duplicate keys.
pub fn parse_strict_json<T: serde::de::DeserializeOwned>(
    input: &[u8],
) -> Result<T, FoundationError> {
    use serde::Deserialize as _;

    let mut deserializer = serde_json::Deserializer::from_slice(input);
    let value = StrictJsonValue::deserialize(&mut deserializer)
        .map_err(|error| FoundationError::InvalidJson(error.to_string()))?;
    deserializer
        .end()
        .map_err(|error| FoundationError::InvalidJson(error.to_string()))?;
    serde_json::from_value(value.0).map_err(|error| FoundationError::InvalidJson(error.to_string()))
}

struct StrictJsonValue(serde_json::Value);

impl<'de> serde::Deserialize<'de> for StrictJsonValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = StrictJsonValue;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("strict JSON without duplicate object keys")
            }
            fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Self::Value, E> {
                Ok(StrictJsonValue(value.into()))
            }
            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> {
                Ok(StrictJsonValue(value.into()))
            }
            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
                Ok(StrictJsonValue(value.into()))
            }
            fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Self::Value, E> {
                let number = serde_json::Number::from_f64(value)
                    .ok_or_else(|| E::custom("non-finite JSON number"))?;
                Ok(StrictJsonValue(serde_json::Value::Number(number)))
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(StrictJsonValue(value.into()))
            }
            fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
                Ok(StrictJsonValue(value.into()))
            }
            fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(StrictJsonValue(serde_json::Value::Null))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(StrictJsonValue(serde_json::Value::Null))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(value) = sequence.next_element::<StrictJsonValue>()? {
                    values.push(value.0);
                }
                Ok(StrictJsonValue(serde_json::Value::Array(values)))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(serde::de::Error::custom(format!(
                            "duplicate JSON object key: {key}"
                        )));
                    }
                    let value = map.next_value::<StrictJsonValue>()?;
                    values.insert(key, value.0);
                }
                Ok(StrictJsonValue(serde_json::Value::Object(values)))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

/// Shared scalar validation failure.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum FoundationError {
    #[error("revision must be non-zero")]
    ZeroRevision,
    #[error("stream sequence must be a canonical unsigned decimal string")]
    InvalidStreamSequence,
    #[error("invalid lowercase SHA-256 digest")]
    InvalidSha256,
    #[error("canonical JSON serialization failed: {0}")]
    CanonicalJson(String),
    #[error("timestamp must be UTC RFC3339 with millisecond precision")]
    InvalidTimestamp,
    #[error("unsafe normalized relative path: {0}")]
    UnsafePath(String),
    #[error("invalid strict JSON document: {0}")]
    InvalidJson(String),
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use serde_json::json;

    use super::{CourseId, Sha256Digest, UtcTimestamp, parse_strict_json, validate_relative_path};

    #[test]
    fn identifiers_round_trip_and_reject_uuid_v4() -> Result<(), Box<dyn std::error::Error>> {
        let id = CourseId::new();
        let json = serde_json::to_string(&id)?;
        assert_eq!(serde_json::from_str::<CourseId>(&json)?, id);
        assert!(
            serde_json::from_str::<CourseId>(r#""67e55044-10b1-426f-9247-bb680e5fe0c8""#).is_err()
        );
        Ok(())
    }

    #[test]
    fn canonical_hash_ignores_object_key_order() -> Result<(), Box<dyn std::error::Error>> {
        let left = json!({"b": 2, "a": 1});
        let right = json!({"a": 1, "b": 2});
        assert_eq!(
            Sha256Digest::of_canonical(&left)?,
            Sha256Digest::of_canonical(&right)?
        );
        Ok(())
    }

    #[test]
    fn strict_wire_scalars_reject_ambiguous_values() {
        assert!(Sha256Digest::from_str(&"A".repeat(64)).is_err());
        assert!(UtcTimestamp::from_str("2026-07-14T00:00:00.000Z").is_ok());
        assert!(UtcTimestamp::from_str("2026-07-14T00:00:00Z").is_err());
        assert!(UtcTimestamp::from_str("2026-07-14T00:00:00.0001Z").is_err());
        assert!(UtcTimestamp::from_str("2026-07-14T08:00:00+08:00").is_err());
        assert!(validate_relative_path("src/main.rs").is_ok());
        assert!(validate_relative_path("../secret").is_err());
        assert!(validate_relative_path("build/**").is_err());
    }

    #[test]
    fn strict_json_rejects_duplicates_unknown_fields_and_trailing_input() {
        #[derive(Debug, serde::Deserialize, PartialEq)]
        #[serde(deny_unknown_fields)]
        struct Input {
            value: u64,
        }

        assert!(matches!(
            parse_strict_json::<Input>(br#"{"value":1}"#),
            Ok(Input { value: 1 })
        ));
        assert!(parse_strict_json::<Input>(br#"{"value":1,"value":2}"#).is_err());
        assert!(parse_strict_json::<Input>(br#"{"value":1,"future":2}"#).is_err());
        assert!(parse_strict_json::<Input>(br#"{"value":1}{}"#).is_err());
    }
}
