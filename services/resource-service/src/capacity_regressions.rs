// Included in capacity::tests so these exercise the production cache and admission path.

fn test_observer(client: KubeClient) -> KubernetesGpuCapacityObserver {
    KubernetesGpuCapacityObserver {
        provider_binding: "test-provider".into(),
        observation_ttl: Duration::from_secs(30),
        cache: KubernetesObservationCache::new(client, 10, 10, "test-provider".into()),
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
    let observer = test_observer(client);
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
    let observer = test_observer(client);
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
    Ok(snapshot.persist(store, observer.observation_ttl).await?)
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
    let observer = test_observer(client);
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
    let observer = test_observer(client);
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
        Instant::now().checked_sub(Duration::from_secs(31));
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

async fn suspend_activation(
    store: &crate::store::PgResourceStore,
    activation: &contracts::environment::ActivateEnvironmentResourceReservationRequest,
    generation: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let result = store
        .suspend_environment_resource_reservation(
            &contracts::environment::SuspendEnvironmentResourceReservationRequest {
                version: 1,
                environment_id: activation.environment_id,
                project_id: activation.project_id,
                owner_actor_id: activation.owner_actor_id,
                operation_id: contracts::OperationId::new(),
                environment_generation: generation,
                trace_id: "cache-suspend".into(),
            },
        )
        .await?;
    assert!(result.applied);
    assert_eq!(
        result.state,
        contracts::environment::EnvironmentResourceReservationState::Suspended
    );
    Ok(())
}

async fn suspended_gpu(
    store: &crate::store::PgResourceStore,
    observer: &KubernetesGpuCapacityObserver,
    entry: &GpuCatalogEntry,
) -> Result<
    contracts::environment::ActivateEnvironmentResourceReservationRequest,
    Box<dyn std::error::Error>,
> {
    store
        .create_gpu_catalog_entry("restore-catalog", entry)
        .await?;
    wait_for_fresh(&observer.cache).await?;
    persist_snapshot(store, observer, entry).await?;
    let request = admission_request();
    let reserved = store
        .resolve_environment_resource_reservation(&request)
        .await?;
    let activation = contracts::environment::ActivateEnvironmentResourceReservationRequest {
        version: 1,
        environment_id: request.environment_id,
        project_id: request.project_id,
        course_id: request.course_id,
        owner_actor_id: request.owner_actor_id,
        provider_binding: request.provider_binding,
        approved_resources: request.approved_resources,
        gpu: request.gpu,
        expected_allocation: reserved.allocation,
        operation_id: contracts::OperationId::new(),
        environment_generation: 3,
        trace_id: "cache-restore".into(),
    };
    suspend_activation(store, &activation, 2).await?;
    Ok(activation)
}

#[tokio::test]
async fn suspended_restore_requires_live_revision_and_reconnected_observer()
-> Result<(), Box<dyn std::error::Error>> {
    let (_container, store) = admission_store().await?;
    let entry = entry()?;
    let (state, client, server) = start_fake_api(WATCH_ERROR).await?;
    let observer = test_observer(client);
    store.register_gpu_observer("test-provider".into(), observer.clone())?;
    let mut activation = suspended_gpu(&store, &observer, &entry).await?;
    state
        .node_events
        .send(json!({"type":"MODIFIED", "object":node("gpu-a", false, false, &json!("0"))}))?;
    wait_for_units(&observer, &entry, 0).await?;
    assert!(
        matches!(
            store
                .activate_environment_resource_reservation(&activation)
                .await,
            Err(crate::store::ResourceStoreError::GpuObservationStale)
        ),
        "restore must fence an unpersisted watch revision"
    );
    persist_snapshot(&store, &observer, &entry).await?;
    assert!(matches!(
        store
            .activate_environment_resource_reservation(&activation)
            .await,
        Err(crate::store::ResourceStoreError::GpuCapacityExhausted)
    ));
    state
        .node_events
        .send(json!({"type":"MODIFIED", "object":node("gpu-a", true, false, &json!("1"))}))?;
    wait_for_units(&observer, &entry, 1).await?;
    persist_snapshot(&store, &observer, &entry).await?;
    assert!(
        store
            .activate_environment_resource_reservation(&activation)
            .await?
            .applied
    );
    suspend_activation(&store, &activation, 4).await?;
    activation.environment_generation = 5;
    activation.operation_id = contracts::OperationId::new();
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
                .activate_environment_resource_reservation(&activation)
                .await,
            Err(crate::store::ResourceStoreError::GpuObservationStale)
        ),
        "disconnect must block suspended restore despite durable TTL"
    );
    state.relist_gate.notify_one();
    wait_for_fresh(&observer.cache).await?;
    persist_snapshot(&store, &observer, &entry).await?;
    assert!(
        store
            .activate_environment_resource_reservation(&activation)
            .await?
            .applied
    );
    observer.shutdown();
    observer.cache.join().await;
    server.abort();
    Ok(())
}

