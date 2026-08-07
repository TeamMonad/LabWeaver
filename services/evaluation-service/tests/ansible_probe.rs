//! Deterministic Ansible probe validation, assertion, and evidence-boundary tests.

use std::net::Ipv4Addr;

use contracts::Sha256Digest;
use contracts::evaluation::FactAssertion;
use evaluation_service::ansible_probe::{
    ANSIBLE_PROBE_EVIDENCE_RECEIPT_SCHEMA_VERSION, ANSIBLE_PROBE_EVIDENCE_SCHEMA_VERSION,
    ANSIBLE_PROBE_EXECUTION_SCHEMA_VERSION, AnsibleProbeAssertionStatus, AnsibleProbeError,
    AnsibleProbeEvidence, AnsibleProbeEvidenceReceipt, AnsibleProbeExecutionLimits,
    AnsibleProbeExecutionRequest, AnsibleProbeFacts, AnsibleProbeSshIdentity, AnsibleProbeTarget,
    AnsibleProbeTerminalStatus, MAX_FACTS, ProbeFactValue, evaluate_assertions,
};
use serde_json::json;
use uuid::Uuid;

fn digest(marker: &[u8]) -> Sha256Digest {
    Sha256Digest::of_bytes(marker)
}

fn assertion(fact: &str, expected: &serde_json::Value) -> FactAssertion {
    match serde_json::from_value(json!({ "fact": fact, "expected": expected })) {
        Ok(assertion) => assertion,
        Err(error) => unreachable!("fixture assertion must deserialize: {error}"),
    }
}

fn request() -> AnsibleProbeExecutionRequest {
    AnsibleProbeExecutionRequest {
        schema_version: ANSIBLE_PROBE_EXECUTION_SCHEMA_VERSION.to_owned(),
        run_id: Uuid::now_v7(),
        step_run_id: Uuid::now_v7(),
        attempt_id: Uuid::now_v7(),
        trace_id: "trace-ansible-probe-test".to_owned(),
        runner_image_digest: format!("labweaver/ansible-probe@sha256:{}", "2".repeat(64)),
        playbook_profile: "linux-nginx-probe-v1".to_owned(),
        module_allowlist: vec![
            "ansible.builtin.service_facts".to_owned(),
            "ansible.builtin.stat".to_owned(),
        ],
        read_only: true,
        assertions: vec![
            assertion("host.reachable", &json!(true)),
            assertion("service.nginx.active", &json!(true)),
        ],
        target: AnsibleProbeTarget {
            host: Ipv4Addr::new(192, 168, 56, 10),
            port: 22,
            username: "labweaver".to_owned(),
        },
        ssh_identity: AnsibleProbeSshIdentity {
            private_key_secret: "probe-ssh-key".to_owned(),
            certificate_secret: "probe-ssh-cert".to_owned(),
            expected_host_key_sha256: digest(b"host-key"),
        },
        limits: AnsibleProbeExecutionLimits {
            wall_time_seconds: 60,
            facts_max_bytes: 1024 * 1024,
            output_max_bytes: 64 * 1024,
            max_assertions: 8,
        },
        evaluation_spec_sha256: digest(b"evaluation-spec"),
    }
}

fn nginx_facts() -> Result<AnsibleProbeFacts, AnsibleProbeError> {
    let mut facts = AnsibleProbeFacts::new();
    facts.insert("host.reachable", ProbeFactValue::Boolean(true))?;
    facts.insert("service.nginx.active", ProbeFactValue::Boolean(true))?;
    facts.insert(
        "service.nginx.state",
        ProbeFactValue::Text("running".to_owned()),
    )?;
    facts.insert("package.nginx.installed", ProbeFactValue::Boolean(true))?;
    facts.insert(
        "package.nginx.version",
        ProbeFactValue::Text("1.24.0-2ubuntu7".to_owned()),
    )?;
    facts.insert(
        "file./etc/nginx/sites-available/default.exists",
        ProbeFactValue::Boolean(true),
    )?;
    facts.insert(
        "file./etc/nginx/sites-available/default.sha256",
        ProbeFactValue::Text("a".repeat(64)),
    )?;
    facts.insert(
        "file./etc/nginx/sites-available/default.mode",
        ProbeFactValue::Text("0644".to_owned()),
    )?;
    Ok(facts)
}

