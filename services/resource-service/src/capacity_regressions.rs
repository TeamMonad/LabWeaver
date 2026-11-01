// Included in capacity::tests so these exercise the production cache and admission path.

fn test_observer(cache: KubernetesObservationCache) -> KubernetesGpuCapacityObserver {
    KubernetesGpuCapacityObserver {
        provider_binding: "test-provider".into(),
        observation_ttl: Duration::from_secs(30),
        cache,
    }
}

#[tokio::test]
async fn pem_constructor_observes_authenticated_https_apiserver()
-> Result<(), Box<dyn std::error::Error>> {
    use hyper_util::{
        rt::{TokioExecutor, TokioIo},
        server::conn::auto::Builder,
        service::TowerToHyperService,
    };
    use std::io::Cursor;
    let (state, _, unused_server) = start_fake_api(HEALTH_CHANGE).await?;
    unused_server.abort();
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()])?;
    let pem = certificate.cert.pem();
    let roots =
        rustls_pemfile::certs(&mut Cursor::new(pem.as_bytes())).collect::<Result<Vec<_>, _>>()?;
    let key_pem = certificate.signing_key.serialize_pem();
    let key = rustls_pemfile::private_key(&mut Cursor::new(key_pem.as_bytes()))?
        .ok_or("missing TLS key")?;
    let tls = Arc::new(
        rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(roots, key)?,
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let router = Router::new()
        .fallback(any(
            |state: State<FakeApi>, request: Request<Body>| async move {
                if request
                    .headers()
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    != Some("Bearer observer-token")
                {
                    return json_response(
                        StatusCode::UNAUTHORIZED,
                        json!({"kind":"Status", "code":401}),
                    );
                }
                fake_api(state, request).await
            },
        ))
        .with_state(state.clone());
    let server = tokio::spawn(async move {
        let acceptor = tokio_rustls::TlsAcceptor::from(tls);
        while let Ok((stream, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            let router = router.clone();
            tokio::spawn(async move {
                let Ok(stream) = acceptor.accept(stream).await else {
                    return;
                };
                let _ = Builder::new(TokioExecutor::new())
                    .serve_connection_with_upgrades(
                        TokioIo::new(stream),
                        TowerToHyperService::new(router),
                    )
                    .await;
            });
        }
    });
    let temporary = tempfile::tempdir()?;
    let token_file = temporary.path().join("token");
    let ca_file = temporary.path().join("ca.pem");
    std::fs::write(&token_file, "observer-token")?;
    std::fs::write(&ca_file, pem)?;
    let configuration = GpuCapacityObservationConfiguration {
        provider_binding: "test-provider".into(),
        api_server: format!("https://localhost:{port}").parse()?,
        bearer_token_file: token_file,
        cluster_ca_file: ca_file.clone(),
        request_timeout_milliseconds: 1000,
        observation_ttl_seconds: 30,
        max_nodes: 10,
        max_pods: 10,
    };
    let observer = KubernetesGpuCapacityObserver::new(configuration.clone())?;
    wait_for_fresh(&observer.cache).await?;
    let snapshot = observer.observe(&[entry()?], &[]).await?;
    assert_eq!(
        snapshot.units.values().copied().collect::<Vec<_>>(),
        vec![1]
    );
    observer.shutdown();
    observer.cache.join().await;
    std::fs::write(ca_file, "not a certificate")?;
    assert!(matches!(
        KubernetesGpuCapacityObserver::new(configuration),
        Err(CapacityProviderError::Configuration)
    ));
    server.abort();
    Ok(())
}

async fn wait_for_units(
    observer: &KubernetesGpuCapacityObserver,
    entry: &GpuCatalogEntry,
    units: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if observer
                .observe(std::slice::from_ref(entry), &[])
                .await
                .is_ok_and(|snapshot| snapshot.units.get(&entry.id) == Some(&units))
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    Ok(())
}

#[tokio::test]
async fn incremental_apply_delete_updates_nodes_and_external_pods_without_relist()
-> Result<(), Box<dyn std::error::Error>> {
    let (state, client, server) = start_fake_api(HEALTH_CHANGE).await?;
    let observer = test_observer(KubernetesObservationCache::new(client, 10, 10));
    wait_for_fresh(&observer.cache).await?;
    let entry = entry()?;
    let pod = json!({"apiVersion":"v1", "kind":"Pod", "metadata":{"name":"outside", "namespace":"other", "uid":"external-pod", "resourceVersion":"4"},
        "spec":{"nodeName":"gpu-a", "containers":[{"name":"main", "resources":{"requests":{"nvidia.com/gpu":"1"}}}]}, "status":{"phase":"Running"}});
    state
        .pod_events
        .send(json!({"type":"ADDED", "object":pod}))?;
    wait_for_units(&observer, &entry, 0).await?;
    state
        .pod_events
        .send(json!({"type":"DELETED", "object":pod}))?;
    wait_for_units(&observer, &entry, 1).await?;
    state
        .node_events
        .send(json!({"type":"DELETED", "object":node("gpu-a", true, false, &json!("1"))}))?;
    wait_for_units(&observer, &entry, 0).await?;
    state
        .node_events
        .send(json!({"type":"ADDED", "object":node("gpu-a", true, false, &json!("1"))}))?;
    wait_for_units(&observer, &entry, 1).await?;
    assert_eq!(state.node_lists.load(Ordering::SeqCst), 1);
    assert_eq!(state.pod_lists.load(Ordering::SeqCst), 1);
    assert_eq!(state.node_watches.load(Ordering::SeqCst), 1);
    observer.shutdown();
    observer.cache.join().await;
    server.abort();
    Ok(())
}

#[tokio::test]
async fn empty_successful_watch_eof_is_stale_and_rate_bounded()
-> Result<(), Box<dyn std::error::Error>> {
    let (state, client, server) = start_fake_api(EMPTY_WATCH).await?;
    let observer = test_observer(KubernetesObservationCache::new(client, 10, 10));
    tokio::time::sleep(Duration::from_millis(450)).await;
    let requests = state.node_watches.load(Ordering::SeqCst);
    assert!(
        (2..=6).contains(&requests),
        "empty EOF reconnect count must be bounded: {requests}"
    );
    assert!(observer.observe(&[entry()?], &[]).await.is_err());
    observer.shutdown();
    tokio::time::timeout(Duration::from_secs(1), observer.cache.join()).await?;
    server.abort();
    Ok(())
}

async fn admission_store() -> Result<
    (
        testcontainers::ContainerAsync<testcontainers_modules::postgres::Postgres>,
        crate::store::PgResourceStore,
    ),
    Box<dyn std::error::Error>,
> {
    use testcontainers::{ImageExt, runners::AsyncRunner};
    let container = testcontainers_modules::postgres::Postgres::default()
        .with_tag("17.5-alpine")
        .start()
        .await?;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        ))
        .await?;
    sqlx::raw_sql("CREATE SCHEMA resource")
        .execute(&pool)
        .await?;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
    let catalog = persistence_sqlx::MigrationCatalog::load(&root.join("catalog.yaml"))?;
    for domain in catalog
        .domains
        .iter()
        .filter(|domain| domain.name == persistence_sqlx::Domain::Resource)
    {
        for migration in &domain.migrations {
            let sql = persistence_sqlx::MigrationCatalog::read_verified_sql(&root, migration)?;
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
                "SET search_path TO resource;\n{sql}"
            )))
            .execute(&pool)
            .await?;
        }
    }
    Ok((container, crate::store::PgResourceStore::new(pool)))
}

