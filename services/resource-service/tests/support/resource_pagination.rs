//! Real Resource writes and PostgreSQL keysets; no replacement business backend.
use super::*;
use contracts::http::ResourceListQuery;
use contracts::resource::credits::{CreditAdminOperation, CreditHoldRequest};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum List {
    Requests,
    Leases,
    ProjectLeases,
    Rates,
    Gpu,
    Charges,
    Usage,
    Ledger,
    Holds,
}
impl List {
    const fn project_scoped(self) -> bool {
        matches!(
            self,
            Self::ProjectLeases | Self::Charges | Self::Usage | Self::Ledger | Self::Holds
        )
    }
    const fn table(self) -> (&'static str, &'static str) {
        match self {
            Self::Requests => ("resource_requests", "request_id"),
            Self::Leases | Self::ProjectLeases => ("resource_leases", "lease_id"),
            Self::Rates => ("resource_rates", "rate_id"),
            Self::Gpu => ("gpu_catalog_entries", "entry_id"),
            Self::Charges => ("resource_charges", "charge_id"),
            Self::Usage => ("resource_usage_records", "usage_record_id"),
            Self::Ledger => ("credit_ledger_entries", "entry_id"),
            Self::Holds => ("credit_holds", "hold_id"),
        }
    }
}

struct Fixture {
    _container: testcontainers::ContainerAsync<Postgres>,
    pool: sqlx::PgPool,
    store: PgResourceStore,
    project: ProjectId,
    other_project: ProjectId,
    actor: ActorId,
    now: UtcTimestamp,
    targets: Vec<(
        ProjectId,
        ResourceRequest,
        contracts::resource::ResourceLease,
    )>,
}
impl Fixture {
    async fn new(kind: List) -> Result<Self> {
        let (container, pool) = migrated_pool().await?;
        let store = PgResourceStore::new(pool.clone());
        let project = ProjectId::new();
        let other_project = ProjectId::new();
        let actor = ActorId::new();
        let mut now = store.current_time().await?;
        let mut targets = Vec::new();
        if matches!(kind, List::Charges | List::Usage) {
            for (index, scope) in [project, other_project].into_iter().enumerate() {
                let (request, lease) = create_active_request(
                    &pool,
                    &store,
                    now,
                    scope,
                    actor,
                    &format!("page-meter-{index}"),
                )
                .await?;
                targets.push((scope, request, lease));
            }
            now = store.current_time().await?;
            store
                .create_rate(
                    "page-cpu",
                    &CreateResourceRateRequest {
                        builder_binding: None,
                        model_binding: None,
                        max_preauthorization: None,
                        unit: ResourceBillingUnit::CpuMillicoreSecond,
                        unit_quantity: 1,
                        gpu_class: None,
                        gpu_mode: None,
                        unit_price: Money {
                            currency: "CREDITS".into(),
                            amount: FixedDecimal::parse("0.100000")?,
                        },
                        effective_from: now,
                        effective_until: None,
                    },
                )
                .await?;
        }
        let fixture = Self {
            _container: container,
            pool,
            store,
            project,
            other_project,
            actor,
            now,
            targets,
        };
        if matches!(kind, List::Holds | List::Charges | List::Usage) {
            fixture.grant(project, "page-funds-main").await?;
            fixture.grant(other_project, "page-funds-other").await?;
        }
        Ok(fixture)
    }

    async fn grant(&self, project: ProjectId, key: &str) -> Result {
        resource_service::credits::PgCreditStore::new(self.pool.clone())
            .admin(
                key,
                self.actor,
                &[PlatformRole::PlatformAdmin],
                &CreditAdminOperation::Grant {
                    project_id: project,
                    amount: FixedDecimal::parse("100.000000")?,
                    reason: "isolated pagination allowance".into(),
                },
            )
            .await?;
        Ok(())
    }