#[tokio::test]
async fn suspended_restore_after_restart_requires_new_generation()
-> Result<(), Box<dyn std::error::Error>> {
    let (_container, store) = admission_store().await?;
    let entry = entry()?;
    let (_, client, server) = start_fake_api(HEALTH_CHANGE).await?;
    let observer = test_observer(client);
    store.register_gpu_observer("test-provider".into(), observer.clone())?;
    let activation = suspended_gpu(&store, &observer, &entry).await?;
    observer.shutdown();
    observer.cache.join().await;
    let restarted = crate::store::PgResourceStore::new(store.pool());
    assert!(
        matches!(
            restarted
                .activate_environment_resource_reservation(&activation)
                .await,
            Err(crate::store::ResourceStoreError::GpuObservationStale)
        ),
        "restore cannot reuse a previous process generation"
    );
    let (_, client, restarted_server) = start_fake_api(HEALTH_CHANGE).await?;
    let restarted_observer = test_observer(client);
    restarted.register_gpu_observer("test-provider".into(), restarted_observer.clone())?;
    wait_for_fresh(&restarted_observer.cache).await?;
    assert!(
        matches!(
            restarted
                .activate_environment_resource_reservation(&activation)
                .await,
            Err(crate::store::ResourceStoreError::GpuObservationStale)
        ),
        "sync alone cannot bypass observation persistence"
    );
    persist_snapshot(&restarted, &restarted_observer, &entry).await?;
    assert!(
        restarted
            .activate_environment_resource_reservation(&activation)
            .await?
            .applied
    );
    restarted_observer.shutdown();
    restarted_observer.cache.join().await;
    server.abort();
    restarted_server.abort();
    Ok(())
}

