//! Transport-boundary coverage only; the Evaluation task authority is verified by its owner.
use super::*;

#[tokio::test]
async fn parent_task_denial_never_falls_back_to_unfenced_freeze_metadata()
-> Result<(), Box<dyn std::error::Error>> {
    let (ca_pem, leaf_pem, leaf_key_pem, jwk) = tls_material()?;
    let directory = tempfile::tempdir()?;
    let ca_path = directory.path().join("ca.pem");
    std::fs::write(&ca_path, ca_pem)?;
    let authority = spawn_authority(jwk).await?;
    let calls = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let capture = calls.clone();
    let app = Router::new().fallback(move |request: Request<Body>| {
        let capture = capture.clone();
        async move {
            let scoped = request.uri().path().contains("/tasks/");
            capture
                .lock()
                .await
                .push((request.uri().to_string(), request.headers().clone()));
            // An old, unfenced endpoint would accept. A denied task must not retry there.
            (
                if scoped {
                    StatusCode::FORBIDDEN
                } else {
                    StatusCode::OK
                },
                Json(json!({})),
            )
        }
    });
    let server = spawn_tls_service(app, &leaf_pem, &leaf_key_pem).await?;
    let token = Arc::new(service_token_client(&authority.issuer).await?);
    let client = control_service::clients::EvaluationClient::new_authenticated(
        service_config(&server.base_url, &ca_path),
        token,
    )?;
    let request = contracts::http::CreateFrozenModelInputAuthorizationRequest {
        input_authorization_context: contracts::http::ModelInputAuthorizationContext {
            session_sha256: "a".repeat(64),
            actor_id: ActorId::new(),
            project_id: ProjectId::new(),
            course_id: Some(CourseId::new()),
            operation_id: "freezeSubmission".to_owned(),
            valid_until: "2099-01-01T00:00:00.000Z".parse()?,
        },
        task_run_id: contracts::TaskRunId::new(),
        parent_lease_sha256: contracts::http::model_input_parent_lease_sha256(uuid::Uuid::now_v7()),
        frozen_submission_id: contracts::FrozenSubmissionId::new(),
        evaluation_release_id: contracts::EvaluationReleaseId::new(),
        evaluation_release_revision: Revision::new(7)?,
        selected_paths: vec!["submission.md".to_owned()],
    };
    assert!(client.frozen_model_input_facts(&request).await.is_err());
    let captured = calls.lock().await;
    assert_eq!(captured.len(), 1);
    assert_eq!(
        captured[0].0,
        format!(
            "/internal/v1/evaluation-releases/{}/frozen-submissions/{}/model-input-facts/tasks/{}",
            request.evaluation_release_id, request.frozen_submission_id, request.task_run_id
        )
    );
    let headers = &captured[0].1;
    assert_eq!(
        headers["x-labweaver-evaluation-lease-sha256"],
        request.parent_lease_sha256
    );
    assert_eq!(
        headers["x-labweaver-project-id"],
        request.input_authorization_context.project_id.to_string()
    );
    assert_eq!(
        headers["x-labweaver-actor-id"],
        request.input_authorization_context.actor_id.to_string()
    );
    assert_eq!(headers["if-match"], "\"rev-7\"");
    drop(captured);
    let mut invalid = request;
    invalid.parent_lease_sha256.clear();
    assert!(client.frozen_model_input_facts(&invalid).await.is_err());
    assert_eq!(calls.lock().await.len(), 1);
    Ok(())
}
