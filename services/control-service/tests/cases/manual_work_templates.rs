//! Model-free publication uses real Control transactions, `PostgreSQL` and Resource admission.
use super::*;
use contracts::http::{ManualWorkTemplateRequest, ProjectAgentCapability};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn manual_runtime_options() -> TestResult<contracts::manual_templates::ManualTemplateRuntimeOptions>
{
    Ok(serde_json::from_value(serde_json::json!({"providers":[
        {"binding":"container-primary-v1","label":"教学容器","description":"配置中的容器位置","runtimeKind":"container","storageClasses":[]},
        {"binding":"kubevirt-primary-v1","label":"教学虚拟机","description":"配置中的虚拟机位置","runtimeKind":"virtual_machine","storageClasses":[{"binding":"vm-rwo-primary-v1","label":"教学磁盘","description":"最大 128 GiB","maxCapacityBytes":137_438_953_472_u64,"seededDisks":[]}]}
    ]}))?)
}

fn assert_environment_disk_resolution(
    release: &EnvironmentTemplateRelease,
    seeded: Option<&contracts::manual_templates::ManualTemplateSeededDiskOption>,
) -> TestResult {
    use environment_service::{
        KubeVirtBaseDiskBinding, KubeVirtBaseDiskIdentity, KubeVirtProviderConfiguration,
        KubeVirtResourceBudget, KubeVirtSshBootstrap, RuntimeVmBasePolicy,
    };
    let ImageArtifact::VirtualMachine {
        base_disk, format, ..
    } = &release.artifact
    else {
        return Err("expected VM artifact".into());
    };
    let seed = seeded
        .map(|seed| {
            KubeVirtBaseDiskBinding::new(
                seed.binding.clone(),
                seed.source_registry_digest.clone(),
                seed.disk_sha256.clone(),
                seed.capacity_bytes,
                seed.format,
                seed.storage_class_binding.clone(),
                "local-path".to_owned(),
                "labweaver-system".to_owned(),
                "reviewed-base".to_owned(),
                "lab".to_owned(),
                22,
            )
        })
        .transpose()?;
    let configuration = KubeVirtProviderConfiguration {
        trust_revision: Revision::new(1)?,
        base_disks: seed.into_iter().collect(),
        runtime_vm_base: Some(RuntimeVmBasePolicy::new(
            "vm-rwo-primary-v1".to_owned(),
            "local-path".to_owned(),
            "labweaver-system".to_owned(),
            "lab".to_owned(),
            22,
            8,
            137_438_953_472,
        )?),
        vm_vgpu_licensing: None,
        ssh: KubeVirtSshBootstrap::new(
            "labweaver-system".to_owned(),
            "gateway".to_owned(),
            "labweaver-system".to_owned(),
            "collector".to_owned(),
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDIhz2GK/XCUj4i6Q5yQJNL1MKDXETe1aM1lHYMGt2SQ",
        )?,
        resource_budget: KubeVirtResourceBudget::new(1, 1, 1, 1, 1, 1)?,
    };
    let resolved = configuration.resolve_base_disk(base_disk, *format)?;
    assert_eq!(
        resolved.binding.source_registry_digest,
        base_disk.source_registry_digest
    );
    assert_eq!(resolved.binding.capacity_bytes, base_disk.capacity_bytes);
    assert_eq!(
        resolved.identity,
        if seeded.is_some() {
            KubeVirtBaseDiskIdentity::ReviewedDiskSha256
        } else {
            KubeVirtBaseDiskIdentity::RuntimeRegistryDigest
        }
    );
    Ok(())
}