fn admission_request() -> contracts::environment::ResolveEnvironmentResourceReservationRequest {
    let gpu = contracts::resource::GpuRequest {
        class: "gpu".into(),
        count: 1,
    };
    contracts::environment::ResolveEnvironmentResourceReservationRequest {
        version: 1,
        environment_id: contracts::EnvironmentId::new(),
        project_id: contracts::ProjectId::new(),
        course_id: None,
        owner_actor_id: contracts::ActorId::new(),
        provider_binding: "test-provider".into(),
        approved_resources: contracts::resource::WorkloadResources {
            cpu_millicores: 1,
            memory_bytes: 1,
            storage_bytes: 1,
            gpu: Some(gpu.clone()),
        },
        gpu: Some(gpu),
        operation_id: contracts::OperationId::new(),
        environment_generation: 1,
        trace_id: "cache-admission".into(),
    }
}

async fn persist_snapshot(
    store: &crate::store::PgResourceStore,
    observer: &KubernetesGpuCapacityObserver,
    entry: &GpuCatalogEntry,
) -> Result<UtcTimestamp, Box<dyn std::error::Error>> {
    let snapshot = observer.observe(std::slice::from_ref(entry), &[]).await?;
    let deadline =
        UtcTimestamp::from_utc(snapshot.observed_at.get() + time::Duration::seconds(30))?;
    store
        .record_gpu_capacity_observation(
            entry.id,
            snapshot.units[&entry.id],
            &snapshot.source_binding,
            snapshot.observed_at,
            deadline,
        )
        .await?;
    Ok(deadline)
}

