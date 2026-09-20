//! Administrator platform image catalog contract conformance for Issue #191.

use contracts::http::{
    ApiSurface, Method, MutationContract, OperationScopeKind, PlatformImageCatalog,
    PlatformImageCatalogView, PlatformImageEntry, PlatformImageEntryView, PlatformImageKind,
    PlatformImageStatus, Security, operation_contract, valid_platform_image_binding,
};
use contracts::{PlatformImageId, PlatformRole, UtcTimestamp};
use serde_json::{Value, json};

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the operation matrix keeps the complete administrator image contract reviewable in one test"
)]
fn platform_image_operations_publish_the_reviewed_contract()
-> Result<(), Box<dyn std::error::Error>> {
    for (
        operation_id,
        surface,
        method,
        path,
        permission,
        security,
        mutation,
        success_status,
        scope,
    ) in [
        (
            "listPlatformImages",
            ApiSurface::Public,
            Method::Get,
            "/api/v1/admin/images",
            "platform_image:read",
            Security::BffSession,
            MutationContract::None,
            200,
            OperationScopeKind::Global,
        ),
        (
            "registerPlatformImage",
            ApiSurface::Public,
            Method::Post,
            "/api/v1/admin/images",
            "platform_image:write",
            Security::BffSession,
            MutationContract::IdempotentCreate,
            201,
            OperationScopeKind::Global,
        ),
        (
            "repinPlatformImage",
            ApiSurface::Public,
            Method::Post,
            "/api/v1/admin/images/{catalogId}/repin",
            "platform_image:write",
            Security::BffSession,
            MutationContract::IdempotentRevisioned,
            200,
            OperationScopeKind::Global,
        ),
        (
            "disablePlatformImage",
            ApiSurface::Public,
            Method::Post,
            "/api/v1/admin/images/{catalogId}/disable",
            "platform_image:write",
            Security::BffSession,
            MutationContract::IdempotentRevisioned,
            200,
            OperationScopeKind::Global,
        ),
        (
            "createPlatformImageUpload",
            ApiSurface::Public,
            Method::Post,
            "/api/v1/admin/images/uploads",
            "platform_image:write",
            Security::BffSession,
            MutationContract::IdempotentCreate,
            201,
            OperationScopeKind::Global,
        ),
        (
            "completePlatformImageUpload",
            ApiSurface::Public,
            Method::Post,
            "/api/v1/admin/images/uploads/{uploadId}/complete",
            "platform_image:write",
            Security::BffSession,
            MutationContract::IdempotentRevisioned,
            201,
            OperationScopeKind::Global,
        ),
        (
            "importPlatformImage",
            ApiSurface::GatewayInternal,
            Method::Post,
            "/internal/v1/platform-images/imports",
            "agent.control.invoke",
            Security::ServiceJwt,
            MutationContract::IdempotentCreate,
            201,
            OperationScopeKind::Service,
        ),
    ] {
        let operation =
            operation_contract(operation_id).ok_or("platform image operation missing")?;
        assert_eq!(operation.surface, surface, "{operation_id}");
        assert_eq!(operation.method, method, "{operation_id}");
        assert_eq!(operation.path, path, "{operation_id}");
        assert_eq!(operation.permission, permission, "{operation_id}");
        assert_eq!(operation.security, security, "{operation_id}");
        assert_eq!(operation.mutation, mutation, "{operation_id}");
        assert_eq!(operation.success_status, success_status, "{operation_id}");
        assert_eq!(operation.scope, scope, "{operation_id}");
        assert_eq!(
            operation.allowed_roles,
            &[PlatformRole::PlatformAdmin],
            "{operation_id}"
        );
        assert!(!operation.cancellable, "{operation_id}");
        assert!(operation.retryable, "{operation_id}");
    }
    Ok(())
}