fn evidence(
    request: &AnsibleProbeExecutionRequest,
    facts: AnsibleProbeFacts,
    terminal_status: AnsibleProbeTerminalStatus,
) -> Result<AnsibleProbeEvidence, AnsibleProbeError> {
    let assertion_results = evaluate_assertions(&facts, &request.assertions);
    Ok(AnsibleProbeEvidence {
        schema_version: ANSIBLE_PROBE_EVIDENCE_SCHEMA_VERSION.to_owned(),
        run_id: request.run_id,
        step_run_id: request.step_run_id,
        attempt_id: request.attempt_id,
        trace_id: request.trace_id.clone(),
        request_sha256: request.request_sha256()?,
        evaluation_spec_sha256: request.evaluation_spec_sha256,
        playbook_profile: request.playbook_profile.clone(),
        runner_image_digest: request.runner_image_digest.clone(),
        terminal_status,
        diagnostic_code: terminal_status.diagnostic_code().to_owned(),
        facts,
        assertion_results,
        duration_milliseconds: 1_500,
        facts_bytes: 4_096,
        output_bytes: 1_024,
    })
}

fn error_diagnostic<T>(
    result: Result<T, AnsibleProbeError>,
) -> Result<&'static str, Box<dyn std::error::Error>> {
    match result {
        Ok(_) => Err("expected ansible probe validation failure".into()),
        Err(error) => Ok(error.diagnostic_code()),
    }
}

#[test]
fn valid_request_passes_and_evaluates_nginx_facts() -> Result<(), Box<dyn std::error::Error>> {
    let request = request();
    request.validate()?;
    let facts = nginx_facts()?;
    let results = evaluate_assertions(&facts, &request.assertions);
    assert!(
        results
            .iter()
            .all(|result| result.status == AnsibleProbeAssertionStatus::Passed && result.passed)
    );
    assert_eq!(
        AnsibleProbeTerminalStatus::for_assertions(&results),
        AnsibleProbeTerminalStatus::Succeeded
    );
    let evidence = evidence(&request, facts, AnsibleProbeTerminalStatus::Succeeded)?;
    evidence.validate_for(&request)?;
    let receipt = AnsibleProbeEvidenceReceipt {
        schema_version: ANSIBLE_PROBE_EVIDENCE_RECEIPT_SCHEMA_VERSION.to_owned(),
        run_id: evidence.run_id,
        step_run_id: evidence.step_run_id,
        attempt_id: evidence.attempt_id,
        trace_id: evidence.trace_id.clone(),
        request_sha256: evidence.request_sha256,
        evidence_sha256: digest(b"evidence"),
        evidence_size_bytes: 2_048,
        terminal_status: evidence.terminal_status,
        diagnostic_code: evidence.diagnostic_code.clone(),
        passed_assertions: 2,
        total_assertions: 2,
    };
    receipt.validate_for(&request)?;
    Ok(())
}

#[test]
fn assertion_failure_is_observed_and_terminal() -> Result<(), Box<dyn std::error::Error>> {
    let request = request();
    let mut facts = AnsibleProbeFacts::new();
    facts.insert("host.reachable", ProbeFactValue::Boolean(true))?;
    facts.insert("service.nginx.active", ProbeFactValue::Boolean(false))?;
    let results = evaluate_assertions(&facts, &request.assertions);
    assert_eq!(results[0].status, AnsibleProbeAssertionStatus::Passed);
    assert_eq!(results[1].status, AnsibleProbeAssertionStatus::Failed);
    assert_eq!(results[1].observed, Some(json!(false)));
    assert!(!results[1].passed);
    assert_eq!(results[1].diagnostic_code, "LW_AP_ASSERTION_FAILED");
    let evidence = evidence(
        &request,
        facts,
        AnsibleProbeTerminalStatus::AssertionsFailed,
    )?;
    evidence.validate_for(&request)?;

    let mut forged = evidence.clone();
    forged.terminal_status = AnsibleProbeTerminalStatus::Succeeded;
    forged.diagnostic_code = "LW_AP_SUCCEEDED".to_owned();
    assert_eq!(
        error_diagnostic(forged.validate_for(&request))?,
        "LW_AP_EVIDENCE_INVALID"
    );
    Ok(())
}

