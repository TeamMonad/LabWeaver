//! Real `PostgreSQL` evidence for Access schema constraints and token storage.

use sqlx::postgres::PgPoolOptions;
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;

#[tokio::test]
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
        "CREATE SCHEMA access; SET search_path TO access;\n{}\n{}\n{}",
        include_str!("../../../migrations/access/0001_initial.sql"),
        include_str!("../../../migrations/access/0002_auth.sql"),
        include_str!("../../../migrations/access/0003_access_grants.sql")
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
    sqlx::query("INSERT INTO access.ssh_authorizations (authorization_id,token_sha256,grant_id,grant_revision,endpoint_grant_id,key_id,gateway_identity,connection_id,source_address_sha256,issued_at,expires_at) VALUES ($1,$2,$3,1,$4,$5,'spiffe://labweaver/gateway','connection-1',$6,now(),now()+interval '30 seconds')")
        .bind(authorization_id)
        .bind("b".repeat(64))
        .bind(grant_id)
        .bind(endpoint_grant_id)
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
    Ok(())
}
