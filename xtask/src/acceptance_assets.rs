use std::fs;
use std::path::Path;

use serde_json::Value;

use super::AppError;

const MANIFEST_PATH: &str = "tests/fixtures/acceptance/sprint3-acceptance-assets.json";
const ASSET_SCHEMA: &str = "sprint3-acceptance-assets.v1.schema.json";
const EVIDENCE_SCHEMA: &str = "sprint3-acceptance-evidence.v1.schema.json";
const REQUIRED_SCENARIOS: [&str; 3] = [
    "oj-real-e4",
    "container-linux-clone-real-e4",
    "kubevirt-linux-clone-real-e4",
];

pub(super) fn list() {
    for scenario in REQUIRED_SCENARIOS {
        println!("{scenario}");
    }
}

pub(super) fn validate(root: &Path) -> Result<(), AppError> {
    let manifest_path = root.join(MANIFEST_PATH);
    let document = read_schema_instance(
        root,
        ASSET_SCHEMA,
        &manifest_path,
        "LW_TEST_ASSET_SCHEMA_INVALID",
    )?;
    // thin check: ensure scenarios field exists and is non-empty; detailed inventory is schema-validated
    if document
        .get("scenarios")
        .and_then(Value::as_array)
        .is_none_or(|arr| arr.is_empty())
    {
        return Err(code(
            "LW_TEST_ASSET_SCENARIO_INVENTORY_INVALID",
            "scenarios are missing",
        ));
    }
    Ok(())
}

pub(super) fn validate_report(root: &Path, report: &Path) -> Result<(), AppError> {
    let _ = read_schema_instance(
        root,
        EVIDENCE_SCHEMA,
        report,
        "LW_TEST_ASSET_REPORT_SCHEMA_INVALID",
    )?;
    Ok(())
}

fn read_schema_instance(
    root: &Path,
    schema_name: &str,
    path: &Path,
    diagnostic: &'static str,
) -> Result<Value, AppError> {
    let schema = read_json(&root.join("schemas/results").join(schema_name), diagnostic)?;
    let instance = read_json(path, diagnostic)?;
    let validator = jsonschema::validator_for(&schema)
        .map_err(|_| code(diagnostic, "acceptance JSON Schema cannot be compiled"))?;
    if !validator.is_valid(&instance) {
        return Err(code(
            diagnostic,
            "document does not satisfy its JSON Schema",
        ));
    }
    Ok(instance)
}

fn read_json(path: &Path, diagnostic: &'static str) -> Result<Value, AppError> {
    let text =
        fs::read_to_string(path).map_err(|_| code(diagnostic, "JSON document is missing"))?;
    serde_json::from_str(&text).map_err(|_| code(diagnostic, "JSON document is malformed"))
}

fn code(code: &'static str, detail: impl Into<String>) -> AppError {
    AppError::AcceptanceAsset {
        code,
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
    }

    #[test]
    fn manifest_and_report_validate_thin() {
        validate(&root()).expect("checked-in manifest must validate via schema");
        let report = root().join("tests/fixtures/acceptance/reports/valid/local-e2.json");
        if report.exists() {
            validate_report(&root(), &report).expect("valid report must pass thin check");
        }
    }
}
