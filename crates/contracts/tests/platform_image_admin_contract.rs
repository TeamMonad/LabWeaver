//! Administrator platform image catalog contract conformance for Issue #191.

use contracts::http::{
    ApiSurface, Method, MutationContract, OperationScopeKind, PLATFORM_IMAGE_DISK_PATH_MAX_BYTES,
    PlatformImageCatalog, PlatformImageCatalogView, PlatformImageEntry, PlatformImageEntryView,
    PlatformImageKind, PlatformImageStatus, Security, operation_contract,
    valid_platform_image_binding, valid_vm_disk_upload,
};
use contracts::supply_chain::VirtualMachineDiskFormat;
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
        capacity_bytes: None,
        disk_sha256: None,
        format: None,
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
    assert!(encoded.get("capacityBytes").is_none());
    assert!(encoded.get("diskSha256").is_none());
    assert!(encoded.get("format").is_none());
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

#[test]
fn platform_image_vm_disk_descriptor_binds_to_the_virtual_machine_kind()
-> Result<(), Box<dyn std::error::Error>> {
    let entry = PlatformImageEntry {
        catalog_id: PlatformImageId::new(),
        kind: PlatformImageKind::VirtualMachine,
        binding: "ubuntu-24.04-vm-v1".to_owned(),
        source_reference: "harbor.lab.lan/labweaver-system/ubuntu-vm:24.04".to_owned(),
        resolved_digest: format!("sha256:{}", "b".repeat(64)),
        media_type: "application/vnd.oci.image.manifest.v1+json".to_owned(),
        size_bytes: 8_192,
        status: PlatformImageStatus::Active,
        trust_revision: 1,
        repin_generation: 0,
        capacity_bytes: Some(8_589_934_592),
        disk_sha256: Some("c".repeat(64)),
        format: Some(VirtualMachineDiskFormat::Qcow2),
        pinned_at: "2026-01-02T03:04:05.000Z".parse::<UtcTimestamp>()?,
        updated_at: "2026-01-02T03:04:06.000Z".parse::<UtcTimestamp>()?,
    };

    let encoded = serde_json::to_value(&entry)?;
    assert_eq!(encoded["capacityBytes"], json!(8_589_934_592_u64));
    assert_eq!(encoded["diskSha256"], json!("c".repeat(64)));
    assert_eq!(encoded["format"], json!("qcow2"));
    assert_eq!(
        serde_json::from_value::<PlatformImageEntry>(encoded)?,
        entry
    );
    Ok(())
}

#[test]
fn platform_image_container_uploads_carry_no_disk_descriptor() {
    assert!(valid_vm_disk_upload(
        PlatformImageKind::Container,
        None,
        None,
        None
    ));
    for (disk_format, disk_path, capacity_bytes) in [
        (Some(VirtualMachineDiskFormat::Raw), None, None),
        (None, Some("disk/disk.img"), None),
        (None, None, Some(8_589_934_592)),
        (
            Some(VirtualMachineDiskFormat::Qcow2),
            Some("disk/disk.img"),
            Some(8_589_934_592),
        ),
    ] {
        assert!(
            !valid_vm_disk_upload(
                PlatformImageKind::Container,
                disk_format,
                disk_path,
                capacity_bytes
            ),
            "container upload accepted {disk_format:?} {disk_path:?} {capacity_bytes:?}"
        );
    }
}

#[test]
fn platform_image_vm_uploads_accept_only_a_complete_disk_descriptor() {
    // An already-published registry containerdisk reference carries no disk descriptor.
    assert!(valid_vm_disk_upload(
        PlatformImageKind::VirtualMachine,
        None,
        None,
        None
    ));
    for (disk_format, disk_path, capacity_bytes) in [
        (
            Some(VirtualMachineDiskFormat::Raw),
            Some("disk/disk.img"),
            None,
        ),
        (Some(VirtualMachineDiskFormat::Raw), None, Some(4_096)),
        (None, Some("disk/disk.img"), Some(4_096)),
    ] {
        assert!(
            !valid_vm_disk_upload(
                PlatformImageKind::VirtualMachine,
                disk_format,
                disk_path,
                capacity_bytes
            ),
            "partial descriptor accepted {disk_format:?} {disk_path:?} {capacity_bytes:?}"
        );
    }
    for disk_format in [
        VirtualMachineDiskFormat::Qcow2,
        VirtualMachineDiskFormat::Raw,
    ] {
        assert!(valid_vm_disk_upload(
            PlatformImageKind::VirtualMachine,
            Some(disk_format),
            Some("disk/disk.img"),
            Some(4_096)
        ));
    }
}

#[test]
fn platform_image_vm_disk_paths_stay_relative_and_bounded() {
    for accepted in ["disk.img", "disk/disk.img", "images/ubuntu-24.04.qcow2"] {
        assert!(
            valid_vm_disk_upload(
                PlatformImageKind::VirtualMachine,
                Some(VirtualMachineDiskFormat::Raw),
                Some(accepted),
                Some(4_096)
            ),
            "{accepted}"
        );
    }
    let boundary = "a".repeat(PLATFORM_IMAGE_DISK_PATH_MAX_BYTES);
    assert!(valid_vm_disk_upload(
        PlatformImageKind::VirtualMachine,
        Some(VirtualMachineDiskFormat::Raw),
        Some(boundary.as_str()),
        Some(4_096)
    ));
    for rejected in [
        "",
        "/disk/disk.img",
        "disk/",
        "..",
        "../disk.img",
        "disk/../disk.img",
        "disk//disk.img",
    ] {
        assert!(
            !valid_vm_disk_upload(
                PlatformImageKind::VirtualMachine,
                Some(VirtualMachineDiskFormat::Raw),
                Some(rejected),
                Some(4_096)
            ),
            "{rejected}"
        );
    }
    let oversized = "a".repeat(PLATFORM_IMAGE_DISK_PATH_MAX_BYTES + 1);
    assert!(!valid_vm_disk_upload(
        PlatformImageKind::VirtualMachine,
        Some(VirtualMachineDiskFormat::Raw),
        Some(oversized.as_str()),
        Some(4_096)
    ));
}

#[test]
fn platform_image_vm_disk_capacity_must_be_positive() {
    assert!(!valid_vm_disk_upload(
        PlatformImageKind::VirtualMachine,
        Some(VirtualMachineDiskFormat::Qcow2),
        Some("disk/disk.img"),
        Some(0)
    ));
}
