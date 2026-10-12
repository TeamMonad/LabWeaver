//! Generated API and runtime-policy transport invariants.

use contracts::{
    PlatformRole,
    http::{OperationScopeKind, operation_contract},
    platform_configuration::PlatformConfigurationDesired,
    schema::generate_all,
};
use serde_json::Value;

#[test]
fn configuration_operations_require_global_administrator_or_separate_executor_permission()
-> Result<(), Box<dyn std::error::Error>> {
    for name in [
        "getPlatformConfiguration",
        "updatePlatformConfiguration",
        "getPlatformConfigurationVersion",
        "applyPlatformConfiguration",
        "getPlatformConfigurationApply",
        "registerPlatformConfigurationBundle",
        "getPlatformConfigurationBundle",
    ] {
        let operation = operation_contract(name).ok_or("missing operation")?;
        assert_eq!(operation.allowed_roles, &[PlatformRole::PlatformAdmin]);
        assert_eq!(operation.scope, OperationScopeKind::Global);
    }
    let executor =
        operation_contract("reportPlatformConfigurationApply").ok_or("missing executor")?;
    assert_eq!(executor.scope, OperationScopeKind::Service);
    assert_eq!(executor.permission, "control.platform_configuration.apply");
    let observation =
        operation_contract("getLoadedPlatformConfiguration").ok_or("missing observation")?;
    assert_eq!(observation.scope, OperationScopeKind::Service);
    assert_eq!(observation.permission, executor.permission);
    Ok(())
}

#[test]
fn generated_configuration_schema_and_openapi_preserve_required_fields_and_yaml_fence()
-> Result<(), Box<dyn std::error::Error>> {
    let artifacts = generate_all()?;
    let desired = &artifacts
        .iter()
        .find(|a| {
            a.relative_path
                .ends_with("/platform-configuration-desired.schema.json")
        })
        .ok_or("missing desired schema")?
        .bytes;
    let schema: Value = serde_json::from_slice(desired)?;
    let validator = jsonschema::validator_for(&schema)?;
    assert!(validator.is_valid(&serde_json::to_value(
        PlatformConfigurationDesired::default()
    )?));
    assert!(!validator.is_valid(&serde_json::json!({"lifecycle":{}})));
    let public: Value = serde_json::from_slice(
        &artifacts
            .iter()
            .find(|a| a.relative_path == "schemas/openapi/labweaver-public.v1.json")
            .ok_or("missing public")?
            .bytes,
    )?;
    let update = &public["paths"]["/api/v1/admin/platform-configuration"]["patch"];
    assert_eq!(
        update["requestBody"]["content"]["application/json"]["schema"],
        update["requestBody"]["content"]["application/yaml"]["schema"]
    );
    assert!(
        update["parameters"]
            .as_array()
            .ok_or("missing headers")?
            .iter()
            .any(|v| v["name"] == "If-Match" && v["required"] == true)
    );
    assert!(
        public["paths"]
            .get("/internal/v1/platform-configuration/applies/{applyId}")
            .is_none()
    );
    assert!(
        public["paths"]
            .get("/internal/v1/platform-configuration/loaded")
            .is_none()
    );
    let version =
        &public["paths"]["/api/v1/admin/platform-configuration/versions/{revision}"]["get"];
    assert_eq!(version["parameters"][0]["schema"]["type"], "integer");
    let registration =
        &public["paths"]["/api/v1/admin/platform-configuration/versions/{revision}/bundle"]["post"];
    assert_eq!(
        registration["requestBody"]["content"]["application/octet-stream"]["schema"]["format"],
        "binary"
    );
    assert!(
        registration["requestBody"]["content"]
            .get("application/json")
            .is_none()
    );
    assert!(
        registration["parameters"]
            .as_array()
            .ok_or("missing binding query")?
            .iter()
            .any(|value| value["name"] == "configurationSha256" && value["required"] == true)
    );
    let apply_schema: Value = serde_json::from_slice(
        &artifacts
            .iter()
            .find(|artifact| {
                artifact
                    .relative_path
                    .ends_with("/apply-platform-configuration-request.schema.json")
            })
            .ok_or("missing apply request")?
            .bytes,
    )?;
    assert!(
        apply_schema["properties"]
            .get("encryptedBundleSha256")
            .is_none()
    );
    Ok(())
}

#[test]
fn legacy_report_serialization_keeps_its_original_idempotency_identity()
-> Result<(), Box<dyn std::error::Error>> {
    let legacy = serde_json::json!({"expectedRevision":1,"stage":"validating","failureReason":null,"observedConfigurationSha256":null,"observedEncryptedBundleSha256":null});
    let request: contracts::platform_configuration::ReportPlatformConfigurationApplyRequest =
        contracts::parse_strict_json(&serde_json::to_vec(&legacy)?)?;
    assert!(request.control_replica_observations.is_none());
    assert_eq!(serde_json::to_value(request)?, legacy);
    Ok(())
}