#[test]
fn unknown_fact_and_type_mismatch_fail_closed() -> Result<(), Box<dyn std::error::Error>> {
    let facts = nginx_facts()?;
    let unknown = evaluate_assertions(&facts, &[assertion("tcp.80.open", &json!(true))]);
    assert_eq!(unknown[0].status, AnsibleProbeAssertionStatus::FactUnknown);
    assert!(!unknown[0].passed);
    assert_eq!(unknown[0].observed, None);
    assert_eq!(unknown[0].diagnostic_code, "LW_AP_FACT_UNKNOWN");

    // evaluate_assertions is defensive: even a request-invalid expectation
    // (bool expected for a text fact) can never pass.
    let mismatched = evaluate_assertions(&facts, &[assertion("service.nginx.state", &json!(true))]);
    assert_eq!(
        mismatched[0].status,
        AnsibleProbeAssertionStatus::FactTypeMismatch
    );
    assert!(!mismatched[0].passed);
    assert_eq!(mismatched[0].diagnostic_code, "LW_AP_FACT_TYPE_MISMATCH");
    Ok(())
}

#[test]
fn request_rejects_non_allowlist_modules_and_writable_probes()
-> Result<(), Box<dyn std::error::Error>> {
    let mut value = request();
    value.module_allowlist = vec!["ansible.builtin.command".to_owned()];
    assert_eq!(
        error_diagnostic(value.validate())?,
        "LW_AP_MODULE_NOT_ALLOWED"
    );

    let mut value = request();
    value
        .module_allowlist
        .push("ansible.builtin.service_facts".to_owned());
    assert_eq!(
        error_diagnostic(value.validate())?,
        "LW_AP_MODULE_NOT_ALLOWED"
    );

    let mut value = request();
    value.module_allowlist.clear();
    assert_eq!(
        error_diagnostic(value.validate())?,
        "LW_AP_MODULE_NOT_ALLOWED"
    );

    let mut value = request();
    value.read_only = false;
    assert_eq!(
        error_diagnostic(value.validate())?,
        "LW_AP_READ_ONLY_REQUIRED"
    );
    Ok(())
}

#[test]
fn request_rejects_public_ip_loopback_non_ssh_port_and_root_user()
-> Result<(), Box<dyn std::error::Error>> {
    for host in [
        Ipv4Addr::new(8, 8, 8, 8),
        Ipv4Addr::new(127, 0, 0, 1),
        Ipv4Addr::new(169, 254, 1, 1),
    ] {
        let mut value = request();
        value.target.host = host;
        assert_eq!(
            error_diagnostic(value.validate())?,
            "LW_AP_TARGET_NOT_PRIVATE"
        );
    }
    for host in [
        Ipv4Addr::new(10, 0, 0, 8),
        Ipv4Addr::new(172, 16, 3, 4),
        Ipv4Addr::new(192, 168, 56, 10),
    ] {
        let mut value = request();
        value.target.host = host;
        value.validate()?;
    }

    let mut value = request();
    value.target.port = 2222;
    assert_eq!(error_diagnostic(value.validate())?, "LW_AP_PORT_INVALID");

    for username in ["root", "", "Admin", "lab user"] {
        let mut value = request();
        value.target.username = username.to_owned();
        assert_eq!(
            error_diagnostic(value.validate())?,
            "LW_AP_USERNAME_INVALID"
        );
    }
    Ok(())
}

