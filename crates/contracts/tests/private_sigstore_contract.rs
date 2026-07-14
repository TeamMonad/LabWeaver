//! Fail-closed Private Sigstore contract tests.

use std::str::FromStr;

use contracts::supply_chain::{
    PrivateSigstoreBackupIdentity, PrivateSigstoreTestFlightReport, PrivateSigstoreTrustBundle,
    SigstoreEvidence, SupplyChainError, TestFlightCheck, TestFlightStatus, TufRootIdentity,
    WorkloadIdentityPolicy,
};
use contracts::{Sha256Digest, UtcTimestamp};

#[allow(clippy::expect_used, reason = "fixed RFC 3339 test vectors")]
fn timestamp(value: &str) -> UtcTimestamp {
    UtcTimestamp::from_str(value).expect("test timestamp must be valid")
}

fn digest(value: &[u8]) -> Sha256Digest {
    Sha256Digest::of_bytes(value)
}

fn workload_policy() -> WorkloadIdentityPolicy {
    WorkloadIdentityPolicy {
        schema_version: "private-sigstore.v1".into(),
        issuer: "https://keycloak.labweaver.internal/realms/workloads".into(),
        audience: "labweaver-buildkit".into(),
        client_id: "labweaver-buildkit".into(),
        allowed_subjects: vec![
            "service-account-labweaver-buildkit!workloads.labweaver.internal".into(),
        ],
        required_claims: vec!["iss".into(), "aud".into(), "sub".into(), "azp".into()],
        certificate_identity_template:
            "service-account-labweaver-buildkit!workloads.labweaver.internal".into(),
        token_lifetime_milliseconds: 300_000,
        clock_skew_milliseconds: 30_000,
        replay_cache_ttl_milliseconds: 600_000,
    }
}

#[test]
fn workload_identity_rejects_public_wildcard_and_human_subjects() {
    assert!(workload_policy().validate().is_ok());

    let mut public = workload_policy();
    public.issuer = "https://oauth2.sigstore.dev/auth".into();
    assert_eq!(
        public.validate(),
        Err(SupplyChainError::WorkloadIdentityInvalid)
    );

    let mut wildcard = workload_policy();
    wildcard.allowed_subjects = vec!["*".into()];
    assert_eq!(
        wildcard.validate(),
        Err(SupplyChainError::WorkloadIdentityInvalid)
    );

    let mut human = workload_policy();
    human.allowed_subjects = vec!["user:teacher@example.test".into()];
    assert_eq!(
        human.validate(),
        Err(SupplyChainError::WorkloadIdentityInvalid)
    );

    let mut wrong_audience = workload_policy();
    wrong_audience.audience = "other-client".into();
    assert_eq!(
        wrong_audience.validate(),
        Err(SupplyChainError::WorkloadIdentityInvalid)
    );

    let mut missing_claim = workload_policy();
    missing_claim.required_claims.retain(|claim| claim != "azp");
    assert_eq!(
        missing_claim.validate(),
        Err(SupplyChainError::WorkloadIdentityInvalid)
    );

    let mut excessive_skew = workload_policy();
    excessive_skew.clock_skew_milliseconds = 90_000;
    assert_eq!(
        excessive_skew.validate(),
        Err(SupplyChainError::WorkloadIdentityInvalid)
    );
}

fn trust_bundle() -> PrivateSigstoreTrustBundle {
    PrivateSigstoreTrustBundle {
        schema_version: "private-sigstore.v1".into(),
        bundle_version: 1,
        generated_at: timestamp("2026-07-14T00:00:00.000Z"),
        expires_at: timestamp("2026-08-14T00:00:00.000Z"),
        run_id: "infra-1720000000-42".into(),
        commit_sha: "a".repeat(40),
        cluster_uid: "11111111-2222-3333-4444-555555555555".into(),
        inventory_sha256: digest(b"inventory"),
        deployment_manifest_sha256: digest(b"manifest"),
        component_lock_sha256: digest(b"lock"),
        trust_bundle_sha256: digest(b"bundle"),
        fulcio_root_sha256: digest(b"fulcio-root"),
        fulcio_intermediate_sha256: digest(b"fulcio-intermediate"),
        fulcio_issuer: "https://fulcio.sigstore.labweaver.internal".into(),
        audience: "labweaver-buildkit".into(),
        allowed_subjects: vec![
            "service-account-labweaver-buildkit!workloads.labweaver.internal".into(),
        ],
        rekor_public_key_sha256: digest(b"rekor-key"),
        rekor_log_id: "labweaver-rekor-v1".into(),
        ct_public_key_sha256: digest(b"ct-key"),
        ct_log_id: "labweaver-ct-v1".into(),
        tuf: TufRootIdentity {
            version: 1,
            root_sha256: digest(b"tuf-root"),
            targets_version: 1,
            snapshot_version: 1,
            timestamp_version: 1,
            expires_at: timestamp("2026-08-14T00:00:00.000Z"),
            compatibility_window_ends_at: timestamp("2026-08-01T00:00:00.000Z"),
            rotation_state: "stable".into(),
        },
    }
}