    async fn seed(&self, kind: List, index: u16, project: ProjectId, actor: ActorId) -> Result {
        let key = format!("page-{index}-{project}-{actor}");
        match kind {
            List::Requests | List::Holds => {
                let mut request = outbox_request()?;
                request.project_id = project;
                request.requester_id = actor;
                request.request_key.clone_from(&key);
                request.created_at = self.now;
                request.updated_at = self.now;
                self.store
                    .create(&key, &request, "pagination-create")
                    .await?;
                if kind == List::Holds {
                    resource_service::credits::PgCreditStore::new(self.pool.clone())
                        .hold(
                            &key,
                            "pagination-owner",
                            &CreditHoldRequest {
                                project_id: project,
                                request_id: request.id,
                                business_key: key.clone(),
                                amount: FixedDecimal::parse("1.000000")?,
                            },
                        )
                        .await?;
                }
            }
            List::Leases | List::ProjectLeases => {
                create_allocating_request(&self.store, self.now, project, actor, &key).await?;
            }
            List::Rates => {
                self.store
                    .create_rate(
                        &key,
                        &CreateResourceRateRequest {
                            builder_binding: Some(key.clone()),
                            model_binding: None,
                            max_preauthorization: None,
                            unit: ResourceBillingUnit::BuildSecond,
                            unit_quantity: 1,
                            gpu_class: None,
                            gpu_mode: None,
                            unit_price: Money {
                                currency: "CREDITS".into(),
                                amount: FixedDecimal::parse("1.000000")?,
                            },
                            effective_from: self.now,
                            effective_until: None,
                        },
                    )
                    .await?;
            }
            List::Gpu => {
                let mut seed = review_seed(&format!("page-gpu-{index}"));
                seed.provider_binding = format!("page-provider-{index}");
                seed.allocation_binding = format!("page-allocation-{index}");
                self.store.seed_gpu_catalog(&[seed]).await?;
            }
            List::Ledger => self.grant(project, &key).await?,
            List::Charges | List::Usage => {
                let (_, request, lease) = self
                    .targets
                    .iter()
                    .find(|(scope, _, _)| *scope == project)
                    .ok_or("meter target missing")?;
                let from = UtcTimestamp::from_utc(
                    self.now.get() + time::Duration::seconds(i64::from(index) * 2),
                )?;
                let until = UtcTimestamp::from_utc(from.get() + time::Duration::seconds(1))?;
                let usage = self
                    .store
                    .record_usage(
                        &RecordResourceUsageRequest {
                            kind: ResourceUsageKind::Compute,
                            target: contracts::resource::ResourceUsageTarget::ResourceRequest {
                                request_id: request.id,
                                lease_id: Some(lease.id),
                            },
                            source_event_id: EventId::new(),
                            measured_from: from,
                            measured_until: until,
                            measurement: UsageMeasurement::Known {
                                quantities: ResourceUsageQuantities {
                                    quantity_divisor: 1,
                                    cpu_millicore_seconds: 1,
                                    memory_byte_seconds: 0,
                                    storage_byte_seconds: 0,
                                    gpu_unit_seconds: 0,
                                },
                            },
                        },
                        UtcTimestamp::from_utc(self.now.get() + time::Duration::minutes(1))?,
                    )
                    .await?;
                self.store
                    .settle_usage(usage.id)
                    .await?
                    .ok_or("meter charge missing")?;
            }
        }
        Ok(())
    }

    async fn read(
        &self,
        kind: List,
        project: ProjectId,
        actor: ActorId,
        query: &ResourceListQuery,
    ) -> Result<Value> {
        Ok(match kind {
            List::Requests => serde_json::to_value(self.store.list_all_requests(query).await?)?,
            List::Leases => serde_json::to_value(self.store.list_all_leases(query).await?)?,
            List::ProjectLeases => serde_json::to_value(
                self.store
                    .list_project_leases(project, Some(actor), query)
                    .await?,
            )?,
            List::Rates => serde_json::to_value(self.store.list_rates(query).await?)?,
            List::Gpu => serde_json::to_value(self.store.list_gpu_catalog(query).await?)?,
            List::Charges => serde_json::to_value(self.store.list_charges(project, query).await?)?,
            List::Usage => serde_json::to_value(self.store.list_usage(project, query).await?)?,
            List::Ledger => {
                serde_json::to_value(self.store.list_credit_ledger(project, query).await?)?
            }
            List::Holds => {
                serde_json::to_value(self.store.list_credit_holds(project, query).await?)?
            }
        })
    }