#[test]
fn generated_openapi_types_every_platform_image_body_and_response()
-> Result<(), Box<dyn std::error::Error>> {
    let public: Value = serde_json::from_str(include_str!(
        "../../../schemas/openapi/labweaver-public.v1.json"
    ))?;
    let paths = public["paths"].as_object().ok_or("paths missing")?;

    let list = &paths["/api/v1/admin/images"]["get"];
    assert_eq!(list["operationId"], "listPlatformImages");
    assert_eq!(list["security"], json!([{ "bffSession": [] }]));
    assert_eq!(
        list["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
        "../contracts/v1/http/platform-image-catalog-view.schema.json"
    );
    let register = &paths["/api/v1/admin/images"]["post"];
    assert_eq!(
        register["requestBody"]["content"]["application/json"]["schema"]["$ref"],
        "../contracts/v1/http/register-platform-image-request.schema.json"
    );
    assert_eq!(
        register["responses"]["201"]["content"]["application/json"]["schema"]["$ref"],
        "../contracts/v1/http/platform-image-entry-view.schema.json"
    );

    for (path, operation_id) in [
        (
            "/api/v1/admin/images/{catalogId}/repin",
            "repinPlatformImage",
        ),
        (
            "/api/v1/admin/images/{catalogId}/disable",
            "disablePlatformImage",
        ),
    ] {
        let operation = &paths[path]["post"];
        assert_eq!(operation["operationId"], operation_id);
        assert_eq!(
            operation["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
            "../contracts/v1/http/platform-image-entry-view.schema.json"
        );
        let parameters = operation["parameters"]
            .as_array()
            .ok_or("parameters missing")?;
        assert!(
            parameters
                .iter()
                .any(|parameter| parameter["name"] == "catalogId"
                    && parameter["in"] == "path"
                    && parameter["required"] == json!(true))
        );
        assert!(
            parameters
                .iter()
                .any(|parameter| parameter["name"] == "Idempotency-Key"
                    && parameter["in"] == "header"
                    && parameter["required"] == json!(true))
        );
        assert!(
            parameters
                .iter()
                .any(|parameter| parameter["name"] == "If-Match"
                    && parameter["in"] == "header"
                    && parameter["required"] == json!(true))
        );
    }

    let create_upload = &paths["/api/v1/admin/images/uploads"]["post"];
    assert_eq!(
        create_upload["requestBody"]["content"]["application/json"]["schema"]["$ref"],
        "../contracts/v1/http/create-platform-image-upload-request.schema.json"
    );
    assert_eq!(
        create_upload["responses"]["201"]["content"]["application/json"]["schema"]["$ref"],
        "../contracts/v1/http/platform-image-upload-session.schema.json"
    );
    let complete = &paths["/api/v1/admin/images/uploads/{uploadId}/complete"]["post"];
    assert_eq!(
        complete["requestBody"]["content"]["application/json"]["schema"]["$ref"],
        "../contracts/v1/http/complete-platform-image-upload-request.schema.json"
    );
    assert_eq!(
        complete["responses"]["201"]["content"]["application/json"]["schema"]["$ref"],
        "../contracts/v1/http/platform-image-entry-view.schema.json"
    );

    let internal: Value = serde_json::from_str(include_str!(
        "../../../schemas/openapi/labweaver-gateway-internal.v1.json"
    ))?;
    let import = &internal["paths"]["/internal/v1/platform-images/imports"]["post"];
    assert_eq!(import["operationId"], "importPlatformImage");
    assert_eq!(
        import["security"],
        json!([{ "serviceJwt": ["agent.control.invoke"] }])
    );
    assert_eq!(
        import["requestBody"]["content"]["application/json"]["schema"]["$ref"],
        "../contracts/v1/http/internal-platform-image-import-request.schema.json"
    );
    assert_eq!(
        import["responses"]["201"]["content"]["application/json"]["schema"]["$ref"],
        "../contracts/v1/http/platform-image-entry.schema.json"
    );
    Ok(())
}

#[test]
fn platform_image_entry_view_round_trips_with_release_impact()
-> Result<(), Box<dyn std::error::Error>> {
    let entry = PlatformImageEntry {
        catalog_id: PlatformImageId::new(),
        kind: PlatformImageKind::Container,
        binding: "ubuntu-24.04-v1".to_owned(),
        source_reference: "harbor.lab.lan/labweaver-system/ubuntu:24.04".to_owned(),
        resolved_digest: format!("sha256:{}", "a".repeat(64)),
        media_type: "application/vnd.oci.image.manifest.v1+json".to_owned(),
        size_bytes: 4_096,
        status: PlatformImageStatus::Active,
        trust_revision: 3,
        repin_generation: 1,
        pinned_at: "2026-01-02T03:04:05.000Z".parse::<UtcTimestamp>()?,
        updated_at: "2026-01-02T03:04:06.000Z".parse::<UtcTimestamp>()?,
    };
    let view = PlatformImageEntryView {
        entry: entry.clone(),
        release_reference_count: 2,
    };

    let encoded = serde_json::to_value(&view)?;
    assert_eq!(encoded["releaseReferenceCount"], json!(2));
    assert_eq!(encoded["catalogId"], json!(entry.catalog_id.to_string()));
    assert_eq!(encoded["kind"], json!("container"));
    assert_eq!(encoded["status"], json!("active"));
    assert_eq!(encoded["sizeBytes"], json!(4_096));
    assert_eq!(encoded["pinnedAt"], json!("2026-01-02T03:04:05.000Z"));
    assert!(encoded["entry"].as_object().is_none());
    assert_eq!(
        serde_json::from_value::<PlatformImageEntryView>(encoded)?,
        view
    );

    let catalog_view = PlatformImageCatalogView {
        entries: vec![view.clone()],
    };
    let encoded = serde_json::to_value(&catalog_view)?;
    assert_eq!(encoded["entries"].as_array().map(Vec::len), Some(1));
    assert_eq!(encoded["entries"][0]["releaseReferenceCount"], json!(2));
    assert_eq!(
        encoded["entries"][0]["catalogId"],
        json!(entry.catalog_id.to_string())
    );
    assert_eq!(
        serde_json::from_value::<PlatformImageCatalogView>(encoded)?,
        catalog_view
    );

    let catalog = PlatformImageCatalog {
        entries: vec![entry],
    };
    let encoded = serde_json::to_value(&catalog)?;
    assert_eq!(encoded["entries"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        serde_json::from_value::<PlatformImageCatalog>(encoded)?,
        catalog
    );
    Ok(())
}

#[test]
fn platform_image_media_type_is_the_reviewed_layout_archive() {
    assert_eq!(
        contracts::http::PLATFORM_IMAGE_ARCHIVE_MEDIA_TYPE,
        "application/vnd.oci.image.layout.v1+tar"
    );
}

#[test]
fn platform_image_binding_charset_is_reviewed() {
    for accepted in ["ubuntu-24.04-v1", "a", "0"] {
        assert!(valid_platform_image_binding(accepted), "{accepted}");
    }
    let boundary = "a".repeat(128);
    assert!(valid_platform_image_binding(&boundary));
    for rejected in [
        "Ubuntu".to_owned(),
        "-x".to_owned(),
        "a/b".to_owned(),
        "a b".to_owned(),
        String::new(),
        "a".repeat(129),
    ] {
        assert!(!valid_platform_image_binding(&rejected), "{rejected}");
    }
}
