//! Real `PostgreSQL` evidence for Access schema constraints and token storage.

use sqlx::postgres::PgPoolOptions;
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one migrated PostgreSQL instance proves interacting schema, fencing, and authorization invariants"
)]
async fn access_schema_enforces_unique_keys_single_live_grant_and_hashed_tokens()
-> Result<(), Box<dyn std::error::Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        container.get_host_port_ipv4(5432).await?
    );
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await?;
    let migrations = format!(
        "CREATE SCHEMA access; SET search_path TO access;\n{}\n{}",
        include_str!("../../../migrations/access/0001_sprint2_baseline.sql"),
        include_str!("../../../migrations/access/0002_console_capabilities_and_sessions.sql")
    );
    sqlx::raw_sql(&migrations).execute(&pool).await?;

    let actor = Uuid::now_v7();
    sqlx::query("INSERT INTO access.actors (actor_id,issuer,subject_sha256) VALUES ($1,'https://issuer.example.test','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa')")
        .bind(actor).execute(&pool).await?;
    let fingerprint = "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    let key_id = Uuid::now_v7();
    sqlx::query("INSERT INTO access.ssh_public_keys (key_id,actor_id,fingerprint_sha256,algorithm,normalized_openssh) VALUES ($1,$2,$3,'ed25519','ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA')")
        .bind(key_id).bind(actor).bind(fingerprint).execute(&pool).await?;
    assert!(sqlx::query("INSERT INTO access.ssh_public_keys (key_id,actor_id,fingerprint_sha256,algorithm,normalized_openssh) VALUES ($1,$2,$3,'ed25519','ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA')")
        .bind(Uuid::now_v7()).bind(actor).bind(fingerprint).execute(&pool).await.is_err());

    let course = Uuid::now_v7();
    let environment = Uuid::now_v7();
    let grant_id = Uuid::now_v7();
    let contract = serde_json::json!({"request": {}});
    sqlx::query("INSERT INTO access.access_grants (grant_id,actor_id,course_id,environment_id,revision,state,not_before,expires_at,contract) VALUES ($1,$2,$3,$4,1,'requested',now(),now()+interval '30 minutes',$5)")
        .bind(grant_id).bind(actor).bind(course).bind(environment).bind(&contract).execute(&pool).await?;
    assert!(sqlx::query("INSERT INTO access.access_grants (grant_id,actor_id,course_id,environment_id,revision,state,not_before,expires_at,contract) VALUES ($1,$2,$3,$4,1,'active',now(),now()+interval '30 minutes',$5)")
        .bind(Uuid::now_v7()).bind(actor).bind(course).bind(environment).bind(&contract).execute(&pool).await.is_err());

    let token_columns: Vec<String> = sqlx::query_scalar(
        "SELECT column_name FROM information_schema.columns WHERE table_schema='access' AND table_name='ssh_authorizations' ORDER BY column_name"
    ).fetch_all(&pool).await?;
    assert!(token_columns.contains(&"token_sha256".to_owned()));
    assert!(!token_columns.contains(&"token".to_owned()));
    assert!(!token_columns.contains(&"force_command_token".to_owned()));

    let endpoint_grant_id = Uuid::now_v7();
    sqlx::query("INSERT INTO access.endpoint_grants (endpoint_grant_id,grant_id,endpoint_id,endpoint_revision,protocol,health,alias,expires_at,contract) VALUES ($1,$2,$3,1,'ssh','healthy','lw-abcdefghijklmnopqrst',now()+interval '30 minutes','{}')")
        .bind(endpoint_grant_id).bind(grant_id).bind(Uuid::now_v7()).execute(&pool).await?;
    let authorization_id = Uuid::now_v7();
    sqlx::query("INSERT INTO access.ssh_authorizations (authorization_id,token_sha256,actor_id,key_id,gateway_identity,connection_id,source_address_sha256,issued_at,expires_at) VALUES ($1,$2,$3,$4,'spiffe://labweaver/gateway','connection-1',$5,now(),now()+interval '30 seconds')")
        .bind(authorization_id)
        .bind("b".repeat(64))
        .bind(actor)
        .bind(key_id)
        .bind("c".repeat(64))
        .execute(&pool).await?;
    let session_id = Uuid::now_v7();
    let first = sqlx::query("UPDATE access.ssh_authorizations SET consumed_at=now(),session_id=$2 WHERE authorization_id=$1 AND consumed_at IS NULL")
        .bind(authorization_id).bind(session_id).execute(&pool).await?.rows_affected();
    let replay = sqlx::query("UPDATE access.ssh_authorizations SET consumed_at=now(),session_id=$2 WHERE authorization_id=$1 AND consumed_at IS NULL")
        .bind(authorization_id).bind(Uuid::now_v7()).execute(&pool).await?.rows_affected();
    assert_eq!(first, 1);
    assert_eq!(replay, 0);

    assert!(sqlx::query("INSERT INTO access.gateway_sessions (session_id,grant_id,grant_revision,actor_id,endpoint_id,state,started_at,expires_at,contract) VALUES ($1,$2,1,$3,$4,'active',now(),now()+interval '5 minutes','{}')")
        .bind(Uuid::now_v7()).bind(grant_id).bind(actor).bind(Uuid::now_v7()).execute(&pool).await.is_err());

    let idempotency_key = "same-client-key";
    for scope in [
        "actor:first",
        "actor:second",
        "service:gateway-a",
        "service:gateway-b",
    ] {
        sqlx::query("INSERT INTO access.idempotency_ledger (operation,scope_id,idempotency_key,request_sha256,state,result,completed_at) VALUES ('create_access_grant',$1,$2,$3,'completed','{}',now())")
            .bind(scope).bind(idempotency_key).bind("d".repeat(64)).execute(&pool).await?;
    }
    assert!(sqlx::query("INSERT INTO access.idempotency_ledger (operation,scope_id,idempotency_key,request_sha256,state) VALUES ('create_access_grant','actor:first',$1,$2,'in_progress')")
        .bind(idempotency_key).bind("e".repeat(64)).execute(&pool).await.is_err());
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM access.idempotency_ledger WHERE operation='create_access_grant' AND idempotency_key=$1")
        .bind(idempotency_key).fetch_one(&pool).await?, 4);

    let stale_token = Uuid::now_v7();
    let current_token = Uuid::now_v7();
    sqlx::query("INSERT INTO access.access_grant_activation_jobs (grant_id,state,lease_owner,lease_token,lease_expires_at) VALUES ($1,'leased','worker-stale',$2,now()-interval '1 second')")
        .bind(grant_id).bind(stale_token).execute(&pool).await?;
    let claimed: Uuid = sqlx::query_scalar(
        "WITH candidate AS (SELECT grant_id FROM access.access_grant_activation_jobs WHERE (state IN ('pending','retry') AND next_attempt_at<=now()) OR (state='leased' AND lease_expires_at<=now()) ORDER BY next_attempt_at,grant_id FOR UPDATE SKIP LOCKED LIMIT 1) UPDATE access.access_grant_activation_jobs j SET state='leased',lease_owner='worker-current',lease_token=$1,lease_expires_at=now()+interval '30 seconds',updated_at=now() FROM candidate WHERE j.grant_id=candidate.grant_id RETURNING j.grant_id",
    ).bind(current_token).fetch_one(&pool).await?;
    assert_eq!(claimed, grant_id);
    let stale_rows = sqlx::query("UPDATE access.access_grant_activation_jobs SET state='completed',lease_owner=NULL,lease_token=NULL,lease_expires_at=NULL WHERE grant_id=$1 AND state='leased' AND lease_token=$2 AND lease_expires_at>now()")
        .bind(grant_id).bind(stale_token).execute(&pool).await?.rows_affected();
    assert_eq!(stale_rows, 0);
    let stale_transition_rows = sqlx::query("UPDATE access.access_grants g SET state='denied',reason_code='LW_ACCESS_ACTIVATION_LEASE_LOST' WHERE g.grant_id=$1 AND EXISTS (SELECT 1 FROM access.access_grant_activation_jobs j WHERE j.grant_id=g.grant_id AND j.state='leased' AND j.lease_token=$2 AND j.lease_expires_at>now())")
        .bind(grant_id).bind(stale_token).execute(&pool).await?.rows_affected();
    assert_eq!(stale_transition_rows, 0);
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM access.access_grants WHERE grant_id=$1")
            .bind(grant_id)
            .fetch_one(&pool)
            .await?,
        "requested"
    );
    let current_rows = sqlx::query("UPDATE access.access_grant_activation_jobs SET state='completed',lease_owner=NULL,lease_token=NULL,lease_expires_at=NULL WHERE grant_id=$1 AND state='leased' AND lease_token=$2 AND lease_expires_at>now()")
        .bind(grant_id).bind(current_token).execute(&pool).await?.rows_affected();
    assert_eq!(current_rows, 1);

    sqlx::query("INSERT INTO access.course_memberships (course_id,actor_id,role,state,revision,expires_at) VALUES ($1,$2,'student','active',1,now()+interval '30 minutes')")
        .bind(course).bind(actor).execute(&pool).await?;
    sqlx::query("UPDATE access.access_grants SET state='active',contract=jsonb_set(contract,'{subjectKind}','\"owner\"'::jsonb) WHERE grant_id=$1")
        .bind(grant_id).execute(&pool).await?;
    let authorized: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM access.access_grants g JOIN access.course_memberships cm ON cm.course_id=g.course_id AND cm.actor_id=g.actor_id WHERE g.grant_id=$1 AND g.state='active' AND cm.state='active' AND (cm.expires_at IS NULL OR cm.expires_at>now()) AND cm.role=CASE g.contract->>'subjectKind' WHEN 'owner' THEN 'student' WHEN 'course_teacher' THEN 'teacher' ELSE '' END)")
        .bind(grant_id).fetch_one(&pool).await?;
    assert!(authorized);

    let bff_session_id = Uuid::now_v7();
    sqlx::query("INSERT INTO access.bff_sessions (session_id,actor_id,platform_roles,authorization_revision,expires_at,idle_expires_at,encrypted_csrf_token,csrf_encryption_key_id) VALUES ($1,$2,ARRAY['student'],1,now()+interval '30 minutes',now()+interval '15 minutes',$3,'test-key')")
        .bind(bff_session_id).bind(actor).bind(vec![7_u8; 32]).execute(&pool).await?;
    let capability_id = Uuid::now_v7();
    sqlx::query("INSERT INTO access.console_capabilities (capability_id,kind,access_grant_id,access_grant_revision,actor_id,bff_session_id,course_id,environment_id,environment_class,environment_revision,issued_at,expires_at,authorization_expires_at,locator_sha256,handoff_secret_sha256,encrypted_handoff_secret,encryption_key_id,idempotency_scope,idempotency_key_sha256) VALUES ($1,'xterm',$2,1,$3,$4,$5,$6,'experiment',1,now(),now()+interval '30 seconds',now()+interval '15 minutes',$7,$8,$9,'test-key','actor:test',$10)")
        .bind(capability_id).bind(grant_id).bind(actor).bind(bff_session_id).bind(course).bind(environment)
        .bind("f".repeat(64)).bind("a".repeat(64)).bind(vec![9_u8; 32]).bind("d".repeat(64)).execute(&pool).await?;
    assert!(sqlx::query("INSERT INTO access.console_capabilities (capability_id,kind,access_grant_id,access_grant_revision,actor_id,bff_session_id,course_id,environment_id,environment_class,environment_revision,issued_at,expires_at,authorization_expires_at,locator_sha256,handoff_secret_sha256,encrypted_handoff_secret,encryption_key_id,idempotency_scope,idempotency_key_sha256) VALUES ($1,'xterm',$2,1,$3,$4,$5,$6,'experiment',1,now(),now()+interval '31 seconds',now()+interval '15 minutes',$7,$8,$9,'test-key','actor:test',$10)")
        .bind(Uuid::now_v7()).bind(grant_id).bind(actor).bind(bff_session_id).bind(course).bind(environment)
        .bind("b".repeat(64)).bind("c".repeat(64)).bind(vec![9_u8; 32]).bind("e".repeat(64)).execute(&pool).await.is_err());
    let console_session_id = Uuid::now_v7();
    sqlx::query("INSERT INTO access.console_sessions (session_id,capability_id,kind,bff_session_id,access_grant_id,access_grant_revision,actor_id,course_id,environment_id,environment_revision,proxy_owner,state,opened_at,authorization_expires_at) VALUES ($1,$2,'xterm',$3,$4,1,$5,$6,$7,1,'test-proxy','opening',now(),now()+interval '15 minutes')")
        .bind(console_session_id).bind(capability_id).bind(bff_session_id).bind(grant_id).bind(actor).bind(course).bind(environment).execute(&pool).await?;
    let consumed = sqlx::query("UPDATE access.console_capabilities SET consumed_at=now(),session_id=$2,secret_scrubbed_at=now(),encrypted_handoff_secret='\\x'::bytea WHERE capability_id=$1 AND consumed_at IS NULL")
        .bind(capability_id).bind(console_session_id).execute(&pool).await?.rows_affected();
    let replay = sqlx::query("UPDATE access.console_capabilities SET consumed_at=now() WHERE capability_id=$1 AND consumed_at IS NULL")
        .bind(capability_id).execute(&pool).await?.rows_affected();
    assert_eq!((consumed, replay), (1, 0));
    let work_capability_id = Uuid::now_v7();
    let work_lease_id = Uuid::now_v7();
    sqlx::query("INSERT INTO access.console_capabilities (capability_id,kind,access_grant_id,access_grant_revision,actor_id,bff_session_id,course_id,environment_id,environment_class,environment_revision,lease_id,lease_revision,lease_expires_at,issued_at,expires_at,authorization_expires_at,locator_sha256,handoff_secret_sha256,encrypted_handoff_secret,encryption_key_id,idempotency_scope,idempotency_key_sha256) VALUES ($1,'xterm',$2,1,$3,$4,$5,$6,'work',1,$7,3,now()+interval '20 minutes',now(),now()+interval '30 seconds',now()+interval '15 minutes',$8,$9,$10,'test-key','actor:work',$11)")
        .bind(work_capability_id).bind(grant_id).bind(actor).bind(bff_session_id).bind(course).bind(environment).bind(work_lease_id)
        .bind("1".repeat(64)).bind("2".repeat(64)).bind(vec![9_u8; 32]).bind("3".repeat(64)).execute(&pool).await?;
    sqlx::query("INSERT INTO access.console_sessions (session_id,capability_id,kind,bff_session_id,access_grant_id,access_grant_revision,actor_id,course_id,environment_id,environment_revision,lease_id,lease_revision,lease_expires_at,proxy_owner,state,opened_at,authorization_expires_at) VALUES ($1,$2,'xterm',$3,$4,1,$5,$6,$7,1,$8,3,now()+interval '20 minutes','test-proxy','opening',now(),now()+interval '15 minutes')")
        .bind(Uuid::now_v7()).bind(work_capability_id).bind(bff_session_id).bind(grant_id).bind(actor).bind(course).bind(environment).bind(work_lease_id).execute(&pool).await?;
    let console_columns: Vec<String> = sqlx::query_scalar("SELECT column_name FROM information_schema.columns WHERE table_schema='access' AND table_name IN ('console_capabilities','console_sessions')")
        .fetch_all(&pool).await?;
    for forbidden in [
        "terminal_input",
        "terminal_output",
        "transcript",
        "cookie",
        "kubernetes_target",
        "credential",
    ] {
        assert!(
            !console_columns
                .iter()
                .any(|column| column.contains(forbidden))
        );
    }

    sqlx::query("UPDATE access.course_memberships SET state='revoked',revision=revision+1 WHERE course_id=$1 AND actor_id=$2")
        .bind(course).bind(actor).execute(&pool).await?;
    let authorized_after_revocation: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM access.access_grants g JOIN access.course_memberships cm ON cm.course_id=g.course_id AND cm.actor_id=g.actor_id WHERE g.grant_id=$1 AND g.state='active' AND cm.state='active' AND (cm.expires_at IS NULL OR cm.expires_at>now()) AND cm.role=CASE g.contract->>'subjectKind' WHEN 'owner' THEN 'student' WHEN 'course_teacher' THEN 'teacher' ELSE '' END)")
        .bind(grant_id).fetch_one(&pool).await?;
    assert!(!authorized_after_revocation);
    Ok(())
}
