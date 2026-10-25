//! Real BFF persistence and HTTP forwarding; only the external upstream is captured.
use super::*;

const CONTEXT_HEADER: &str = "x-labweaver-model-input-authorization-context";

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn freeze_context_is_server_derived_and_denied_requests_never_forward()
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
    let a = session(
        &state.pool,
        &state.key_ring,
        actor,
        contracts::PlatformRole::Teacher,
    )
    .await?;
    let project = Uuid::now_v7();
    let course = Uuid::now_v7();
    let environment = Uuid::now_v7();
    let grant = Uuid::now_v7();
    let now = OffsetDateTime::now_utc();
    let membership_expiry = utc_timestamp(now + Duration::minutes(2))
        .map_err(|e| io_error(e.diagnostic))?
        .get();
    sqlx::query("INSERT INTO control.project_memberships (course_id,project_id,actor_id,role,object_role,state,revision,expires_at) VALUES (NULL,$1,$2,'teacher','editor','active',1,$3)")
        .bind(project).bind(actor).bind(membership_expiry).execute(&state.pool).await?;
    sqlx::query("INSERT INTO control.course_memberships (course_id,actor_id,role,object_role,state,revision,expires_at) VALUES ($1,$2,'teacher','instructor','active',1,$3)")
        .bind(course).bind(actor).bind(membership_expiry).execute(&state.pool).await?;
    sqlx::query("INSERT INTO access.access_grants (grant_id,actor_id,project_id,course_id,environment_id,revision,state,not_before,expires_at,contract) VALUES ($1,$2,$3,$4,$5,1,'active',$6,$7,'{}')")
        .bind(grant).bind(actor).bind(project).bind(course).bind(environment)
        .bind(now - Duration::minutes(1)).bind(now + Duration::minutes(3)).execute(&state.pool).await?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let router = browser_router_on(Arc::clone(&state), listener.local_addr()?);
    tasks.0.push(tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    }));
    let client = reqwest::Client::builder().no_proxy().build()?;
    let body = json!({"courseId": course, "manifest": {
        "apiVersion":"evaluation.labweaver.io/v1", "kind":"SubmissionManifest", "name":"workspace", "source":"workspace",
        "include":[{"kind":"directoryTree","path":"src"}], "exclude":[], "required":[], "llmReadable":[],
        "maxTotalBytes":1_048_576, "maxFiles":100, "followSymlinks":false
    }});
    let make_request = |environment: Uuid, session: &BffSession, body: &Value| {
        client
            .post(format!("{base}/api/v1/environments/{environment}/freeze"))
            .header(
                header::COOKIE,
                format!(
                    "{}={}",
                    state.deployment.browser.session_cookie_name, session.session_id
                ),
            )
            .header(header::ORIGIN, ORIGIN)
            .header("x-csrf-token", session.csrf_token.expose())
            .header("idempotency-key", "stable-freeze-intent")
            .json(body)
    };
    for forged in [
        json!({"sessionSha256":"f".repeat(64), "actorId":Uuid::now_v7(),"projectId":Uuid::now_v7(),"courseId":Uuid::now_v7(),"operationId":"createAgentRun","validUntil":"2099-01-01T00:00:00.000Z"}).to_string(),
        "{".to_owned(), "x".repeat(4096),
    ] {
        let response = make_request(environment, &a, &body)
            .header(CONTEXT_HEADER, &forged).header(CONTEXT_HEADER, &forged)
            .header("x-labweaver-actor-id", Uuid::now_v7().to_string())
            .header("x-labweaver-session-id", Uuid::now_v7().to_string())
            .header("x-labweaver-project-id", Uuid::now_v7().to_string())
            .header("x-labweaver-course-id", Uuid::now_v7().to_string()).send().await?;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let records = requests.lock().await;
        let record = records.last().ok_or("no upstream freeze")?;
        assert_eq!(record["modelContextCount"], 1);
        let context: contracts::http::ModelInputAuthorizationContext = serde_json::from_value(record["modelContext"].clone())?;
        context.validate()?;
        assert_eq!(context.session_sha256, Sha256Digest::of_bytes(a.session_id.to_string().as_bytes()).to_string());
        assert_eq!(context.actor_id.to_string(), actor.to_string());
        assert_eq!(context.project_id.to_string(), project.to_string());
        assert_eq!(context.course_id.map(|id| id.to_string()), Some(course.to_string()));
        assert_eq!(context.operation_id, "freezeSubmission");
        assert!(context.valid_until.get() <= membership_expiry);
        assert!(context.valid_until.get() <= a.expires_at);
        assert_eq!(record["actor"], actor.to_string());
        assert_eq!(record["session"], a.session_id.to_string());
        assert_eq!(record["project"], project.to_string());
        assert_eq!(record["course"], course.to_string());
        assert_eq!(record["browserCookie"], false);
        assert_eq!(record["browserCsrf"], false);
    }
    let b = create_bff_session(
        &state.pool,
        &state.key_ring,
        CreateBffSession {
            actor_id: actor,
            roles: vec![contracts::PlatformRole::Teacher],
            authorization_revision: 1,
            expires_at: now + Duration::minutes(10),
            idle_ttl: Duration::minutes(5),
            oidc_sid: None,
            logout_hint: fixture_logout_hint(),
        },
        OffsetDateTime::now_utc(),
    )
    .await?;
    let response = make_request(environment, &b, &body).send().await?;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(
        requests
            .lock()
            .await
            .last()
            .ok_or("missing replay capture")?["modelContext"]["sessionSha256"],
        Sha256Digest::of_bytes(b.session_id.to_string().as_bytes()).to_string()
    );
    let expected = requests.lock().await.len();
    let mut wrong_course = body.clone();
    wrong_course["courseId"] = json!(Uuid::now_v7());
    for request in [
        make_request(Uuid::now_v7(), &a, &body),
        make_request(environment, &a, &wrong_course),
    ] {
        assert_eq!(request.send().await?.status(), StatusCode::FORBIDDEN);
    }
    let mut rejected_csrf = make_request(environment, &a, &body).build()?;
    rejected_csrf
        .headers_mut()
        .insert("x-csrf-token", "wrong".parse()?);
    assert_eq!(
        client.execute(rejected_csrf).await?.status(),
        StatusCode::FORBIDDEN
    );
    for (column, value) in [("object_role", "viewer"), ("state", "revoked")] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE control.project_memberships SET {column}=$1 WHERE project_id=$2 AND actor_id=$3"
        )))
        .bind(value)
        .bind(project)
        .bind(actor)
        .execute(&state.pool)
        .await?;
        assert_eq!(
            make_request(environment, &a, &body).send().await?.status(),
            StatusCode::FORBIDDEN
        );
        sqlx::query("UPDATE control.project_memberships SET object_role='editor',state='active' WHERE project_id=$1 AND actor_id=$2")
            .bind(project).bind(actor).execute(&state.pool).await?;
    }
    sqlx::query("UPDATE control.course_memberships SET state='revoked' WHERE course_id=$1")
        .bind(course)
        .execute(&state.pool)
        .await?;
    assert_eq!(
        make_request(environment, &a, &body).send().await?.status(),
        StatusCode::FORBIDDEN
    );
    sqlx::query("UPDATE control.course_memberships SET state='active' WHERE course_id=$1")
        .bind(course)
        .execute(&state.pool)
        .await?;
    sqlx::query("UPDATE access.access_grants SET state='revoked',revoked_at=now(),reason_code='fixture_revoke' WHERE grant_id=$1")
        .bind(grant)
        .execute(&state.pool)
        .await?;
    assert_eq!(
        make_request(environment, &a, &body).send().await?.status(),
        StatusCode::FORBIDDEN
    );
    sqlx::query("UPDATE access.access_grants SET state='active',revoked_at=NULL,reason_code=NULL WHERE grant_id=$1")
        .bind(grant)
        .execute(&state.pool)
        .await?;
    for column in ["expires_at", "idle_expires_at", "revoked_at"] {
        // Use separate persisted sessions; expiry/idle are never rounded up by the producer.
        let candidate = create_bff_session(
            &state.pool,
            &state.key_ring,
            CreateBffSession {
                actor_id: actor,
                roles: vec![contracts::PlatformRole::Teacher],
                authorization_revision: 1,
                expires_at: now + Duration::minutes(10),
                idle_ttl: Duration::minutes(5),
                oidc_sid: None,
                logout_hint: fixture_logout_hint(),
            },
            OffsetDateTime::now_utc(),
        )
        .await?;
        let expiry_assignment = if column == "expires_at" {
            "expires_at=now()-interval '1 microsecond',idle_expires_at=now()-interval '1 microsecond'".to_owned()
        } else {
            format!("{column}=now()-interval '1 microsecond'")
        };
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE access.bff_sessions SET {expiry_assignment} WHERE session_id=$1"
        )))
        .bind(candidate.session_id)
        .execute(&state.pool)
        .await?;
        assert_eq!(
            make_request(environment, &candidate, &body)
                .send()
                .await?
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        requests.lock().await.len(),
        expected,
        "denied requests must not reach freeze upstream"
    );
    Ok(())
}