    async fn delete(&self, kind: List, id: &str) -> Result<bool> {
        let id = Uuid::parse_str(id)?;
        if kind == List::Requests {
            sqlx::query("DELETE FROM resource.resource_request_transitions WHERE request_id=$1")
                .bind(id)
                .execute(&self.pool)
                .await?;
        }
        if kind == List::Usage {
            sqlx::query("DELETE FROM resource.resource_charges WHERE usage_record_id=$1")
                .bind(id)
                .execute(&self.pool)
                .await?;
        }
        let (table, key) = kind.table();
        // Both identifiers come exclusively from the fixed enum above, never from an HTTP caller.
        let result = sqlx::query(sqlx::AssertSqlSafe(format!(
            "DELETE FROM resource.{table} WHERE {key}=$1"
        )))
        .bind(id)
        .execute(&self.pool)
        .await;
        if matches!(kind, List::Ledger | List::Holds) {
            // Financial retention is a domain invariant, not something pagination tests may disable.
            let error = result.expect_err("ledger/linked hold deletion must remain forbidden");
            let code = error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .ok_or("missing deletion rejection")?;
            assert_eq!(
                code,
                if kind == List::Ledger {
                    "P0001"
                } else {
                    "23503"
                }
            );
            Ok(false)
        } else {
            assert_eq!(result?.rows_affected(), 1);
            Ok(true)
        }
    }
}

fn ids(page: &Value) -> Vec<String> {
    page["items"]
        .as_array()
        .expect("real page items")
        .iter()
        .map(|item| item["id"].as_str().expect("typed resource ID").to_owned())
        .collect()
}