#[tokio::test]
async fn sync_barrier_blocks_postgres_admission_before_initial_bookmark()
-> Result<(), Box<dyn std::error::Error>> {
    let (_container, store) = admission_store().await?;
    let entry = entry()?;
    store
        .create_gpu_catalog_entry("sync-catalog", &entry)
        .await?;
    let now = store.current_time().await?;
    store
        .record_gpu_capacity_observation(
            entry.id,
            1,
            "old-list",
            now,
            UtcTimestamp::from_utc(now.get() + time::Duration::seconds(30))?,
        )
        .await?;
    let (state, client, server) = start_fake_api(SYNC_PENDING).await?;
    let observer = test_observer(KubernetesObservationCache::new(client, 10, 10));
    store.register_gpu_observer("test-provider".into(), observer.clone())?;
    tokio::time::timeout(Duration::from_secs(2), async {
        while state.active_watches.load(Ordering::SeqCst) != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        observer
            .observe(std::slice::from_ref(&entry), &[])
            .await
            .is_err(),
        "list/watch headers are not the sync barrier"
    );
    assert!(matches!(
        store
            .resolve_environment_resource_reservation(&admission_request())
            .await,
        Err(crate::store::ResourceStoreError::GpuObservationStale)
    ));
    state.gate.notify_waiters();
    wait_for_fresh(&observer.cache).await?;
    persist_snapshot(&store, &observer, &entry).await?;
    assert!(
        store
            .resolve_environment_resource_reservation(&admission_request())
            .await
            .is_ok()
    );
    observer.shutdown();
    observer.cache.join().await;
    server.abort();
    Ok(())
}

#[tokio::test]
async fn source_deadline_epoch_disconnect_and_restart_fence_postgres_admission()
-> Result<(), Box<dyn std::error::Error>> {
    let (_container, store) = admission_store().await?;
    let entry = entry()?;
    store
        .create_gpu_catalog_entry("epoch-catalog", &entry)
        .await?;
    let (state, client, server) = start_fake_api(WATCH_ERROR).await?;
    let observer = test_observer(KubernetesObservationCache::new(client, 10, 10));
    store.register_gpu_observer("test-provider".into(), observer.clone())?;
    wait_for_fresh(&observer.cache).await?;
    let first = persist_snapshot(&store, &observer, &entry).await?;
    tokio::time::sleep(Duration::from_millis(80)).await;
    let second = persist_snapshot(&store, &observer, &entry).await?;
    assert_eq!(
        first, second,
        "reading a cache must not renew its durable TTL"
    );
    let stored: time::OffsetDateTime =
        sqlx::query_scalar("SELECT max(valid_until) FROM resource.gpu_capacity_observations")
            .fetch_one(&store.pool())
            .await?;
    assert_eq!(stored, first.get());
    let restarted = crate::store::PgResourceStore::new(store.pool());
    assert!(
        matches!(
            restarted
                .resolve_environment_resource_reservation(&admission_request())
                .await,
            Err(crate::store::ResourceStoreError::GpuObservationStale)
        ),
        "a new process may not reuse the previous observer generation"
    );
    state
        .node_events
        .send(json!({"type":"MODIFIED", "object":node("gpu-a", false, false, &json!("0"))}))?;
    wait_for_units(&observer, &entry, 0).await?;
    assert!(
        matches!(
            store
                .resolve_environment_resource_reservation(&admission_request())
                .await,
            Err(crate::store::ResourceStoreError::GpuObservationStale)
        ),
        "unpersisted incremental revision may not reuse old capacity"
    );
    persist_snapshot(&store, &observer, &entry).await?;
    assert!(matches!(
        store
            .resolve_environment_resource_reservation(&admission_request())
            .await,
        Err(crate::store::ResourceStoreError::GpuCapacityExhausted)
    ));
    // Quiet watch source expiry must invalidate an otherwise unexpired database row.
    *observer.cache.node_freshness.last_fresh.write().await =
        Some(Instant::now() - Duration::from_secs(31));
    assert!(matches!(
        store
            .resolve_environment_resource_reservation(&admission_request())
            .await,
        Err(crate::store::ResourceStoreError::GpuObservationStale)
    ));
    observer.cache.node_freshness.mark_fresh().await;
    state
        .node_events
        .send(json!({"type":"MODIFIED", "object":node("gpu-a", true, false, &json!("1"))}))?;
    wait_for_units(&observer, &entry, 1).await?;
    persist_snapshot(&store, &observer, &entry).await?;
    state.node_events.send(json!({"type":"ERROR", "object":{"apiVersion":"v1", "kind":"Status", "status":"Failure", "reason":"Gone", "code":410}}))?;
    tokio::time::timeout(Duration::from_secs(2), async {
        while state.node_lists.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    assert!(
        matches!(
            store
                .resolve_environment_resource_reservation(&admission_request())
                .await,
            Err(crate::store::ResourceStoreError::GpuObservationStale)
        ),
        "disconnect must revoke the unexpired durable row"
    );
    state.relist_gate.notify_one();
    wait_for_fresh(&observer.cache).await?;
    persist_snapshot(&store, &observer, &entry).await?;
    observer.shutdown();
    observer.cache.join().await;
    assert!(
        matches!(
            store
                .resolve_environment_resource_reservation(&admission_request())
                .await,
            Err(crate::store::ResourceStoreError::GpuObservationStale)
        ),
        "shutdown must revoke the still-valid database observation"
    );
    server.abort();
    Ok(())
}