fn manual_spec(kind: RuntimeKind, image: &PlatformImageEntry) -> TestResult<EnvironmentSpec> {
    let runtime = match kind {
        RuntimeKind::Container => serde_json::json!({
            "kind":"container", "provider_binding":"container-primary-v1", "service_port":8080,
            "terminal":{"executable":"/bin/sh","args":[],"workingDirectory":"/workspace"}
        }),
        RuntimeKind::VirtualMachine => serde_json::json!({
            "kind":"virtual_machine", "provider_binding":"kubevirt-primary-v1",
            "storage_class_binding":"vm-rwo-primary-v1", "ssh_port":22,
            "base_disk":{"binding":image.binding,"sourceRegistryDigest":format!("docker://harbor.internal/labweaver-system/manual@{}",image.resolved_digest),"capacityBytes":image.capacity_bytes}
        }),
    };
    Ok(serde_json::from_value(serde_json::json!({
        "apiVersion":"environment.labweaver.io/v1", "kind":"EnvironmentSpec",
        "name":"无模型教学工作环境", "class":"work",
        "resources":{"cpuMillicores":1000,"memoryBytes":1_073_741_824_u64,"storageBytes":10_737_418_240_u64},
        "network":{"mode":"deny_all"},
        "entries":[{"name":"entry","protocol":if kind==RuntimeKind::Container {"http"} else {"ssh"},"servicePort":if kind==RuntimeKind::Container {8080} else {22}}],
        "security":{"runAsUser":null,"readOnlyRootFilesystem":false,"allowPrivilegeEscalation":false,"publicExposurePolicy":"deny","securityProfileBinding":"restricted-v1"},
        "runtime":runtime,
        "retention":{"policyId":PolicyId::new(),"policyRevision":1,"class":"run_evidence","retainUntil":"2026-08-16T08:00:00.000Z","disposition":"delete"}
    }))?)
}

fn catalog_image(kind: RuntimeKind, now: UtcTimestamp) -> PlatformImageEntry {
    PlatformImageEntry {
        catalog_id: PlatformImageId::new(),
        kind: if kind == RuntimeKind::Container {
            PlatformImageKind::Container
        } else {
            PlatformImageKind::VirtualMachine
        },
        binding: "manual-202".to_owned(),
        source_reference: "harbor.internal/labweaver-system/manual:202".to_owned(),
        resolved_digest: format!("sha256:{}", "a".repeat(64)),
        media_type: "application/vnd.oci.image.manifest.v1+json".to_owned(),
        size_bytes: 1024,
        status: PlatformImageStatus::Active,
        trust_revision: 1,
        repin_generation: 1,
        capacity_bytes: (kind == RuntimeKind::VirtualMachine).then_some(10_737_418_240),
        disk_sha256: (kind == RuntimeKind::VirtualMachine).then(|| "b".repeat(64)),
        format: (kind == RuntimeKind::VirtualMachine).then_some(VirtualMachineDiskFormat::Qcow2),
        pinned_at: now,
        updated_at: now,
    }
}

struct ManualPublication {
    service: ControlService,
    project: Project,
    request: CreateEnvironmentTemplateReleaseRequest,
    image: PlatformImageEntry,
    now: UtcTimestamp,
}

impl ManualPublication {
    async fn new(pool: &sqlx::PgPool, kind: RuntimeKind) -> TestResult<Self> {
        let service = ControlService::new(
            pool.clone(),
            Arc::new(FixtureObjects { fail_second: false }),
            control_config()?,
        )?;
        let now: UtcTimestamp = "2026-07-16T08:00:00.000Z".parse()?;
        let project = service
            .create_project(
                ActorId::new(),
                contracts::PlatformRole::Teacher,
                &contracts::CreateProjectRequest {
                    name: "manual-work".to_owned(),
                    description: None,
                    course_id: None,
                },
                &IdempotencyKey::parse("manual-work-project")?,
                now,
            )
            .await?;
        let image = catalog_image(kind, now);
        let request = CreateEnvironmentTemplateReleaseRequest {
            project_id: project.id,
            course_id: None,
            candidate_id: None,
            candidate_revision: None,
            runtime_kind: kind,
            approval_id: None,
            manual: Some(Box::new(ManualWorkTemplateRequest {
                platform_image_id: image.catalog_id,
                platform_image_generation: 1,
                template_family_id: "manual-cpu-work".to_owned(),
                description: "手工审核的 CPU Work".to_owned(),
                spec: manual_spec(kind, &image)?,
                reason: "教师审核此固定镜像与配置".to_owned(),
                confirmed: true,
            })),
        };
        let mut request = request;
        let retention = service
            .manual_template_options(manual_runtime_options()?)?
            .retention;
        let spec = &mut request.manual.as_mut().ok_or("manual request")?.spec;
        spec.retention.policy_id = retention.policy_id;
        spec.retention.retain_until = Some(UtcTimestamp::from_utc(
            now.get() + time::Duration::hours(12),
        )?);
        Ok(Self {
            service,
            project,
            request,
            image,
            now,
        })
    }