async fn refresh_both_streams(
    state: &FakeApi,
    observer: &KubernetesGpuCapacityObserver,
) -> Result<(), Box<dyn std::error::Error>> {
    let node_revision = observer
        .cache
        .node_freshness
        .revision
        .load(Ordering::Acquire);
    let pod_revision = observer
        .cache
        .pod_freshness
        .revision
        .load(Ordering::Acquire);
    let bookmark = json!({"type":"BOOKMARK", "object":{"apiVersion":"v1", "kind":"PartialObjectMetadata", "metadata":{"resourceVersion":"10"}}});
    state.node_events.send(bookmark.clone())?;
    state.pod_events.send(bookmark)?;
    tokio::time::timeout(Duration::from_secs(2), async {
        while observer
            .cache
            .node_freshness
            .revision
            .load(Ordering::Acquire)
            == node_revision
            || observer
                .cache
                .pod_freshness
                .revision
                .load(Ordering::Acquire)
                == pod_revision
            || observer.source().await.is_err()
        {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    Ok(())
}

#[tokio::test]
async fn replica_generations_coexist_at_equal_millisecond_and_share_reservations()
-> Result<(), Box<dyn std::error::Error>> {
    let (_container, store_a) = admission_store().await?;
    let store_b = crate::store::PgResourceStore::new(store_a.pool());
    let mut entry = entry()?;
    entry.capacity_units = 2;
    store_a
        .create_gpu_catalog_entry("replica-catalog", &entry)
        .await?;
    let (api_a, client_a, server_a) = start_fake_api(PAGINATED).await?;
    let (api_b, client_b, server_b) = start_fake_api(PAGINATED).await?;
    let observer_a = test_observer(client_a);
    let observer_b = test_observer(client_b);
    store_a.register_gpu_observer("test-provider".into(), observer_a.clone())?;
    store_b.register_gpu_observer("test-provider".into(), observer_b.clone())?;
    wait_for_fresh(&observer_a.cache).await?;
    wait_for_fresh(&observer_b.cache).await?;
    assert_equal_millisecond_generations(&store_a, &store_b, &observer_a, &observer_b, &entry)
        .await?;
    tokio::time::sleep(Duration::from_millis(5)).await;
    refresh_both_streams(&api_b, &observer_b).await?;
    persist_snapshot(&store_b, &observer_b, &entry).await?;
    let request_a = admission_request();
    assert!(
        store_a
            .resolve_environment_resource_reservation(&request_a)
            .await
            .is_ok(),
        "A must select its own row even when B wrote a newer observation"
    );
    assert!(
        store_b
            .resolve_environment_resource_reservation(&admission_request())
            .await
            .is_ok()
    );
    refresh_both_streams(&api_a, &observer_a).await?;
    persist_snapshot(&store_a, &observer_a, &entry).await?;
    for store in [&store_a, &store_b] {
        assert!(
            matches!(
                store
                    .resolve_environment_resource_reservation(&admission_request())
                    .await,
                Err(crate::store::ResourceStoreError::GpuCapacityExhausted)
            ),
            "both replicas must count all shared reservations"
        );
    }
    let restarted = crate::store::PgResourceStore::new(store_a.pool());
    assert!(matches!(
        restarted
            .resolve_environment_resource_reservation(&admission_request())
            .await,
        Err(crate::store::ResourceStoreError::GpuObservationStale)
    ));
    store_a
        .release_environment_resource_reservation(
            &contracts::environment::ReleaseEnvironmentResourceReservationRequest {
                version: 1,
                environment_id: request_a.environment_id,
                project_id: request_a.project_id,
                owner_actor_id: request_a.owner_actor_id,
                operation_id: contracts::OperationId::new(),
                environment_generation: 2,
                trace_id: "replica-release".into(),
            },
        )
        .await?;
    let (_, client_c, server_c) = start_fake_api(PAGINATED).await?;
    let observer_c = test_observer(client_c);
    restarted.register_gpu_observer("test-provider".into(), observer_c.clone())?;
    wait_for_fresh(&observer_c.cache).await?;
    persist_snapshot(&restarted, &observer_c, &entry).await?;
    assert!(
        restarted
            .resolve_environment_resource_reservation(&admission_request())
            .await
            .is_ok()
    );
    assert!(
        matches!(
            store_b
                .resolve_environment_resource_reservation(&admission_request())
                .await,
            Err(crate::store::ResourceStoreError::GpuCapacityExhausted)
        ),
        "a peer remains usable after another replica restarts and writes"
    );
    for observer in [&observer_a, &observer_b, &observer_c] {
        observer.shutdown();
        observer.cache.join().await;
    }
    server_a.abort();
    server_b.abort();
    server_c.abort();
    Ok(())
}

#[tokio::test]
async fn missing_initial_bookmark_reports_specific_stale_reason_on_eof_and_timeout()
-> Result<(), Box<dyn std::error::Error>> {
    for mode in [NO_BARRIER, NO_BARRIER_OPEN] {
        let (_, client, server) = start_fake_api(mode).await?;
        let nodes = tokio::sync::RwLock::new(Vec::new());
        let pods = tokio::sync::RwLock::new(Vec::new());
        let node_freshness = ObservationFreshness::new();
        let pod_freshness = ObservationFreshness::new();
        let node_api = Api::<Node>::all(client.clone());
        let pod_api = Api::<Pod>::all(client);
        let (node_result, pod_result) = tokio::join!(
            reflect_session(&node_api, &nodes, &node_freshness, 10, 10),
            reflect_session(&pod_api, &pods, &pod_freshness, 10, 10)
        );
        for (kind, result) in [("Node", node_result), ("Pod", pod_result)] {
            let error = result.err().ok_or(
                "watch without an initial-events bookmark must report the missing barrier",
            )?;
            assert!(
                error
                    .to_string()
                    .contains("LW_RESOURCE_GPU_OBSERVATION_STALE"),
                "{kind}: {error}"
            );
            assert!(
                error
                    .to_string()
                    .contains("initial-events bookmark missing"),
                "{kind}: {error}"
            );
        }
        assert!(nodes.read().await.is_empty());
        assert!(pods.read().await.is_empty());
        assert!(node_freshness.check(Duration::from_secs(30)).await.is_err());
        assert!(pod_freshness.check(Duration::from_secs(30)).await.is_err());
        server.abort();
    }
    Ok(())
}

async fn assert_equal_millisecond_generations(
    store_a: &crate::store::PgResourceStore,
    store_b: &crate::store::PgResourceStore,
    observer_a: &KubernetesGpuCapacityObserver,
    observer_b: &KubernetesGpuCapacityObserver,
    entry: &GpuCatalogEntry,
) -> Result<(), Box<dyn std::error::Error>> {
    // Retain independent API snapshots, but give both source clocks the exact same millisecond.
    let equal = store_a.current_time().await?;
    for observer in [observer_a, observer_b] {
        *observer.cache.node_freshness.observed_at.write().await = Some(equal.get());
        *observer.cache.pod_freshness.observed_at.write().await = Some(equal.get());
    }
    persist_snapshot(store_a, observer_a, entry).await?;
    persist_snapshot(store_b, observer_b, entry).await?;
    let generations: i64 = sqlx::query_scalar("SELECT count(DISTINCT observer_generation) FROM resource.gpu_capacity_observations WHERE entry_id=$1 AND observed_at=$2")
        .bind(entry.id.as_uuid()).bind(equal.get()).fetch_one(&store_a.pool()).await?;
    assert_eq!(
        generations, 2,
        "equal-millisecond replicas must not overwrite one another"
    );
    Ok(())
}

#[tokio::test]
async fn api_error_before_initial_bookmark_preserves_typed_cause_and_closes_cache()
-> Result<(), Box<dyn std::error::Error>> {
    let (_, client, server) = start_fake_api(ERROR_BEFORE_BARRIER).await?;
    let nodes = tokio::sync::RwLock::new(Vec::new());
    let pods = tokio::sync::RwLock::new(Vec::new());
    let node_freshness = ObservationFreshness::new();
    let pod_freshness = ObservationFreshness::new();
    let node_api = Api::<Node>::all(client.clone());
    let pod_api = Api::<Pod>::all(client.clone());
    let (node_result, pod_result) = tokio::join!(
        reflect_session(&node_api, &nodes, &node_freshness, 10, 10),
        reflect_session(&pod_api, &pods, &pod_freshness, 10, 10)
    );
    for (kind, result) in [("Node", node_result), ("Pod", pod_result)] {
        let error = result
            .err()
            .ok_or("pre-bookmark API ERROR must fail synchronization")?;
        let sync_error = error
            .downcast_ref::<InitialWatchSyncError>()
            .ok_or("pre-bookmark API ERROR must retain the typed barrier wrapper")?;
        assert!(
            sync_error
                .to_string()
                .contains("LW_RESOURCE_GPU_OBSERVATION_STALE"),
            "{kind}: {sync_error}"
        );
        assert!(
            sync_error
                .to_string()
                .contains("initial-events bookmark missing"),
            "{kind}: {sync_error}"
        );
        let cause = std::error::Error::source(sync_error).ok_or("missing Kubernetes cause")?;
        let Some(kube::Error::Api(api_error)) = cause.downcast_ref::<kube::Error>() else {
            return Err("barrier error source must be a typed Kubernetes API error".into());
        };
        assert_eq!(api_error.code, 410);
        assert_eq!(api_error.status, "Failure");
        assert_eq!(api_error.reason, "Expired");
        assert_eq!(
            api_error.message,
            "requested resourceVersion 1 is older than 9"
        );
    }
    assert!(nodes.read().await.is_empty());
    assert!(pods.read().await.is_empty());
    assert!(node_freshness.check(Duration::from_secs(30)).await.is_err());
    assert!(pod_freshness.check(Duration::from_secs(30)).await.is_err());
    let observer = test_observer(client);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if *observer.cache.node_freshness.reason.read().await
                == "initial-events bookmark missing"
                && *observer.cache.pod_freshness.reason.read().await
                    == "initial-events bookmark missing"
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await?;
    assert!(matches!(
        observer.observe(&[entry()?], &[]).await,
        Err(CapacityProviderError::ObservationStale(_))
    ));
    assert!(observer.cache.nodes.read().await.is_empty());
    assert!(observer.cache.pods.read().await.is_empty());
    observer.shutdown();
    observer.cache.join().await;
    server.abort();
    Ok(())
}
