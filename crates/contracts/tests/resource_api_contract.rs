//! Resource browser API and generated projection conformance for Issue #142.

use serde_json::{Value, json};

use contracts::{PlatformRole, http::operation_contract};

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
        "courseId": "01900000-0000-7000-8000-8000-000000000001",
        "requestKey": "resource-api-contract",
        "environmentId": "01900000-0000-7000-8000-000000000002",
        "releaseId": "01900000-0000-7000-8000-000000000003",
        "releaseVersion": 1,
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
    assert_eq!(leases["parameters"], json!([]));
    assert_eq!(
        leases["responses"]["200"]["content"]["application/json"]["schema"]["type"],
        "array"
    );
    assert_eq!(
        leases["responses"]["200"]["content"]["application/json"]["schema"]["items"]["$ref"],
        "../contracts/v1/resource-lease.schema.json"
    );
    let requests = &paths["/api/v1/resource-requests"]["get"];
    assert_eq!(requests["parameters"], json!([]));

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

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the endpoint matrix keeps the complete public Resource contract reviewable in one test"
)]
fn generated_openapi_exposes_project_resource_finance_and_options_endpoints()
-> Result<(), Box<dyn std::error::Error>> {
    let openapi: Value = serde_json::from_str(include_str!(
        "../../../schemas/openapi/labweaver-public.v1.json"
    ))?;
    let paths = openapi["paths"].as_object().ok_or("paths missing")?;

    let assert_operation =
        |path: &str, method: &str, operation_id: &str, permission: &str, scope: &str| {
            let operation = &paths[path][method];
            assert_eq!(operation["operationId"], operation_id);
            assert_eq!(operation["x-labweaver-permission"], permission);
            assert_eq!(operation["x-labweaver-scope"], scope);
            assert_eq!(operation["security"], json!([{ "bffSession": [] }]));
            operation
        };

    let create_request = assert_operation(
        "/api/v1/projects/{projectId}/resource-requests",
        "post",
        "createProjectResourceRequest",
        "resource_request:write",
        "project",
    );
    assert_eq!(
        create_request["responses"]["202"]["content"]["application/json"]["schema"]["$ref"],
        "../contracts/v1/http/resource-operation-accepted.schema.json"
    );
    assert_eq!(
        create_request["requestBody"]["content"]["application/json"]["schema"]["$ref"],
        "../contracts/v1/http/create-resource-request.schema.json"
    );
    let list_request = assert_operation(
        "/api/v1/projects/{projectId}/resource-requests",
        "get",
        "listProjectResourceRequests",
        "resource_request:read",
        "project",
    );
    assert_eq!(
        list_request["responses"]["200"]["content"]["application/json"]["schema"]["items"]["$ref"],
        "../contracts/v1/resource-request.schema.json"
    );
    assert_eq!(
        list_request["parameters"]
            .as_array()
            .and_then(|parameters| parameters
                .iter()
                .find(|parameter| parameter["name"] == "courseId"))
            .and_then(|parameter| parameter["schema"]["format"].as_str()),
        Some("uuid")
    );
    let list_lease = assert_operation(
        "/api/v1/projects/{projectId}/resource-leases",
        "get",
        "listProjectResourceLeases",
        "resource_lease:read",
        "project",
    );
    assert_eq!(
        list_lease["responses"]["200"]["content"]["application/json"]["schema"]["items"]["$ref"],
        "../contracts/v1/resource-lease.schema.json"
    );
    assert_eq!(
        list_lease["parameters"]
            .as_array()
            .and_then(|parameters| parameters
                .iter()
                .find(|parameter| parameter["name"] == "courseId"))
            .and_then(|parameter| parameter["schema"]["format"].as_str()),
        Some("uuid")
    );

    let gpu_catalog = assert_operation(
        "/api/v1/resource/gpu-catalog",
        "get",
        "listResourceGpuCatalog",
        "resource_gpu_catalog:read",
        "global",
    );
    assert_eq!(
        gpu_catalog["responses"]["200"]["content"]["application/json"]["schema"]["items"]["$ref"],
        "../contracts/v1/gpu-catalog-entry.schema.json"
    );
    let create_gpu_catalog = assert_operation(
        "/api/v1/resource/gpu-catalog",
        "post",
        "createResourceGpuCatalogEntry",
        "resource_gpu_catalog:write",
        "global",
    );
    assert_eq!(
        create_gpu_catalog["requestBody"]["content"]["application/json"]["schema"]["$ref"],
        "../contracts/v1/gpu-catalog-entry.schema.json"
    );
    assert_eq!(
        create_gpu_catalog["responses"]["201"]["content"]["application/json"]["schema"]["$ref"],
        "../contracts/v1/gpu-catalog-entry.schema.json"
    );

    let rates = assert_operation(
        "/api/v1/resource/rates",
        "get",
        "listResourceRates",
        "resource_rate:read",
        "global",
    );
    assert_eq!(
        rates["responses"]["200"]["content"]["application/json"]["schema"]["items"]["$ref"],
        "../contracts/v1/resource-rate.schema.json"
    );
    let create_rate = assert_operation(
        "/api/v1/resource/rates",
        "post",
        "createResourceRate",
        "resource_rate:write",
        "global",
    );
    assert_eq!(
        create_rate["requestBody"]["content"]["application/json"]["schema"]["$ref"],
        "../contracts/v1/http/create-resource-rate-request.schema.json"
    );
    assert_eq!(
        create_rate["responses"]["201"]["content"]["application/json"]["schema"]["$ref"],
        "../contracts/v1/resource-rate.schema.json"
    );

    let budget_path = "/api/v1/projects/{projectId}/resource-budget";
    let budget = assert_operation(
        budget_path,
        "get",
        "getProjectResourceBudget",
        "resource_budget:read",
        "project",
    );
    assert_eq!(
        budget["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
        "../contracts/v1/resource-budget.schema.json"
    );
    let upsert_budget = assert_operation(
        budget_path,
        "put",
        "upsertProjectResourceBudget",
        "resource_budget:write",
        "project",
    );
    assert_eq!(
        upsert_budget["requestBody"]["content"]["application/json"]["schema"]["$ref"],
        "../contracts/v1/http/upsert-resource-budget-request.schema.json"
    );
    assert_eq!(
        upsert_budget["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
        "../contracts/v1/resource-budget.schema.json"
    );

    let charges = assert_operation(
        "/api/v1/projects/{projectId}/charges",
        "get",
        "listProjectResourceCharges",
        "resource_charge:read",
        "project",
    );
    assert_eq!(
        charges["responses"]["200"]["content"]["application/json"]["schema"]["items"]["$ref"],
        "../contracts/v1/resource-charge.schema.json"
    );
    let adjustment = assert_operation(
        "/api/v1/projects/{projectId}/charges/{chargeId}/adjustments",
        "post",
        "createProjectResourceChargeAdjustment",
        "resource_charge:adjust",
        "project",
    );
    assert_eq!(
        adjustment["requestBody"]["content"]["application/json"]["schema"]["$ref"],
        "../contracts/v1/http/create-resource-adjustment-request.schema.json"
    );
    assert_eq!(
        adjustment["responses"]["201"]["content"]["application/json"]["schema"]["$ref"],
        "../contracts/v1/resource-charge.schema.json"
    );
    Ok(())
}

#[test]
fn resource_list_authorization_scopes_are_explicit() -> Result<(), Box<dyn std::error::Error>> {
    for (operation_id, permission) in [
        ("listResourceRequests", "resource_request:read"),
        ("listResourceLeases", "resource_lease:read"),
    ] {
        let operation =
            operation_contract(operation_id).ok_or("resource list operation missing")?;
        assert_eq!(operation.permission, permission);
        assert_eq!(operation.allowed_roles, &[PlatformRole::PlatformAdmin]);
        assert_eq!(operation.scope, contracts::http::OperationScopeKind::Global);
    }
    for (operation_id, permission) in [
        ("listProjectResourceRequests", "resource_request:read"),
        ("listProjectResourceLeases", "resource_lease:read"),
    ] {
        let operation =
            operation_contract(operation_id).ok_or("project resource list operation missing")?;
        assert_eq!(operation.permission, permission);
        assert_eq!(
            operation.allowed_roles,
            &[
                PlatformRole::Teacher,
                PlatformRole::Student,
                PlatformRole::PlatformAdmin,
            ]
        );
        assert_eq!(
            operation.scope,
            contracts::http::OperationScopeKind::Project
        );
    }
    Ok(())
}

#[test]
fn resource_option_reads_allow_all_authenticated_platform_roles()
-> Result<(), Box<dyn std::error::Error>> {
    for operation_id in ["listResourceGpuCatalog", "listResourceRates"] {
        let operation = operation_contract(operation_id).ok_or("resource option operation")?;
        assert_eq!(
            operation.allowed_roles,
            &[
                PlatformRole::Teacher,
                PlatformRole::Student,
                PlatformRole::PlatformAdmin,
            ]
        );
        assert_eq!(operation.scope, contracts::http::OperationScopeKind::Global);
        assert_eq!(operation.security, contracts::http::Security::BffSession);
    }
    for operation_id in ["createResourceGpuCatalogEntry", "createResourceRate"] {
        let operation = operation_contract(operation_id).ok_or("resource option operation")?;
        assert_eq!(operation.allowed_roles, &[PlatformRole::PlatformAdmin]);
    }
    Ok(())
}