    async fn publish(
        &self,
        actor: ActorId,
        key: &str,
        request: &CreateEnvironmentTemplateReleaseRequest,
    ) -> Result<EnvironmentTemplateRelease, ControlError> {
        let key = IdempotencyKey::parse(key).map_err(|_| ControlError::ContractInvalid)?;
        self.service
            .create_project_work_release(
                self.project.id,
                request,
                actor,
                false,
                &key,
                self.now,
                "manual-work-publish",
                std::slice::from_ref(&self.image),
                &manual_runtime_options().map_err(|_| ControlError::ContractInvalid)?,
            )
            .await
    }

    async fn assert_rejections(&self, release: &EnvironmentTemplateRelease) -> TestResult {
        let owner = self.project.owner_actor_id;
        let outsider = ActorId::new();
        assert!(matches!(
            self.publish(outsider, "manual-work-publish", &self.request)
                .await,
            Err(ControlError::ProjectGovernanceDenied)
        ));
        assert!(
            self.service
                .project_release(self.project.id, release.id, None, outsider)
                .await
                .is_err()
        );
        assert!(
            self.service
                .project_release(ProjectId::new(), release.id, None, owner)
                .await
                .is_err()
        );
        let mut changed = self.request.clone();
        "changed".clone_into(
            &mut changed
                .manual
                .as_mut()
                .ok_or("missing manual request")?
                .description,
        );
        assert!(matches!(
            self.publish(owner, "manual-work-publish", &changed).await,
            Err(ControlError::IdempotencyConflict)
        ));
        let mut unconfirmed = self.request.clone();
        unconfirmed
            .manual
            .as_mut()
            .ok_or("missing manual request")?
            .confirmed = false;
        assert!(matches!(
            self.publish(owner, "manual-unconfirmed", &unconfirmed)
                .await,
            Err(ControlError::ContractInvalid)
        ));
        let mut disabled = self.image.clone();
        disabled.status = PlatformImageStatus::Disabled;
        assert!(matches!(
            self.service
                .create_project_work_release(
                    self.project.id,
                    &self.request,
                    owner,
                    false,
                    &IdempotencyKey::parse("manual-disabled-image")?,
                    self.now,
                    "manual-work-publish",
                    &[disabled],
                    &manual_runtime_options()?,
                )
                .await,
            Err(ControlError::ArtifactNotAuthoritative)
        ));
        Ok(())
    }

    async fn assert_model_read_failures(&self, pool: &sqlx::PgPool) -> TestResult {
        let policy = authoring_policy(self.project.id, None, self.now)?;
        self.service
            .activate_project_policy(
                self.project.id,
                policy,
                &IdempotencyKey::parse("manual-model-configured")?,
                None,
            )
            .await?;
        assert!(matches!(
            self.service
                .project_agent_capability(self.project.id)
                .await?,
            ProjectAgentCapability::Configured { .. }
        ));
        sqlx::query(
            "UPDATE control.project_llm_policies SET contract='{}'::jsonb WHERE project_id=$1",
        )
        .bind(self.project.id.as_uuid())
        .execute(pool)
        .await?;
        assert!(
            self.service
                .project_agent_capability(self.project.id)
                .await
                .is_err(),
            "unreadable configuration is a failure, not unconfigured"
        );
        Ok(())
    }
}

