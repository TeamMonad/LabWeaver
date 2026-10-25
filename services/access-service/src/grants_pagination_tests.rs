use super::*;
use testcontainers::{ImageExt, runners::AsyncRunner};

#[path = "../tests/support/mod.rs"]
mod support;

#[tokio::test]
#[allow(clippy::too_many_lines, clippy::expect_used)]
async fn grant_pagination_filters_before_limit_and_survives_concurrent_insert_delete()
-> Result<(), Box<dyn std::error::Error>> {
    let postgres = testcontainers_modules::postgres::Postgres::default()
        .with_tag("17.5-alpine")
        .start()
        .await?;
    let url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        postgres.get_host_port_ipv4(5432).await?
    );
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await?;
    support::apply_access_migrations(&pool).await?;
    let actor = ActorId::new();
    let foreign = ActorId::new();
    let project = ProjectId::new();
    let environment = contracts::EnvironmentId::new();
    let other_environment = contracts::EnvironmentId::new();
    let endpoint = EndpointId::new();
    for id in [actor, foreign] {
        sqlx::query("INSERT INTO access.actors (actor_id,issuer,subject_sha256) VALUES ($1,'https://issuer.example.test',$2)")
            .bind(id.as_uuid()).bind(Sha256Digest::of_bytes(id.to_string().as_bytes()).to_string())
            .execute(&pool).await?;
    }
    // Equal microsecond timestamps force the UUID tie breaker. Newer excluded rows would
    // exhaust a page if endpoint or actor filtering happened after LIMIT.
    let mut owned = Vec::new();
    for (index, owner, env, matches_endpoint) in [
        (1_u128, actor, environment, true),
        (2, actor, environment, true),
        (3, actor, environment, true),
        (4, actor, environment, false),
        (5, foreign, environment, true),
        (6, actor, other_environment, true),
    ] {
        let id = Uuid::from_u128(index);
        sqlx::query("INSERT INTO access.access_grants (grant_id,actor_id,project_id,environment_id,revision,state,not_before,expires_at,contract,reason_code,created_at) VALUES ($1,$2,$3,$4,1,'expired',now()-interval '2 hours',now()-interval '1 hour','{}','expired','2026-01-01T00:00:00.123456Z')")
            .bind(id).bind(owner.as_uuid()).bind(project.as_uuid()).bind(env.as_uuid())
            .execute(&pool).await?;
        if matches_endpoint {
            sqlx::query("INSERT INTO access.endpoint_grants (endpoint_grant_id,grant_id,endpoint_id,endpoint_revision,protocol,health,expires_at,contract) VALUES ($1,$2,$3,1,'https','healthy',now()+interval '1 hour','{}')")
                .bind(Uuid::now_v7()).bind(id).bind(endpoint.as_uuid()).execute(&pool).await?;
        }
        if owner == actor && env == environment && matches_endpoint {
            owned.push(id);
        }
    }
    let mut query = EnvironmentAccessGrantListQuery {
        state: None,
        endpoint_id: Some(endpoint),
        include_terminal: true,
        cursor: None,
        limit: Some(1),
    };
    let first = access_grant_ids_page(&pool, actor, environment, &query)
        .await
        .expect("first page");
    assert_eq!(first.items, vec![owned[2]]);
    assert!(first.next_cursor.is_some());
    query.cursor = first.next_cursor;
    for (owner, env, endpoint_id) in [
        (foreign, environment, Some(endpoint)),
        (actor, other_environment, Some(endpoint)),
        (actor, environment, None),
    ] {
        let foreign_query = EnvironmentAccessGrantListQuery {
            endpoint_id,
            ..query.clone()
        };
        assert!(
            access_grant_ids_page(&pool, owner, env, &foreign_query)
                .await
                .is_err()
        );
    }
    let writer = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await?;
    // Commit on an independent connection while the client holds its cursor. Removing the
    // issuing row must not invalidate continuation; a new head must not repeat a prior page.
    sqlx::query("DELETE FROM access.endpoint_grants WHERE grant_id=$1")
        .bind(owned[2])
        .execute(&writer)
        .await?;
    sqlx::query("DELETE FROM access.access_grants WHERE grant_id=$1")
        .bind(owned[2])
        .execute(&writer)
        .await?;
    let inserted = Uuid::now_v7();
    sqlx::query("INSERT INTO access.access_grants (grant_id,actor_id,project_id,environment_id,revision,state,not_before,expires_at,contract,reason_code) VALUES ($1,$2,$3,$4,1,'expired',now()-interval '2 hours',now()-interval '1 hour','{}','expired')")
        .bind(inserted).bind(actor.as_uuid()).bind(project.as_uuid()).bind(environment.as_uuid())
        .execute(&writer).await?;
    sqlx::query("INSERT INTO access.endpoint_grants (endpoint_grant_id,grant_id,endpoint_id,endpoint_revision,protocol,health,expires_at,contract) VALUES ($1,$2,$3,1,'https','healthy',now()+interval '1 hour','{}')")
        .bind(Uuid::now_v7()).bind(inserted).bind(endpoint.as_uuid()).execute(&writer).await?;
    let second = access_grant_ids_page(&pool, actor, environment, &query)
        .await
        .expect("second page");
    assert_eq!(second.items, vec![owned[1]]);
    query.cursor = second.next_cursor;
    let third = access_grant_ids_page(&pool, actor, environment, &query)
        .await
        .expect("third page");
    assert_eq!(third.items, vec![owned[0]]);
    assert!(third.next_cursor.is_none());
    Ok(())
}