#[test]
fn trust_bundle_rejects_tamper_expiry_public_fallback_and_tuf_rollback() {
    let bundle = trust_bundle();
    assert!(
        bundle
            .validate(timestamp("2026-07-15T00:00:00.000Z"), digest(b"bundle"))
            .is_ok()
    );
    assert_eq!(
        bundle.validate(timestamp("2026-07-15T00:00:00.000Z"), digest(b"tampered")),
        Err(SupplyChainError::TrustBundleInvalid)
    );
    assert_eq!(
        bundle.validate(timestamp("2026-09-01T00:00:00.000Z"), digest(b"bundle")),
        Err(SupplyChainError::TrustBundleInvalid)
    );

    let mut public = bundle.clone();
    public.fulcio_issuer = "https://fulcio.sigstore.dev".into();
    assert_eq!(
        public.validate(timestamp("2026-07-15T00:00:00.000Z"), digest(b"bundle")),
        Err(SupplyChainError::TrustBundleInvalid)
    );

    let mut successor = bundle.tuf.clone();
    successor.version = bundle.tuf.version;
    assert_eq!(
        successor.validate_successor(&bundle.tuf),
        Err(SupplyChainError::TufRollbackDetected)
    );
}

#[test]
fn trust_bundle_rejects_wrong_transparency_and_certificate_identity() {
    let bundle = trust_bundle();
    let mut evidence = SigstoreEvidence {
        trust_bundle_sha256: bundle.trust_bundle_sha256,
        fulcio_issuer: bundle.fulcio_issuer.clone(),
        certificate_subject: bundle.allowed_subjects[0].clone(),
        certificate_sha256: digest(b"certificate"),
        signature_sha256: digest(b"signature"),
        rekor_log_id: bundle.rekor_log_id.clone(),
        rekor_log_index: 1,
        rekor_inclusion_proof_sha256: digest(b"inclusion"),
        ct_log_id: bundle.ct_log_id.clone(),
        sct_sha256: digest(b"sct"),
        verified_at: timestamp("2026-07-15T00:00:00.000Z"),
    };
    assert!(bundle.verify_evidence(&evidence).is_ok());

    evidence.certificate_subject = "system:serviceaccount:other:builder".into();
    assert_eq!(
        bundle.verify_evidence(&evidence),
        Err(SupplyChainError::SignatureInvalid)
    );
    evidence.certificate_subject = bundle.allowed_subjects[0].clone();
    evidence.rekor_log_id = "public-or-wrong-log".into();
    assert_eq!(
        bundle.verify_evidence(&evidence),
        Err(SupplyChainError::SignatureInvalid)
    );
    evidence.rekor_log_id = bundle.rekor_log_id.clone();
    evidence.sct_sha256 = Sha256Digest::of_bytes(&[]);
    assert_eq!(
        bundle.verify_evidence(&evidence),
        Err(SupplyChainError::SignatureInvalid)
    );
}

fn required_checks(status: TestFlightStatus) -> Vec<TestFlightCheck> {
    [
        "identity",
        "schema",
        "component_lock",
        "backup",
        "restore",
        "cleanup",
        "tls",
        "network_policy",
        "oidc",
        "sct",
        "rekor_inclusion",
        "tuf_root",
        "trust_bundle",
    ]
    .into_iter()
    .map(|name| TestFlightCheck {
        name: name.into(),
        status,
        diagnostic_code: (status != TestFlightStatus::Passed).then(|| "SIGSTORE_BLOCKED".into()),
    })
    .collect()
}

#[test]
fn testflight_report_cannot_hide_partial_failure() {
    let mut report = PrivateSigstoreTestFlightReport {
        schema_version: "private-sigstore-testflight.v1".into(),
        scope: "private-sigstore".into(),
        status: TestFlightStatus::Passed,
        run_id: "testflight-1720000000-42".into(),
        commit_sha: "b".repeat(40),
        cluster_uid: "11111111-2222-3333-4444-555555555555".into(),
        inventory_sha256: digest(b"inventory"),
        deployment_manifest_sha256: digest(b"manifest"),
        component_lock_sha256: digest(b"lock"),
        trust_bundle_sha256: digest(b"bundle"),
        workload_identity_policy_sha256: digest(b"oidc"),
        backup: Some(PrivateSigstoreBackupIdentity {
            schema_version: "private-sigstore-backup.v1".into(),
            run_id: "testflight-1720000000-42".into(),
            commit_sha: "b".repeat(40),
            cluster_uid: "11111111-2222-3333-4444-555555555555".into(),
            inventory_sha256: digest(b"inventory"),
            deployment_manifest_sha256: digest(b"manifest"),
            component_lock_sha256: digest(b"lock"),
            tuf_root_sha256: digest(b"tuf-root"),
            trust_bundle_sha256: digest(b"bundle"),
            artifact_sha256: digest(b"backup"),
            generated_at: timestamp("2026-07-14T00:00:00.000Z"),
        }),
        checks: required_checks(TestFlightStatus::Passed),
        cleanup_status: TestFlightStatus::Passed,
        blocked_items: Vec::new(),
        unblock_owner: None,
        exit_condition: None,
        generated_at: timestamp("2026-07-14T00:00:00.000Z"),
    };
    assert!(report.validate().is_ok());

    let valid_backup = report.backup.clone();
    report.backup = report.backup.clone().map(|mut backup| {
        backup.cluster_uid = "wrong-cluster".into();
        backup
    });
    assert_eq!(
        report.validate(),
        Err(SupplyChainError::TestFlightReportInvalid)
    );
    report.backup = valid_backup;

    report.checks[0].status = TestFlightStatus::Failed;
    report.checks[0].diagnostic_code = Some("SIGSTORE_IDENTITY_MISMATCH".into());
    assert_eq!(
        report.validate(),
        Err(SupplyChainError::TestFlightReportInvalid)
    );

    report.status = TestFlightStatus::Blocked;
    report.blocked_items = vec!["private-cluster".into()];
    report.unblock_owner = Some("@2018wzh".into());
    report.exit_condition = Some("run the identity-bound E3 TestFlight".into());
    assert!(report.validate().is_ok());
}