async fn assert_publication_facts(
    pool: &sqlx::PgPool,
    fixture: &ManualPublication,
    release: &EnvironmentTemplateRelease,
) -> TestResult {
    let invalid = sqlx::query("INSERT INTO control.environment_template_releases (release_id,project_id,course_id,version,spec_sha256,contract,published_at) VALUES($1,$2,NULL,99,$3,'{}'::jsonb,$4)")
        .bind(ReleaseId::new().as_uuid()).bind(fixture.project.id.as_uuid())
        .bind(Sha256Digest::of_bytes(b"no-release-evidence").to_string()).bind(fixture.now.get())
        .execute(pool).await.err().ok_or("missing release evidence was accepted")?;
    assert_eq!(
        invalid
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref(),
        Some("23514")
    );
    let facts:(i64,i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM control.candidates),(SELECT count(*) FROM control.agent_run_projections),(SELECT count(*) FROM control.project_llm_policies),(SELECT count(*) FROM control.outbox_events WHERE subject=$1)")
        .bind(contracts::events::subjects::ENVIRONMENT_TEMPLATE_RELEASE_PUBLISHED).fetch_one(pool).await?;
    assert_eq!(
        facts,
        (0, 0, 0, 1),
        "publication cannot invent Agent/model evidence or duplicate release events"
    );
    let event: serde_json::Value =
        sqlx::query_scalar("SELECT payload FROM control.outbox_events WHERE aggregate_id=$1")
            .bind(release.id.as_uuid())
            .fetch_one(pool)
            .await?;
    let event: CloudEvent<contracts::events::ReleasePublished> = serde_json::from_value(event)?;
    event.data.validate()?;
    assert_eq!(
        event.data.environment_spec,
        fixture
            .request
            .manual
            .as_ref()
            .ok_or("manual request")?
            .spec
    );
    assert_eq!(
        fixture
            .service
            .platform_image_release_references()
            .await?
            .get(&fixture.image.resolved_digest),
        Some(&1)
    );
    Ok(())
}

async fn publish_without_model(kind: RuntimeKind) -> TestResult {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        container.get_host_port_ipv4(5432).await?
    );
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await?;
    apply_domain_migrations(&pool, Domain::Control).await?;
    let fixture = ManualPublication::new(&pool, kind).await?;
    let owner = fixture.project.owner_actor_id;
    assert!(
        matches!(fixture.service.project_agent_capability(fixture.project.id).await?,ProjectAgentCapability::Unconfigured { reason_code } if reason_code=="LW_AGENT_MODEL_NOT_CONFIGURED")
    );
    let release = fixture
        .publish(owner, "manual-work-publish", &fixture.request)
        .await?;
    assert_eq!(release.candidate_id, None);
    assert_eq!(release.agent_run_id, None);
    assert_eq!(release.approval, None);
    assert_eq!(
        release
            .manual_approval
            .as_ref()
            .ok_or("missing human approval")?
            .approved_by,
        owner
    );
    assert_eq!(
        fixture
            .publish(owner, "manual-work-publish", &fixture.request)
            .await?,
        release
    );
    fixture.assert_rejections(&release).await?;
    let view = fixture
        .service
        .project_release(fixture.project.id, release.id, None, owner)
        .await?;
    assert_eq!(view.name, "无模型教学工作环境");
    assert_eq!(view.template_family_id, "manual-cpu-work");
    assert_eq!(view.purpose, EnvironmentClass::Work);
    assert_eq!(
        view.capabilities
            .contains(&contracts::supply_chain::TemplateCapability::Terminal),
        kind == RuntimeKind::Container
    );
    assert_eq!(
        view.capabilities
            .contains(&contracts::supply_chain::TemplateCapability::Ssh),
        kind == RuntimeKind::VirtualMachine
    );
    assert_eq!(
        fixture
            .service
            .project_releases(fixture.project.id, None, 0, 10, owner)
            .await?
            .items
            .len(),
        1
    );
    assert_publication_facts(&pool, &fixture, &release).await?;
    super::runtime_authorization_cases::assert_manual_work_binding(
        &pool,
        &release,
        owner,
        fixture.now,
    )
    .await?;
    fixture.assert_model_read_failures(&pool).await?;
    Ok(())
}

#[tokio::test]
async fn manual_container_publication_needs_no_model_and_preserves_permissions_and_idempotency()
-> TestResult {
    publish_without_model(RuntimeKind::Container).await
}

#[tokio::test]
async fn manual_vm_publication_needs_no_model_and_pins_the_reviewed_catalog_disk() -> TestResult {
    publish_without_model(RuntimeKind::VirtualMachine).await
}

