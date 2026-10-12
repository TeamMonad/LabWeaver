//! Validate a portable managed version with the same types and digest as Control.

use std::io::{Read, Write};

use contracts::platform_configuration::PlatformConfigurationVersion;
use persistence_sqlx::Sha256Digest;

use crate::AppError;

const MAX_BYTES: u64 = 1_048_576;

fn invalid(code: &'static str) -> AppError {
    AppError::BundleConfiguration { code }
}

pub fn validate(payload: &[u8]) -> Result<PlatformConfigurationVersion, AppError> {
    if payload.is_empty() || payload.len() as u64 > MAX_BYTES {
        return Err(invalid("LW_BUNDLE_CONFIGURATION_LIMIT"));
    }
    let version: PlatformConfigurationVersion = contracts::parse_strict_json(payload)
        .map_err(|_| invalid("LW_BUNDLE_CONFIGURATION_INVALID"))?;
    version
        .configuration
        .validate()
        .map_err(|_| invalid("LW_BUNDLE_CONFIGURATION_INVALID"))?;
    if version.revision.get() > i64::MAX as u64 {
        return Err(invalid("LW_BUNDLE_CONFIGURATION_INVALID"));
    }
    let digest = Sha256Digest::of_canonical(&version.configuration)
        .map_err(|_| invalid("LW_BUNDLE_CONFIGURATION_INVALID"))?;
    if version.configuration_sha256 != digest.to_string() {
        return Err(invalid("LW_BUNDLE_CONFIGURATION_DIGEST_MISMATCH"));
    }
    Ok(version)
}

pub fn run() -> Result<(), AppError> {
    let mut payload = Vec::new();
    std::io::stdin()
        .take(MAX_BYTES + 1)
        .read_to_end(&mut payload)
        .map_err(|_| invalid("LW_BUNDLE_CONFIGURATION_IO_FAILED"))?;
    let version = validate(&payload)?;
    let mut output = std::io::stdout().lock();
    serde_json::to_writer(&mut output, &version)
        .map_err(|_| invalid("LW_BUNDLE_CONFIGURATION_IO_FAILED"))?;
    output
        .write_all(b"\n")
        .map_err(|_| invalid("LW_BUNDLE_CONFIGURATION_IO_FAILED"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use contracts::platform_configuration::PlatformConfigurationDesired;
    use contracts::{ActorId, Revision, UtcTimestamp};
    use serde_json::json;

    fn version() -> PlatformConfigurationVersion {
        let configuration = PlatformConfigurationDesired::default();
        PlatformConfigurationVersion {
            revision: Revision::new(1).unwrap(),
            configuration_sha256: Sha256Digest::of_canonical(&configuration)
                .unwrap()
                .to_string(),
            configuration,
            created_by: ActorId::new(),
            created_at: UtcTimestamp::from_utc(time::OffsetDateTime::UNIX_EPOCH).unwrap(),
        }
    }

    #[test]
    fn canonical_version_rejects_changed_settings_and_unknown_permissions() {
        let original = version();
        let payload = serde_json::to_vec(&original).unwrap();
        assert_eq!(validate(&payload).unwrap(), original);
        let mut changed = serde_json::to_value(&original).unwrap();
        changed["configuration"]["lifecycle"]["preemptionGraceSeconds"] = json!(90);
        assert_eq!(
            validate(&serde_json::to_vec(&changed).unwrap())
                .unwrap_err()
                .diagnostic_code(),
            "LW_BUNDLE_CONFIGURATION_DIGEST_MISMATCH"
        );
        changed["configuration"]["runtimePolicy"]["allowedCapabilities"] = json!(["ALL"]);
        assert_eq!(
            validate(&serde_json::to_vec(&changed).unwrap())
                .unwrap_err()
                .diagnostic_code(),
            "LW_BUNDLE_CONFIGURATION_INVALID"
        );
    }

    #[test]
    fn secret_fields_duplicate_fields_and_invalid_boundaries_are_not_importable() {
        let mut value = serde_json::to_value(version()).unwrap();
        value["configuration"]["token"] = json!("synthetic-token-never-echo");
        let error = validate(&serde_json::to_vec(&value).unwrap()).unwrap_err();
        assert!(!error.to_string().contains("synthetic-token"));
        value["configuration"]
            .as_object_mut()
            .unwrap()
            .remove("token");
        value["configuration"]["runtimePolicy"]["privilegedAllowed"] = json!(true);
        assert!(validate(&serde_json::to_vec(&value).unwrap()).is_err());
        assert!(validate(br#"{"revision":1,"revision":2}"#).is_err());
        assert!(validate(&vec![b' '; MAX_BYTES as usize + 1]).is_err());
    }
}