fn io_error(message: &str) -> std::io::Error {
    std::io::Error::other(message)
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn freeze_rejects_idle_expiry_while_absolute_session_expiry_remains_valid()
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
    let session = session(
        &state.pool,
        &state.key_ring,
        actor,
        contracts::PlatformRole::Teacher,
    )
    .await?;
    let project = Uuid::now_v7();
    let course = Uuid::now_v7();
    let environment = Uuid::now_v7();
    let now = OffsetDateTime::now_utc();
    sqlx::query("INSERT INTO control.project_memberships (course_id,project_id,actor_id,role,object_role,state,revision) VALUES (NULL,$1,$2,'teacher','editor','active',1)")
        .bind(project).bind(actor).execute(&state.pool).await?;
    sqlx::query("INSERT INTO control.course_memberships (course_id,actor_id,role,object_role,state,revision) VALUES ($1,$2,'teacher','instructor','active',1)")
        .bind(course).bind(actor).execute(&state.pool).await?;
    sqlx::query("INSERT INTO access.access_grants (grant_id,actor_id,project_id,course_id,environment_id,revision,state,not_before,expires_at,contract) VALUES ($1,$2,$3,$4,$5,1,'active',$6,$7,'{}')")
        .bind(Uuid::now_v7()).bind(actor).bind(project).bind(course).bind(environment)
        .bind(now - Duration::minutes(1)).bind(now + Duration::minutes(5)).execute(&state.pool).await?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let router = browser_router_on(Arc::clone(&state), listener.local_addr()?);
    tasks.0.push(tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    }));
    let client = reqwest::Client::builder().no_proxy().build()?;
    let body = json!({"courseId": course, "manifest": {
        "apiVersion":"evaluation.labweaver.io/v1", "kind":"SubmissionManifest", "name":"workspace", "source":"workspace",
        "include":[{"kind":"directoryTree","path":"src"}], "exclude":[], "required":[], "llmReadable":[],
        "maxTotalBytes":1_048_576, "maxFiles":100, "followSymlinks":false
    }});
    let post = || {
        client
            .post(format!("{base}/api/v1/environments/{environment}/freeze"))
            .header(
                header::COOKIE,
                format!(
                    "{}={}",
                    state.deployment.browser.session_cookie_name, session.session_id
                ),
            )
            .header(header::ORIGIN, ORIGIN)
            .header("x-csrf-token", session.csrf_token.expose())
            .header("idempotency-key", "idle-expiry-freeze-intent")
            .json(&body)
    };
    assert_eq!(post().send().await?.status(), StatusCode::ACCEPTED);
    let accepted = requests.lock().await.len();
    assert_eq!(accepted, 1, "the authorized request must forward");
    sqlx::query("UPDATE access.bff_sessions SET idle_expires_at=clock_timestamp()-interval '1 microsecond' WHERE session_id=$1")
        .bind(session.session_id).execute(&state.pool).await?;
    let (absolute_valid, idle_expired, not_revoked): (bool, bool, bool) = sqlx::query_as(
        "SELECT expires_at>clock_timestamp(),idle_expires_at<=clock_timestamp(),revoked_at IS NULL FROM access.bff_sessions WHERE session_id=$1",
    ).bind(session.session_id).fetch_one(&state.pool).await?;
    assert!(absolute_valid, "absolute expiry must remain in the future");
    assert!(idle_expired);
    assert!(not_revoked);
    assert_eq!(post().send().await?.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        requests.lock().await.len(),
        accepted,
        "idle expiry must stop forwarding while absolute expiry remains valid"
    );
    Ok(())
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn freeze_context_uses_persisted_earliest_expiries_and_microsecond_idle()
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
    let (initial, records) = Box::pin(state(pool, actor, &mut tasks)).await?;
    let mut configured = (*initial).clone();
    configured
        .deployment
        .environment_owner_resolver
        .decision_ttl_seconds = 60;
    let state = Arc::new(configured);
    let session = session(
        &state.pool,
        &state.key_ring,
        actor,
        contracts::PlatformRole::Teacher,
    )
    .await?;
    let project = Uuid::now_v7();
    let course = Uuid::now_v7();
    let environment = Uuid::now_v7();
    let grant = Uuid::now_v7();
    sqlx::query("INSERT INTO control.project_memberships (course_id,project_id,actor_id,role,object_role,state,revision) VALUES (NULL,$1,$2,'teacher','editor','active',1)")
        .bind(project).bind(actor).execute(&state.pool).await?;
    sqlx::query("INSERT INTO control.course_memberships (course_id,actor_id,role,object_role,state,revision) VALUES ($1,$2,'teacher','instructor','active',1)")
        .bind(course).bind(actor).execute(&state.pool).await?;
    let now = OffsetDateTime::now_utc();
    sqlx::query("INSERT INTO access.access_grants (grant_id,actor_id,project_id,course_id,environment_id,revision,state,not_before,expires_at,contract) VALUES ($1,$2,$3,$4,$5,1,'active',$6,$7,'{}')")
        .bind(grant).bind(actor).bind(project).bind(course).bind(environment).bind(now-Duration::minutes(1)).bind(now+Duration::minutes(5)).execute(&state.pool).await?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let router = browser_router_on(Arc::clone(&state), listener.local_addr()?);
    tasks.0.push(tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    }));
    let client = reqwest::Client::builder().no_proxy().build()?;
    let body = json!({"courseId":course,"manifest":{"apiVersion":"evaluation.labweaver.io/v1","kind":"SubmissionManifest","name":"workspace","source":"workspace","include":[{"kind":"directoryTree","path":"src"}],"exclude":[],"required":[],"llmReadable":[],"maxTotalBytes":1_048_576,"maxFiles":100,"followSymlinks":false}});
    let post = |session: &BffSession| {
        client
            .post(format!("{base}/api/v1/environments/{environment}/freeze"))
            .header(
                header::COOKIE,
                format!(
                    "{}={}",
                    state.deployment.browser.session_cookie_name, session.session_id
                ),
            )
            .header(header::ORIGIN, ORIGIN)
            .header("x-csrf-token", session.csrf_token.expose())
            .header("idempotency-key", Uuid::now_v7().to_string())
            .json(&body)
    };
    for (kind, seconds, micros) in [
        ("project", 30, 123_000),
        ("course", 25, 456_000),
        ("grant", 20, 789_123),
    ] {
        let bound =
            OffsetDateTime::now_utc() + Duration::seconds(seconds) + Duration::microseconds(micros);
        let wire_bound = utc_timestamp(bound).map_err(|e| io_error(e.diagnostic))?;
        sqlx::query("UPDATE control.project_memberships SET expires_at=$1 WHERE project_id=$2")
            .bind(if kind == "project" {
                Some(wire_bound.get())
            } else {
                None
            })
            .bind(project)
            .execute(&state.pool)
            .await?;
        sqlx::query("UPDATE control.course_memberships SET expires_at=$1 WHERE course_id=$2")
            .bind(if kind == "course" {
                Some(wire_bound.get())
            } else {
                None
            })
            .bind(course)
            .execute(&state.pool)
            .await?;
        sqlx::query("UPDATE access.access_grants SET expires_at=$1 WHERE grant_id=$2")
            .bind(if kind == "grant" {
                bound
            } else {
                now + Duration::minutes(5)
            })
            .bind(grant)
            .execute(&state.pool)
            .await?;
        assert_eq!(post(&session).send().await?.status(), StatusCode::ACCEPTED);
        let value = records
            .lock()
            .await
            .last()
            .ok_or("missing expiry capture")?["modelContext"]
            .clone();
        let context: contracts::http::ModelInputAuthorizationContext =
            serde_json::from_value(value)?;
        assert_eq!(
            context.valid_until, wire_bound,
            "earliest persisted {kind} expiry"
        );
        assert!(context.valid_until.get() <= bound);
    }
    sqlx::query("UPDATE access.access_grants SET expires_at=$1 WHERE grant_id=$2")
        .bind(now + Duration::minutes(5))
        .bind(grant)
        .execute(&state.pool)
        .await?;
    let idle_bound =
        OffsetDateTime::now_utc() + Duration::seconds(15) + Duration::microseconds(123_456);
    let short = create_bff_session(
        &state.pool,
        &state.key_ring,
        CreateBffSession {
            actor_id: actor,
            roles: vec![contracts::PlatformRole::Teacher],
            authorization_revision: 1,
            expires_at: idle_bound,
            idle_ttl: Duration::minutes(5),
            oidc_sid: None,
            logout_hint: fixture_logout_hint(),
        },
        OffsetDateTime::now_utc(),
    )
    .await?;
    assert_eq!(post(&short).send().await?.status(), StatusCode::ACCEPTED);
    let persisted_idle: OffsetDateTime =
        sqlx::query_scalar("SELECT idle_expires_at FROM access.bff_sessions WHERE session_id=$1")
            .bind(short.session_id)
            .fetch_one(&state.pool)
            .await?;
    // PostgreSQL stores microseconds; transport can only truncate that actual value.
    assert_eq!(
        persisted_idle.unix_timestamp_nanos() / 1_000,
        idle_bound.unix_timestamp_nanos() / 1_000
    );
    let value = records.lock().await.last().ok_or("missing idle capture")?["modelContext"].clone();
    let context: contracts::http::ModelInputAuthorizationContext = serde_json::from_value(value)?;
    assert_eq!(
        context.valid_until,
        utc_timestamp(persisted_idle).map_err(|e| io_error(e.diagnostic))?
    );
    assert!(context.valid_until.get() <= persisted_idle);
    Ok(())
}