#[test]
fn request_rejects_bad_profile_image_identity_assertions_limits_and_secrets()
-> Result<(), Box<dyn std::error::Error>> {
    let mut value = request();
    value.playbook_profile = "  ".to_owned();
    assert_eq!(error_diagnostic(value.validate())?, "LW_AP_PROFILE_INVALID");

    let mut value = request();
    value.runner_image_digest = "ansible-probe:latest".to_owned();
    assert_eq!(
        error_diagnostic(value.validate())?,
        "LW_AP_IMAGE_IDENTITY_INVALID"
    );

    let mut value = request();
    value.run_id = Uuid::nil();
    assert_eq!(
        error_diagnostic(value.validate())?,
        "LW_AP_IDENTITY_INVALID"
    );

    let mut value = request();
    value.schema_version = "evaluation.labweaver.io/ansible-probe-execution/v0".to_owned();
    assert_eq!(
        error_diagnostic(value.validate())?,
        "LW_AP_SCHEMA_VERSION_INVALID"
    );

    let mut value = request();
    value.assertions.clear();
    assert_eq!(
        error_diagnostic(value.validate())?,
        "LW_AP_ASSERTIONS_INVALID"
    );

    let mut value = request();
    value.assertions = vec![
        assertion("host.reachable", &json!(true)),
        assertion("host.reachable", &json!(false)),
    ];
    assert_eq!(
        error_diagnostic(value.validate())?,
        "LW_AP_ASSERTIONS_INVALID"
    );

    let mut value = request();
    value.assertions = vec![assertion("tcp.80.open", &json!(true))];
    assert_eq!(
        error_diagnostic(value.validate())?,
        "LW_AP_ASSERTIONS_INVALID"
    );

    // A text-family fact can never carry a boolean expectation.
    let mut value = request();
    value.assertions = vec![assertion("service.nginx.state", &json!(true))];
    assert_eq!(
        error_diagnostic(value.validate())?,
        "LW_AP_ASSERTIONS_INVALID"
    );

    let mut value = request();
    value.limits.wall_time_seconds = 0;
    assert_eq!(error_diagnostic(value.validate())?, "LW_AP_LIMIT_INVALID");

    let mut value = request();
    value.limits.facts_max_bytes = 5 * 1024 * 1024;
    assert_eq!(error_diagnostic(value.validate())?, "LW_AP_LIMIT_INVALID");

    let mut value = request();
    value.limits.max_assertions = 1;
    assert_eq!(error_diagnostic(value.validate())?, "LW_AP_LIMIT_INVALID");

    let mut value = request();
    value.ssh_identity.private_key_secret = "Probe_SSH_Key".to_owned();
    assert_eq!(
        error_diagnostic(value.validate())?,
        "LW_AP_SSH_IDENTITY_INVALID"
    );
    Ok(())
}

#[test]
fn facts_reject_malformed_names_types_content_duplicates_and_overflow()
-> Result<(), Box<dyn std::error::Error>> {
    let mut facts = AnsibleProbeFacts::new();
    for (name, value) in [
        ("tcp.80.open", ProbeFactValue::Boolean(true)),
        ("service..active", ProbeFactValue::Boolean(true)),
        ("service.nginx.enabled", ProbeFactValue::Boolean(true)),
        ("host.reachable", ProbeFactValue::Text("yes".to_owned())),
        ("service.nginx.state", ProbeFactValue::Boolean(true)),
        ("file.relative/path.exists", ProbeFactValue::Boolean(true)),
        ("file./etc/../shadow.exists", ProbeFactValue::Boolean(true)),
        (
            "file./etc/nginx/nginx.conf.sha256",
            ProbeFactValue::Text("not-a-digest".to_owned()),
        ),
        (
            "file./etc/nginx/nginx.conf.mode",
            ProbeFactValue::Text("888".to_owned()),
        ),
        (
            "package.nginx.version",
            ProbeFactValue::Text("v".repeat(300)),
        ),
    ] {
        assert_eq!(
            error_diagnostic(facts.insert(name, value))?,
            "LW_AP_FACTS_MALFORMED"
        );
    }

    facts.insert("host.reachable", ProbeFactValue::Boolean(true))?;
    assert_eq!(
        error_diagnostic(facts.insert("host.reachable", ProbeFactValue::Boolean(true)))?,
        "LW_AP_FACTS_MALFORMED"
    );

    let mut overflow = AnsibleProbeFacts::new();
    for index in 0..MAX_FACTS {
        overflow.insert(
            &format!("service.service-{index}.active"),
            ProbeFactValue::Boolean(true),
        )?;
    }
    assert_eq!(
        error_diagnostic(overflow.insert("service.extra.active", ProbeFactValue::Boolean(true)))?,
        "LW_AP_FACTS_MALFORMED"
    );
    Ok(())
}