#[tokio::test]
async fn ordinary_capability_publication_requires_platform_range_and_current_scope_grant()
-> TestResult {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        container.get_host_port_ipv4(5432).await?
    );
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await?;
    apply_domain_migrations(&pool, Domain::Control).await?;
    let mut fixture = ManualPublication::new(&pool, RuntimeKind::Container).await?;
    let owner = fixture.project.owner_actor_id;
    let security = fixture
        .request
        .manual
        .as_ref()
        .ok_or("manual")?
        .spec
        .security
        .clone();
    for (index, rejected) in [
        contracts::authoring::EnvironmentSecuritySpec {
            privileged: true,
            ..security.clone()
        },
        contracts::authoring::EnvironmentSecuritySpec {
            capabilities: std::collections::BTreeSet::from([
                contracts::platform_configuration::LinuxCapability::SysAdmin,
            ]),
            ..security.clone()
        },
        contracts::authoring::EnvironmentSecuritySpec {
            run_as_user: Some(1000),
            allow_privilege_escalation: true,
            ..security
        },
    ]
    .into_iter()
    .enumerate()
    {
        let mut invalid = fixture.request.clone();
        invalid.manual.as_mut().ok_or("manual")?.spec.security = rejected;
        assert!(matches!(
            fixture
                .publish(owner, &format!("unsupported-security-{index}"), &invalid)
                .await,
            Err(ControlError::ContractInvalid)
        ));
    }
    let releases: i64 =
        sqlx::query_scalar("SELECT count(*) FROM control.environment_template_releases")
            .fetch_one(&pool)
            .await?;
    assert_eq!(
        releases, 0,
        "rejected permissions must not publish or enqueue a release"
    );
    let spec = &mut fixture.request.manual.as_mut().ok_or("manual")?.spec;
    spec.security
        .capabilities
        .insert(contracts::platform_configuration::LinuxCapability::NetAdmin);
    let mut encoded = serde_json::to_value(&*spec)?;
    encoded["security"]["capabilities"] = serde_json::json!(["UNKNOWN_CAPABILITY"]);
    assert!(serde_json::from_value::<EnvironmentSpec>(encoded).is_err());
    let release = fixture
        .publish(owner, "ordinary-net-admin", &fixture.request)
        .await?;
    super::runtime_authorization_cases::assert_manual_work_binding(
        &pool,
        &release,
        owner,
        fixture.now,
    )
    .await?;
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "one database fixture checks authority, rejects forged choices without effects, and resolves accepted disk identities through Environment"
)]
#[tokio::test]
async fn manual_options_require_publication_authority_and_reject_forged_choices() -> TestResult {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        ))
        .await?;
    apply_domain_migrations(&pool, Domain::Control).await?;
    let fixture = ManualPublication::new(&pool, RuntimeKind::VirtualMachine).await?;
    let owner = fixture.project.owner_actor_id;
    fixture
        .service
        .require_manual_template_publisher(fixture.project.id, owner, false)
        .await?;
    fixture
        .service
        .require_manual_template_publisher(fixture.project.id, ActorId::new(), true)
        .await?;
    for (role, object_role, allowed) in [
        ("student", "viewer", false),
        ("teacher", "editor", false),
        ("teacher", "manager", true),
    ] {
        let actor = ActorId::new();
        sqlx::query("INSERT INTO control.project_memberships(project_id,actor_id,role,object_role,state,revision) VALUES($1,$2,$3,$4,'active',1)")
            .bind(fixture.project.id.as_uuid()).bind(actor.as_uuid()).bind(role).bind(object_role).execute(&pool).await?;
        let result = fixture
            .service
            .require_manual_template_publisher(fixture.project.id, actor, false)
            .await;
        assert_eq!(result.is_ok(), allowed, "{role}/{object_role}");
    }
    let options = fixture
        .service
        .manual_template_options(manual_runtime_options()?)?;
    assert_eq!(
        options.retention.policy_id,
        fixture
            .request
            .manual
            .as_ref()
            .ok_or("manual request")?
            .spec
            .retention
            .policy_id
    );
    assert_eq!(
        options.retention.max_retention_seconds,
        control_config()?.retention_seconds
    );
    let mut colliding = manual_runtime_options()?;
    colliding.providers[1].storage_classes[0].seeded_disks.push(
        contracts::manual_templates::ManualTemplateSeededDiskOption {
            binding: fixture.image.binding.clone(),
            disk_sha256: "b".repeat(64),
            capacity_bytes: fixture
                .image
                .capacity_bytes
                .ok_or("VM catalog image capacity")?,
            source_registry_digest: format!(
                "docker://harbor.internal/labweaver-system/manual@{}",
                fixture.image.resolved_digest
            ),
            format: contracts::supply_chain::VirtualMachineDiskFormat::Qcow2,
            storage_class_binding: "vm-rwo-primary-v1".to_owned(),
        },
    );
    let seed = &colliding.providers[1].storage_classes[0].seeded_disks[0];
    let mut conflicts = Vec::new();
    let mut disk_hash = seed.clone();
    disk_hash.disk_sha256 = "a".repeat(64);
    conflicts.push(("disk_sha256", disk_hash));
    let mut source = seed.clone();
    source.source_registry_digest = format!(
        "docker://harbor.internal/labweaver-system/manual@sha256:{}",
        "c".repeat(64)
    );
    conflicts.push(("source_digest", source));
    let mut capacity = seed.clone();
    capacity.capacity_bytes += 1;
    conflicts.push(("capacity", capacity));
    let mut format = seed.clone();
    format.format = contracts::supply_chain::VirtualMachineDiskFormat::Raw;
    conflicts.push(("format", format));
    let mut storage = seed.clone();
    storage.storage_class_binding = "other-storage".to_owned();
    conflicts.push(("storage_binding", storage));
    for (field, conflict) in conflicts {
        let mut options = colliding.clone();
        options.providers[1].storage_classes[0].seeded_disks[0] = conflict;
        let result = fixture
            .service
            .create_project_work_release(
                fixture.project.id,
                &fixture.request,
                owner,
                false,
                &IdempotencyKey::parse(&format!("seeded-conflict-{field}"))?,
                fixture.now,
                "seeded-image-conflict",
                std::slice::from_ref(&fixture.image),
                &options,
            )
            .await;
        assert!(
            matches!(result, Err(ControlError::ManualTemplateOptionInvalid)),
            "independent seeded identity conflict: {field}; got {result:?}"
        );
    }
    for (index, pointer, value) in [
        (
            0,
            "/manual/spec/runtime/provider_binding",
            serde_json::json!("unknown-provider"),
        ),
        (
            1,
            "/manual/spec/runtime/storage_class_binding",
            serde_json::json!("unknown-storage"),
        ),
        (
            2,
            "/manual/spec/retention/policyId",
            serde_json::json!(PolicyId::new()),
        ),
        (
            3,
            "/manual/spec/retention/policyRevision",
            serde_json::json!(2),
        ),
        (
            4,
            "/manual/spec/retention/retainUntil",
            serde_json::json!("2027-07-16T08:00:00.000Z"),
        ),
    ] {
        let mut invalid = serde_json::to_value(&fixture.request)?;
        *invalid.pointer_mut(pointer).ok_or("invalid test pointer")? = value;
        let invalid = serde_json::from_value(invalid)?;
        assert!(
            matches!(
                fixture
                    .publish(owner, &format!("unknown-choice-{index}"), &invalid)
                    .await,
                Err(ControlError::ManualTemplateOptionInvalid)
            ),
            "{pointer}"
        );
    }
    let counts: (i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM control.environment_template_releases),(SELECT count(*) FROM control.outbox_events WHERE subject=$1)")
        .bind(contracts::events::subjects::ENVIRONMENT_TEMPLATE_RELEASE_PUBLISHED).fetch_one(&pool).await?;
    assert_eq!(counts, (0, 0));
    let release = fixture
        .service
        .create_project_work_release(
            fixture.project.id,
            &fixture.request,
            owner,
            false,
            &IdempotencyKey::parse("seeded-image-matches")?,
            fixture.now,
            "seeded-image-matches",
            std::slice::from_ref(&fixture.image),
            &colliding,
        )
        .await?;
    assert_environment_disk_resolution(
        &release,
        Some(&colliding.providers[1].storage_classes[0].seeded_disks[0]),
    )?;
    assert_eq!(
        fixture
            .service
            .replay_project_work_release(
                fixture.project.id,
                &fixture.request,
                owner,
                false,
                &IdempotencyKey::parse("seeded-image-matches")?,
            )
            .await?,
        Some(release)
    );
    let unseeded = fixture
        .publish(owner, "valid-options", &fixture.request)
        .await?;
    assert_environment_disk_resolution(&unseeded, None)?;
    Ok(())
}
