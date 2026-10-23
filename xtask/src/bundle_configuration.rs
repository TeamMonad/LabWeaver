//! Validate a portable managed version with the same types and digest as Control.

use std::io::{Read, Write};

use contracts::platform_configuration::{
    PlatformConfigurationContractError, PlatformConfigurationDesired, PlatformConfigurationVersion,
    UpdatePlatformConfigurationRequest,
};
use persistence_sqlx::Sha256Digest;

use crate::AppError;

const MAX_BYTES: u64 = 1_048_576;

fn invalid(code: &'static str) -> AppError {
    AppError::BundleConfiguration { code }
}

fn policy_error(error: PlatformConfigurationContractError) -> AppError {
    match error {
        PlatformConfigurationContractError::PolicyUnsupported { .. } => {
            invalid("LW_PLATFORM_CONFIGURATION_POLICY_UNSUPPORTED")
        }
        _ => invalid("LW_BUNDLE_CONFIGURATION_INVALID"),
    }
}

pub fn validate(payload: &[u8]) -> Result<PlatformConfigurationVersion, AppError> {
    if payload.is_empty() || payload.len() as u64 > MAX_BYTES {
        return Err(invalid("LW_BUNDLE_CONFIGURATION_LIMIT"));
    }
    let version: PlatformConfigurationVersion = contracts::parse_strict_json(payload)
        .map_err(|_| invalid("LW_BUNDLE_CONFIGURATION_INVALID"))?;
    version.configuration.validate().map_err(policy_error)?;
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

pub fn validate_initial(payload: &[u8]) -> Result<UpdatePlatformConfigurationRequest, AppError> {
    if payload.is_empty() || payload.len() as u64 > MAX_BYTES {
        return Err(invalid("LW_BUNDLE_CONFIGURATION_LIMIT"));
    }
    let request: UpdatePlatformConfigurationRequest = contracts::parse_strict_json(payload)
        .map_err(|_| invalid("LW_BUNDLE_CONFIGURATION_INVALID"))?;
    if request.expected_revision != 0 {
        return Err(invalid("LW_BUNDLE_INITIAL_REVISION_INVALID"));
    }
    request.configuration.validate().map_err(policy_error)?;
    Ok(request)
}

pub fn run(initial: bool, defaults: bool) -> Result<(), AppError> {
    let mut payload = Vec::new();
    if !defaults {
        std::io::stdin()
            .take(MAX_BYTES + 1)
            .read_to_end(&mut payload)
            .map_err(|_| invalid("LW_BUNDLE_CONFIGURATION_IO_FAILED"))?;
    }
    let document = if initial || defaults {
        let request = if defaults {
            UpdatePlatformConfigurationRequest {
                expected_revision: 0,
                configuration: PlatformConfigurationDesired::default(),
            }
        } else {
            validate_initial(&payload)?
        };
        let digest = Sha256Digest::of_canonical(&request.configuration)
            .map_err(|_| invalid("LW_BUNDLE_CONFIGURATION_INVALID"))?;
        serde_json::json!({"request":request, "configurationSha256":digest.to_string()})
    } else {
        serde_json::to_value(validate(&payload)?)
            .map_err(|_| invalid("LW_BUNDLE_CONFIGURATION_INVALID"))?
    };
    let mut output = std::io::stdout().lock();
    serde_json::to_writer(&mut output, &document)
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

    #[test]
    fn first_install_uses_only_an_unsaved_zero_revision_request() {
        let request = UpdatePlatformConfigurationRequest {
            expected_revision: 0,
            configuration: PlatformConfigurationDesired::default(),
        };
        let payload = serde_json::to_vec(&request).unwrap();
        assert_eq!(validate_initial(&payload).unwrap(), request);
        let mut wrong = serde_json::to_value(&request).unwrap();
        wrong["expectedRevision"] = json!(1);
        assert_eq!(
            validate_initial(&serde_json::to_vec(&wrong).unwrap())
                .unwrap_err()
                .diagnostic_code(),
            "LW_BUNDLE_INITIAL_REVISION_INVALID"
        );
        wrong["expectedRevision"] = json!(0);
        wrong["applied"] = json!({"revision":1});
        assert!(validate_initial(&serde_json::to_vec(&wrong).unwrap()).is_err());
        assert!(validate_initial(&serde_json::to_vec(&version()).unwrap()).is_err());
    }
}