#[test]
fn unreachable_terminal_requires_empty_facts_and_fail_closed_results()
-> Result<(), Box<dyn std::error::Error>> {
    let request = request();
    let evidence = evidence(
        &request,
        AnsibleProbeFacts::new(),
        AnsibleProbeTerminalStatus::HostUnreachable,
    )?;
    assert_eq!(evidence.diagnostic_code, "LW_AP_HOST_UNREACHABLE");
    assert!(
        evidence
            .assertion_results
            .iter()
            .all(|result| result.status == AnsibleProbeAssertionStatus::FactUnknown)
    );
    evidence.validate_for(&request)?;

    let mut forged = evidence.clone();
    forged
        .facts
        .insert("host.reachable", ProbeFactValue::Boolean(true))?;
    assert_eq!(
        error_diagnostic(forged.validate_for(&request))?,
        "LW_AP_EVIDENCE_INVALID"
    );

    assert_eq!(
        AnsibleProbeTerminalStatus::Timeout.diagnostic_code(),
        "LW_AP_TIMEOUT"
    );
    assert_eq!(
        AnsibleProbeTerminalStatus::IdentityExpired.diagnostic_code(),
        "LW_AP_IDENTITY_EXPIRED"
    );
    assert_eq!(
        AnsibleProbeTerminalStatus::OutputExceeded.diagnostic_code(),
        "LW_AP_OUTPUT_LIMIT"
    );
    assert_eq!(
        AnsibleProbeTerminalStatus::HostKeyMismatch.diagnostic_code(),
        "LW_AP_HOST_KEY_MISMATCH"
    );
    assert_eq!(
        AnsibleProbeTerminalStatus::GrantMissing.diagnostic_code(),
        "LW_AP_GRANT_MISSING"
    );
    Ok(())
}

#[test]
fn wire_roundtrip_and_canonical_identity_are_stable() -> Result<(), Box<dyn std::error::Error>> {
    let request = request();
    let json = serde_json::to_string(&request)?;
    let parsed: AnsibleProbeExecutionRequest = serde_json::from_str(&json)?;
    assert_eq!(parsed, request);
    assert_eq!(parsed.request_sha256()?, request.request_sha256()?);

    let evidence = evidence(
        &request,
        nginx_facts()?,
        AnsibleProbeTerminalStatus::Succeeded,
    )?;
    let json = serde_json::to_string(&evidence)?;
    let parsed: AnsibleProbeEvidence = serde_json::from_str(&json)?;
    assert_eq!(parsed, evidence);
    parsed.validate_for(&request)?;

    // RFC 8785 canonicalization is deterministic across serializations.
    let canonical = serde_jcs::to_string(&evidence)?;
    assert_eq!(canonical, serde_jcs::to_string(&evidence)?);
    assert_eq!(
        Sha256Digest::of_canonical(&evidence)?,
        Sha256Digest::of_canonical(&parsed)?
    );
    Ok(())
}

#[test]
fn strict_json_rejects_unknown_fields_and_bad_host_key_format()
-> Result<(), Box<dyn std::error::Error>> {
    let mut json = serde_json::to_value(request())?;
    json["unexpected"] = serde_json::json!(true);
    assert!(serde_json::from_value::<AnsibleProbeExecutionRequest>(json).is_err());

    let mut json = serde_json::to_value(request())?;
    json["sshIdentity"]["expectedHostKeySha256"] = serde_json::json!("zzzz");
    assert!(serde_json::from_value::<AnsibleProbeExecutionRequest>(json).is_err());
    Ok(())
}
