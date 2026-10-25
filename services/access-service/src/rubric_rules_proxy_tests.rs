//! Real BFF, persisted sessions/memberships and HTTPS forwarding to a capture boundary.
use super::*;

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn rubric_rules_bff_forwards_teacher_requests_and_denies_before_upstream()
-> Result<(), Box<dyn Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        ))
        .await?;
    support::apply_access_migrations(&pool).await?;
    let mut tasks = Tasks(Vec::new());
    let actor = Uuid::now_v7();
    let (state, requests) = Box::pin(state(pool, actor, &mut tasks)).await?;
    let teacher = session(
        &state.pool,
        &state.key_ring,
        actor,
        contracts::PlatformRole::Teacher,
    )
    .await?;
    let project = Uuid::now_v7();
    sqlx::query("INSERT INTO control.project_memberships(project_id,actor_id,role,object_role,state,revision) VALUES($1,$2,'teacher','editor','active',1)")
        .bind(project).bind(actor).execute(&state.pool).await?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let router = browser_router_on(Arc::clone(&state), listener.local_addr()?);
    tasks.0.push(tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    }));
    let client = reqwest::Client::builder().no_proxy().build()?;
    let request_body = json!({
        "evaluationReleaseId": Uuid::now_v7(), "evaluationReleaseRevision": 3,
        "rubric": {"packageId": Uuid::now_v7(),"packageRevision":2,"path":"rubrics/review.md",
            "artifact":{"artifactId":Uuid::now_v7(),"storeBinding":"rubric-fixture","objectVersion":"rubric-v2","sizeBytes":42,"mediaType":"text/markdown"},
            "sha256":"b".repeat(64)},
        "action":{"kind":"set_visibility","visibility":"course_public"}
    });
    let contract: contracts::http::MaintainRubricModelInputRuleRequest =
        contracts::parse_strict_json(&serde_json::to_vec(&request_body)?)?;
    contract.rubric.validate()?;
    let endpoint = |project| {
        format!("{base}/api/v1/projects/{project}/llm-egress-policies/active/rubric-rules")
    };
    let request = |method: Method, project, bff: &BffSession| {
        let mut request = client.request(method.clone(), endpoint(project)).header(
            header::COOKIE,
            format!(
                "{}={}",
                state.deployment.browser.session_cookie_name, bff.session_id
            ),
        );
        if method == Method::POST {
            request = request
                .header(header::ORIGIN, ORIGIN)
                .header("x-csrf-token", bff.csrf_token.expose())
                .header(header::IF_MATCH, "\"6\"")
                .header("idempotency-key", "rubric-declare-public")
                .json(&request_body);
        }
        request
    };
    for method in [Method::GET, Method::POST] {
        let response = request(method.clone(), project, &teacher)
            .header("x-labweaver-actor-id", Uuid::now_v7().to_string())
            .header("x-labweaver-session-id", Uuid::now_v7().to_string())
            .send()
            .await?;
        let status = response.status();
        let etag = response.headers().get(header::ETAG).cloned();
        let body = response.text().await?;
        assert_eq!(status, StatusCode::OK, "{method}: {body}");
        assert_eq!(
            etag.as_ref().and_then(|value| value.to_str().ok()),
            Some("\"7\"")
        );
        assert_eq!(
            serde_json::from_str::<Value>(&body)?["boundary"],
            "request_record_only"
        );
    }
    {
        let captured = requests.lock().await;
        assert_eq!(captured.len(), 2);
        for (record, method) in captured.iter().zip(["GET", "POST"]) {
            assert_eq!(record["method"], method);
            assert_eq!(
                record["path"],
                format!("/api/v1/projects/{project}/llm-egress-policies/active/rubric-rules")
            );
            assert_eq!(record["actor"], actor.to_string());
            assert_eq!(record["session"], teacher.session_id.to_string());
            assert_eq!(record["browserCookie"], false);
            assert_eq!(record["browserCsrf"], false);
        }
        assert_eq!(captured[1]["body"], request_body);
        assert_eq!(captured[1]["ifMatch"], "\"6\"");
        assert_eq!(captured[1]["idempotencyKey"], "rubric-declare-public");
    }
    let forwarded = requests.lock().await.len();
    for role in [
        contracts::PlatformRole::Student,
        contracts::PlatformRole::PlatformAdmin,
    ] {
        let member = Uuid::now_v7();
        let bff = session(&state.pool, &state.key_ring, member, role).await?;
        sqlx::query("INSERT INTO control.project_memberships(project_id,actor_id,role,object_role,state,revision) VALUES($1,$2,'teacher','editor','active',1)")
            .bind(project).bind(member).execute(&state.pool).await?;
        for method in [Method::GET, Method::POST] {
            assert_problem_response(
                request(method, project, &bff).send().await?,
                StatusCode::FORBIDDEN,
                "LW_AUTH_SCOPE_DENIED",
            )
            .await?;
            assert_eq!(requests.lock().await.len(), forwarded);
        }
    }
    for method in [Method::GET, Method::POST] {
        assert_problem_response(
            request(method, Uuid::now_v7(), &teacher).send().await?,
            StatusCode::FORBIDDEN,
            "LW_AUTH_SCOPE_DENIED",
        )
        .await?;
        assert_eq!(requests.lock().await.len(), forwarded);
    }
    sqlx::query("UPDATE control.project_memberships SET object_role='viewer' WHERE project_id=$1 AND actor_id=$2")
        .bind(project).bind(actor).execute(&state.pool).await?;
    assert_problem_response(
        request(Method::POST, project, &teacher).send().await?,
        StatusCode::FORBIDDEN,
        "LW_AUTH_SCOPE_DENIED",
    )
    .await?;
    assert_eq!(requests.lock().await.len(), forwarded);
    assert_eq!(
        request(Method::GET, project, &teacher)
            .send()
            .await?
            .status(),
        StatusCode::OK
    );
    let forwarded = requests.lock().await.len();
    sqlx::query("UPDATE control.project_memberships SET object_role='editor',state='revoked' WHERE project_id=$1 AND actor_id=$2")
        .bind(project).bind(actor).execute(&state.pool).await?;
    for method in [Method::GET, Method::POST] {
        assert_problem_response(
            request(method, project, &teacher).send().await?,
            StatusCode::FORBIDDEN,
            "LW_AUTH_SCOPE_DENIED",
        )
        .await?;
        assert_eq!(requests.lock().await.len(), forwarded);
    }
    sqlx::query(
        "UPDATE control.project_memberships SET state='active' WHERE project_id=$1 AND actor_id=$2",
    )
    .bind(project)
    .bind(actor)
    .execute(&state.pool)
    .await?;
    for (origin, csrf) in [
        (None, Some(teacher.csrf_token.expose())),
        (
            Some("https://other.example.invalid"),
            Some(teacher.csrf_token.expose()),
        ),
        (Some(ORIGIN), None),
        (Some(ORIGIN), Some("wrong")),
    ] {
        let mut denied = request(Method::POST, project, &teacher).build()?;
        denied.headers_mut().remove(header::ORIGIN);
        denied.headers_mut().remove("x-csrf-token");
        if let Some(origin) = origin {
            denied.headers_mut().insert(header::ORIGIN, origin.parse()?);
        }
        if let Some(csrf) = csrf {
            denied.headers_mut().insert("x-csrf-token", csrf.parse()?);
        }
        assert_eq!(
            client.execute(denied).await?.status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(requests.lock().await.len(), forwarded);
    }
    for method in [Method::GET, Method::POST] {
        assert_eq!(
            client
                .request(method, endpoint(project))
                .send()
                .await?
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(requests.lock().await.len(), forwarded);
    }
    sqlx::query("UPDATE access.bff_sessions SET revoked_at=now() WHERE session_id=$1")
        .bind(teacher.session_id)
        .execute(&state.pool)
        .await?;
    assert_eq!(
        request(Method::GET, project, &teacher)
            .send()
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(requests.lock().await.len(), forwarded);
    assert_eq!(
        client.put(endpoint(project)).send().await?.status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
    assert_eq!(requests.lock().await.len(), forwarded);
    Ok(())
}