async fn stable_pages(kind: List) -> Result {
    let fixture = Fixture::new(kind).await?;
    for index in 0..5 {
        fixture
            .seed(kind, index, fixture.project, fixture.actor)
            .await?;
    }
    // Newest hidden rows must not consume the SQL LIMIT or generate a false end page.
    if kind.project_scoped() {
        fixture
            .seed(kind, 5, fixture.other_project, fixture.actor)
            .await?;
        if kind == List::ProjectLeases {
            fixture
                .seed(kind, 6, fixture.project, ActorId::new())
                .await?;
        }
    }
    let all = fixture
        .read(
            kind,
            fixture.project,
            fixture.actor,
            &ResourceListQuery {
                limit: Some(100),
                ..Default::default()
            },
        )
        .await?;
    let baseline = ids(&all);
    assert_eq!(baseline.len(), 5);
    assert!(all["nextCursor"].is_null());
    let query = ResourceListQuery {
        limit: Some(2),
        ..Default::default()
    };
    // Usage seeds already share a valid observed_at after all intervals. Ledger rows cannot be updated.
    if !matches!(kind, List::Rates | List::Gpu | List::Ledger | List::Usage) {
        let (table, _) = kind.table();
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE resource.{table} SET created_at=$1"
        )))
        .bind(fixture.now.get())
        .execute(&fixture.pool)
        .await?;
    }
    let first = fixture
        .read(kind, fixture.project, fixture.actor, &query)
        .await?;
    assert_eq!(ids(&first), baseline[..2]);
    let cursor = first["nextCursor"]
        .as_str()
        .ok_or("missing continuation")?
        .to_owned();
    let continuation = ResourceListQuery {
        cursor: Some(cursor.clone()),
        ..query.clone()
    };

    let mut altered: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(&cursor)?)?;
    altered["cursor"]["ordinal"] = json!(1);
    let forged = ResourceListQuery {
        cursor: Some(URL_SAFE_NO_PAD.encode(serde_json::to_vec(&altered)?)),
        ..query.clone()
    };
    assert!(
        fixture
            .read(kind, fixture.project, fixture.actor, &forged)
            .await
            .is_err()
    );
    let other_collection = if kind == List::Requests {
        List::Rates
    } else {
        List::Requests
    };
    assert!(
        fixture
            .read(
                other_collection,
                fixture.project,
                fixture.actor,
                &continuation
            )
            .await
            .is_err()
    );
    if kind.project_scoped() {
        assert!(
            fixture
                .read(kind, fixture.other_project, fixture.actor, &continuation)
                .await
                .is_err()
        );
    }
    if kind == List::ProjectLeases {
        assert!(
            fixture
                .read(kind, fixture.project, ActorId::new(), &continuation)
                .await
                .is_err()
        );
        let course = ResourceListQuery {
            course_id: Some(CourseId::new()),
            ..continuation.clone()
        };
        assert!(
            fixture
                .read(kind, fixture.project, fixture.actor, &course)
                .await
                .is_err()
        );
        // Changing from owner-only visibility to administrator visibility is a new scope too.
        assert!(
            fixture
                .store
                .list_project_leases(fixture.project, None, &continuation)
                .await
                .is_err()
        );
    }
    // Deletable collections lose the anchor. The immutable ledger and referenced holds instead
    // prove PostgreSQL rejects deletion; neither case permits a cursor to depend on an anchor lookup.
    let anchor_deleted = fixture.delete(kind, &baseline[1]).await?;
    assert_eq!(anchor_deleted, !matches!(kind, List::Ledger | List::Holds));
    let (inserted, deleted) = tokio::join!(
        fixture.seed(kind, 10, fixture.project, fixture.actor),
        fixture.delete(kind, &baseline[3]),
    );
    inserted?;
    let deleted = deleted?;
    let mut next = continuation;
    let mut remaining = Vec::new();
    loop {
        let page = fixture
            .read(kind, fixture.project, fixture.actor, &next)
            .await?;
        let page_ids = ids(&page);
        assert!(
            !page_ids.is_empty(),
            "limit+1 must not offer an empty terminal page"
        );
        remaining.extend(page_ids);
        next.cursor = page["nextCursor"].as_str().map(str::to_owned);
        if next.cursor.is_none() {
            break;
        }
    }
    let expected = baseline
        .iter()
        .enumerate()
        .skip(2)
        .filter(|(index, _)| !deleted || *index != 3)
        .map(|(_, id)| id.clone())
        .collect::<Vec<_>>();
    assert_eq!(remaining, expected);
    let seen = ids(&first).into_iter().chain(remaining).collect::<Vec<_>>();
    assert_eq!(
        seen.iter().collect::<BTreeSet<_>>().len(),
        seen.len(),
        "no duplicated billing or resource rows"
    );
    // A new traversal sees the concurrent insertion, while the old traversal did not shift.
    let fresh = fixture
        .read(kind, fixture.project, fixture.actor, &query)
        .await?;
    assert_ne!(ids(&fresh)[0], baseline[0]);
    Ok(())
}

#[tokio::test]
async fn global_requests_cursor_survives_tied_times_insert_delete_and_tampering() -> Result {
    stable_pages(List::Requests).await
}
#[tokio::test]
async fn global_leases_cursor_survives_tied_times_insert_delete_and_tampering() -> Result {
    stable_pages(List::Leases).await
}
#[tokio::test]
async fn project_leases_cursor_filters_owner_before_limit_and_binds_scope() -> Result {
    stable_pages(List::ProjectLeases).await
}
#[tokio::test]
async fn rates_cursor_survives_identical_effective_times_and_deleted_anchor() -> Result {
    stable_pages(List::Rates).await
}
#[tokio::test]
async fn gpu_catalog_cursor_survives_insert_delete_and_rejects_other_catalog_scope() -> Result {
    stable_pages(List::Gpu).await
}
#[tokio::test]
async fn charges_cursor_never_duplicates_or_skips_surviving_billing_rows() -> Result {
    stable_pages(List::Charges).await
}
#[tokio::test]
async fn usage_cursor_never_duplicates_or_skips_surviving_meter_rows() -> Result {
    stable_pages(List::Usage).await
}
#[tokio::test]
async fn credit_ledger_cursor_never_duplicates_or_skips_surviving_financial_rows() -> Result {
    stable_pages(List::Ledger).await
}
#[tokio::test]
async fn credit_holds_cursor_never_duplicates_or_skips_surviving_financial_rows() -> Result {
    stable_pages(List::Holds).await
}