#[tokio::test]
#[allow(clippy::expect_used)]
async fn ssh_key_cursor_preserves_microseconds_and_filters_other_actors()
-> Result<(), Box<dyn std::error::Error>> {
    let postgres = testcontainers_modules::postgres::Postgres::default()
        .with_tag("17.5-alpine")
        .start()
        .await?;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            postgres.get_host_port_ipv4(5432).await?
        ))
        .await?;
    support::apply_access_migrations(&pool).await?;
    let actor = ActorId::new();
    let foreign = ActorId::new();
    for id in [actor, foreign] {
        sqlx::query("INSERT INTO access.actors (actor_id,issuer,subject_sha256) VALUES ($1,'https://issuer.example.test',$2)")
            .bind(id.as_uuid()).bind(Sha256Digest::of_bytes(id.to_string().as_bytes()).to_string()).execute(&pool).await?;
    }
    let mut own_ids = Vec::new();
    for index in 0_u8..4 {
        let owner = if index == 0 { foreign } else { actor };
        let id = Uuid::from_u128(u128::from(index) + 1);
        if owner == actor {
            own_ids.push(id);
        }
        sqlx::query("INSERT INTO access.ssh_public_keys (key_id,actor_id,fingerprint_sha256,algorithm,normalized_openssh,created_at) VALUES ($1,$2,$3,'ed25519','ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA','2026-01-01T00:00:00.123456Z')")
            .bind(id).bind(owner.as_uuid())
            .bind(format!("SHA256:{}", base64::engine::general_purpose::STANDARD_NO_PAD.encode([index;32])))
            .execute(&pool).await?;
    }
    let mut query = KeyListQuery {
        cursor: None,
        limit: Some(1),
    };
    let first = ssh_keys_page(&pool, actor, &query)
        .await
        .expect("first SSH page");
    assert_eq!(first.items[0].id.as_uuid(), own_ids[0]);
    query.cursor = first.next_cursor;
    assert!(ssh_keys_page(&pool, foreign, &query).await.is_err());
    sqlx::query("DELETE FROM access.ssh_public_keys WHERE key_id=$1")
        .bind(own_ids[0])
        .execute(&pool)
        .await?;
    let second = ssh_keys_page(&pool, actor, &query)
        .await
        .expect("second SSH page");
    assert_eq!(second.items[0].id.as_uuid(), own_ids[1]);
    query.cursor = second.next_cursor;
    let third = ssh_keys_page(&pool, actor, &query)
        .await
        .expect("third SSH page");
    assert_eq!(third.items[0].id.as_uuid(), own_ids[2]);
    assert!(third.next_cursor.is_none());
    assert!(
        ssh_keys_page(
            &pool,
            actor,
            &KeyListQuery {
                cursor: None,
                limit: Some(0)
            }
        )
        .await
        .is_err()
    );
    Ok(())
}
