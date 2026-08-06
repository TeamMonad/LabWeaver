//! Resource browser API and generated projection conformance for Issue #142.

use serde_json::{Value, json};

#[test]
fn resource_create_requires_project_scope() -> Result<(), Box<dyn std::error::Error>> {
    let schema: Value = serde_json::from_str(include_str!(
        "../../../schemas/contracts/v1/http/create-resource-request.schema.json"
    ))?;
    let required = schema["required"]
        .as_array()
        .ok_or("required fields missing")?;
    assert!(required.iter().any(|field| field == "projectId"));

    let validator = jsonschema::validator_for(&schema)?;
    let value = json!({
        "courseId": "01900000-0000-7000-8000-000000000001",
        "requestKey": "resource-api-contract",
        "environmentId": "01900000-0000-7000-8000-000000000002",
        "releaseId": "01900000-0000-7000-8000-000000000003",
        "releaseVersion": 1,
        "releaseSha256": "11".repeat(32),
        "resources": {
            "cpuMillicores": 500,
            "memoryMebibytes": 512,
            "ephemeralStorageMebibytes": 1024,
            "persistentStorageMebibytes": 1024
        },
        "durationSeconds": 3600
    });
    assert!(!validator.is_valid(&value));
    Ok(())
}

#[test]
fn generated_openapi_types_every_resource_body_and_response()
-> Result<(), Box<dyn std::error::Error>> {
    let openapi: Value = serde_json::from_str(include_str!(
        "../../../schemas/openapi/labweaver-public.v1.json"
    ))?;
    let paths = openapi["paths"].as_object().ok_or("paths missing")?;

    let create = &paths["/api/v1/resource-requests"]["post"];
    assert_eq!(
        create["requestBody"]["content"]["application/json"]["schema"]["$ref"],
        "../contracts/v1/http/create-resource-request.schema.json"
    );
    assert_eq!(
        create["responses"]["202"]["content"]["application/json"]["schema"]["$ref"],
        "../contracts/v1/http/resource-operation-accepted.schema.json"
    );

    let leases = &paths["/api/v1/resource-leases"]["get"];
    assert_eq!(
        leases["responses"]["200"]["content"]["application/json"]["schema"]["type"],
        "array"
    );
    assert_eq!(
        leases["responses"]["200"]["content"]["application/json"]["schema"]["items"]["$ref"],
        "../contracts/v1/resource-lease.schema.json"
    );

    for action in ["renew", "revoke"] {
        let operation = &paths[&format!("/api/v1/resource-leases/{{leaseId}}/{action}")]["post"];
        assert!(
            operation["requestBody"]["content"]["application/json"]["schema"]["$ref"].is_string()
        );
        assert_eq!(
            operation["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
            "../contracts/v1/resource-lease.schema.json"
        );
    }
    Ok(())
}