#[tokio::test]
async fn resource_list_http_contract_enforces_real_jwt_authorization_and_cursor_errors() -> Result {
    let fixture = mutation_http_fixture_with_projection(false).await?;
    let project = fixture.request.project_id;
    let paths = [
        "/api/v1/resource-requests".to_owned(),
        "/api/v1/resource-leases".to_owned(),
        format!("/api/v1/projects/{project}/resource-leases"),
        "/api/v1/resource/rates".to_owned(),
        "/api/v1/resource/gpu-catalog".to_owned(),
        format!("/api/v1/projects/{project}/charges"),
        format!("/api/v1/projects/{project}/usage"),
        format!("/api/v1/projects/{project}/credits/ledger"),
        format!("/api/v1/projects/{project}/credits/holds"),
    ];
    for path in &paths {
        let url = format!("{}{path}", fixture.base);
        let denied = fixture.client.get(&url).send().await?;
        assert_eq!(denied.status(), StatusCode::FORBIDDEN, "{path}");
        let page = fixture
            .client
            .get(&url)
            .bearer_auth(&fixture.token)
            .header("x-labweaver-resource-delegation", &fixture.delegation)
            .query(&[("limit", "1")])
            .send()
            .await?;
        assert_eq!(page.status(), StatusCode::OK, "{path}");
        let page: Value = page.json().await?;
        assert!(page["items"].is_array(), "{path}");
        assert!(page.get("nextCursor").is_some(), "{path}");
        assert!(page.get("page").is_none() && page.get("hasMore").is_none());
        for query in [[("limit", "0")], [("cursor", "forged")]] {
            let invalid = fixture
                .client
                .get(&url)
                .bearer_auth(&fixture.token)
                .header("x-labweaver-resource-delegation", &fixture.delegation)
                .query(&query)
                .send()
                .await?;
            assert_eq!(invalid.status(), StatusCode::BAD_REQUEST, "{path}");
            let problem: contracts::ProblemDetails = invalid.json().await?;
            assert_eq!(
                problem.diagnostic_code.as_str(),
                "LW_RESOURCE_REQUEST_CURSOR_INVALID"
            );
        }
        let obsolete = fixture
            .client
            .get(&url)
            .bearer_auth(&fixture.token)
            .header("x-labweaver-resource-delegation", &fixture.delegation)
            .query(&[("page", "1"), ("pageSize", "25")])
            .send()
            .await?;
        assert_eq!(
            obsolete.status(),
            StatusCode::BAD_REQUEST,
            "old offset query: {path}"
        );
    }
    let now = time::OffsetDateTime::now_utc();
    let session = auth::BffSession {
        session_id: Uuid::now_v7(),
        actor_id: fixture.request.requester_id.as_uuid(),
        roles: vec![PlatformRole::Student],
        authorization_revision: 1,
        expires_at: now + time::Duration::minutes(5),
        idle_expires_at: now + time::Duration::minutes(5),
        csrf_token: auth::CsrfToken::from_secret("pagination-session".into()),
    };
    let member = auth::encode_resource_delegation(&[9_u8; 32], &session, Some(project), now)?;
    for path in [&paths[0], &paths[1], &paths[5], &paths[6]] {
        let denied = fixture
            .client
            .get(format!("{}{path}", fixture.base))
            .bearer_auth(&fixture.token)
            .header("x-labweaver-resource-delegation", &member)
            .send()
            .await?;
        assert_eq!(denied.status(), StatusCode::FORBIDDEN, "admin list: {path}");
    }
    for tail in ["credits/ledger", "credits/holds"] {
        let denied = fixture
            .client
            .get(format!(
                "{}/api/v1/projects/{}/{tail}",
                fixture.base,
                ProjectId::new()
            ))
            .bearer_auth(&fixture.token)
            .header("x-labweaver-resource-delegation", &member)
            .send()
            .await?;
        assert_eq!(
            denied.status(),
            StatusCode::FORBIDDEN,
            "cross-project: {tail}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn resource_list_ordinal_migration_backfills_existing_immutable_ledger_and_holds() -> Result {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        ))
        .await?;
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
    let catalog = persistence_sqlx::MigrationCatalog::load(&root.join("catalog.yaml"))?;
    let resource = catalog
        .domains
        .iter()
        .find(|domain| domain.name == persistence_sqlx::Domain::Resource)
        .ok_or("resource catalog missing")?;
    let mut ddl = "CREATE SCHEMA resource; SET search_path TO resource;\n".to_owned();
    for migration in resource
        .migrations
        .iter()
        .filter(|migration| migration.id < 24)
    {
        let sql = persistence_sqlx::MigrationCatalog::read_verified_sql(&root, migration)?;
        ddl.push_str(&sql);
        ddl.push('\n');
    }
    sqlx::raw_sql(sqlx::AssertSqlSafe(ddl))
        .execute(&pool)
        .await?;
    let store = PgResourceStore::new(pool.clone());
    let request = outbox_request()?;
    store
        .create("pre-pagination", &request, "pre-pagination")
        .await?;
    let credits = resource_service::credits::PgCreditStore::new(pool.clone());
    credits
        .admin(
            "pre-pagination",
            request.requester_id,
            &[PlatformRole::PlatformAdmin],
            &CreditAdminOperation::Grant {
                project_id: request.project_id,
                amount: FixedDecimal::parse("10.000000")?,
                reason: "existing immutable finance".into(),
            },
        )
        .await?;
    let hold = credits
        .hold(
            "pre-pagination",
            "pagination-owner",
            &CreditHoldRequest {
                project_id: request.project_id,
                request_id: request.id,
                business_key: "pre-pagination".into(),
                amount: FixedDecimal::parse("1.000000")?,
            },
        )
        .await?;
    sqlx::raw_sql(include_str!(
        "../../../../migrations/resource/0024_resource_list_ordinals.sql"
    ))
    .execute(&pool)
    .await?;
    let first = store
        .list_credit_ledger(request.project_id, &ResourceListQuery::default())
        .await?;
    assert_eq!(
        first.items.len(),
        2,
        "grant and hold ledger rows survive backfill"
    );
    assert_eq!(
        store
            .list_credit_holds(request.project_id, &ResourceListQuery::default())
            .await?
            .items[0]
            .id,
        hold.id
    );
    assert_eq!(
        store
            .list_all_requests(&ResourceListQuery::default())
            .await?
            .items[0]
            .id,
        request.id
    );
    let maximum: i64 =
        sqlx::query_scalar("SELECT max(ordinal) FROM resource.credit_ledger_entries")
            .fetch_one(&pool)
            .await?;
    credits
        .admin(
            "post-pagination",
            request.requester_id,
            &[PlatformRole::PlatformAdmin],
            &CreditAdminOperation::Grant {
                project_id: request.project_id,
                amount: FixedDecimal::parse("1.000000")?,
                reason: "post-migration insertion".into(),
            },
        )
        .await?;
    let new_minimum: i64 = sqlx::query_scalar(
        "SELECT min(ordinal) FROM resource.credit_ledger_entries WHERE ordinal>$1",
    )
    .bind(maximum)
    .fetch_one(&pool)
    .await?;
    assert!(new_minimum > maximum);
    assert_eq!(
        store
            .list_credit_ledger(request.project_id, &ResourceListQuery::default())
            .await?
            .items
            .len(),
        3
    );
    Ok(())
}
