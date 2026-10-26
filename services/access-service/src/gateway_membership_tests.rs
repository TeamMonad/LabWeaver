//! Production-role coverage for SSH redemption snapshots and heartbeat-bounded revocation.

use super::*;

#[path = "../../control-service/tests/support/production_roles.rs"]
mod production_roles;
use production_roles::ProductionRoles;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[expect(
    clippy::needless_pass_by_value,
    reason = "owned Result::map_err callback"
)]
fn api_error(error: ApiError) -> std::io::Error {
    std::io::Error::other(format!("{} ({:?})", error.diagnostic, error.status))
}

struct Candidate {
    actor: Uuid,
    project: Uuid,
    course: Option<Uuid>,
    key: Uuid,
    grant: Uuid,
    endpoint: Uuid,
    endpoint_grant: Uuid,
    environment: Uuid,
    alias: &'static str,
    now: OffsetDateTime,
}

impl Candidate {
    async fn seed(roles: &ProductionRoles, course_associated: bool) -> TestResult<Self> {
        let candidate = Self {
            actor: Uuid::now_v7(),
            project: Uuid::now_v7(),
            course: course_associated.then(Uuid::now_v7),
            key: Uuid::now_v7(),
            grant: Uuid::now_v7(),
            endpoint: Uuid::now_v7(),
            endpoint_grant: Uuid::now_v7(),
            environment: Uuid::now_v7(),
            alias: if course_associated {
                "lw-bcdefghijklmnopqrstu"
            } else {
                "lw-abcdefghijklmnopqrst"
            },
            now: sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
                .fetch_one(&roles.provisioner)
                .await?,
        };
        sqlx::query("INSERT INTO access.actors(actor_id,issuer,subject_sha256) VALUES($1,'https://issuer.example.test',$2)")
            .bind(candidate.actor).bind(sha256_hex(candidate.actor.as_bytes())).execute(&roles.access).await?;
        sqlx::query("INSERT INTO access.ssh_public_keys(key_id,actor_id,fingerprint_sha256,algorithm,normalized_openssh) VALUES($1,$2,$3,'ed25519','ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA')")
            .bind(candidate.key).bind(candidate.actor).bind(format!("SHA256:{}", if course_associated { "B" } else { "A" }.repeat(43)))
            .execute(&roles.access).await?;
        sqlx::query("INSERT INTO control.project_memberships(project_id,actor_id,course_id,role,object_role,state,revision) VALUES($1,$2,$3,'student','editor','active',1)")
            .bind(candidate.project).bind(candidate.actor).bind(candidate.course).execute(&roles.control).await?;
        if let Some(course) = candidate.course {
            sqlx::query("INSERT INTO control.course_memberships(course_id,actor_id,role,object_role,state,revision,expires_at) VALUES($1,$2,'student','participant','active',1,$3)")
                .bind(course).bind(candidate.actor).bind(candidate.now + time::Duration::minutes(30))
                .execute(&roles.control).await?;
        }
        sqlx::query("INSERT INTO access.access_grants(grant_id,actor_id,project_id,course_id,environment_id,revision,state,not_before,expires_at,contract) VALUES($1,$2,$3,$4,$5,1,'active',$6,$7,'{\"subjectKind\":\"owner\"}')")
            .bind(candidate.grant).bind(candidate.actor).bind(candidate.project).bind(candidate.course)
            .bind(candidate.environment).bind(candidate.now - time::Duration::minutes(1))
            .bind(candidate.now + time::Duration::minutes(30)).execute(&roles.access).await?;
        sqlx::query("INSERT INTO access.endpoint_grants(endpoint_grant_id,grant_id,endpoint_id,endpoint_revision,protocol,health,alias,expires_at,contract) VALUES($1,$2,$3,1,'ssh','healthy',$4,$5,'{}')")
            .bind(candidate.endpoint_grant).bind(candidate.grant).bind(candidate.endpoint).bind(candidate.alias)
            .bind(candidate.now + time::Duration::minutes(30)).execute(&roles.access).await?;
        Ok(candidate)
    }

    async fn revoke(&self, roles: &ProductionRoles) -> TestResult {
        // This independent Control transaction commits while Access still holds its own row locks.
        let result = if let Some(course) = self.course {
            sqlx::query("UPDATE control.course_memberships SET state='revoked',revision=revision+1 WHERE course_id=$1 AND actor_id=$2")
                .bind(course).bind(self.actor).execute(&roles.control).await?
        } else {
            sqlx::query("UPDATE control.project_memberships SET state='revoked',revision=revision+1 WHERE project_id=$1 AND actor_id=$2")
                .bind(self.project).bind(self.actor).execute(&roles.control).await?
        };
        assert_eq!(result.rows_affected(), 1);
        Ok(())
    }
}

#[tokio::test]
async fn restricted_access_runtime_redeems_project_and_course_candidates() -> TestResult {
    let roles = ProductionRoles::start().await?;
    for associated in [false, true] {
        let candidate = Candidate::seed(&roles, associated).await?;
        let mut tx = roles.access.begin().await?;
        let result = load_gateway_session_candidate(
            &mut tx,
            candidate.alias,
            candidate.key,
            candidate.actor,
            candidate.now,
        )
        .await;
        let (row, course_expiry) = match result {
            Ok(value) => value,
            Err(error) => {
                return Err(std::io::Error::other(format!(
                    "real redemption helper failed (course={associated}): {error:?}\n{}",
                    roles.database_errors().await?
                ))
                .into());
            }
        };
        assert_eq!(row.get::<Uuid, _>("grant_id"), candidate.grant);
        assert_eq!(row.get::<Option<Uuid>, _>("course_id"), candidate.course);
        assert_eq!(course_expiry.is_some(), associated);
        if let Some(course) = candidate.course {
            let expiry = load_course_membership(
                &mut tx,
                course,
                candidate.actor,
                EnvironmentAccessSubjectKind::Owner,
                candidate.now,
            )
            .await
            .map_err(api_error)?;
            assert_eq!(expiry, course_expiry);
        }
        tx.commit().await?;
    }
    Ok(())
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "deterministically interleaves redemption, revoke, heartbeat and close for both membership scopes"
)]
async fn redemption_snapshot_revocation_is_caught_by_next_heartbeat_authority_check() -> TestResult
{
    let roles = ProductionRoles::start().await?;
    for associated in [false, true] {
        let candidate = Candidate::seed(&roles, associated).await?;
        let mut tx = roles.access.begin().await?;
        let (row, _) = load_gateway_session_candidate(
            &mut tx,
            candidate.alias,
            candidate.key,
            candidate.actor,
            candidate.now,
        )
        .await
        .map_err(api_error)?;
        assert_eq!(row.get::<Uuid, _>("grant_id"), candidate.grant);
        // Deterministic window: the real redemption helper succeeded, but the session isn't inserted.
        candidate.revoke(&roles).await?;
        let session_id = GatewaySessionId::new();
        let connection = session_id.to_string();
        sqlx::query(
            "INSERT INTO access.gateway_sessions \
             (session_id,grant_id,grant_revision,actor_id,endpoint_id,endpoint_grant_id,key_id,state,\
              started_at,expires_at,contract,gateway_identity,connection_id,revision,last_heartbeat_at) \
             VALUES($1,$2,1,$3,$4,$5,$6,'active',$7,$8,$9,'gateway-runtime',$10,1,$7)",
        ).bind(session_id.as_uuid()).bind(candidate.grant).bind(candidate.actor)
            .bind(candidate.endpoint).bind(candidate.endpoint_grant).bind(candidate.key)
            .bind(candidate.now).bind(candidate.now + time::Duration::seconds(30))
            .bind(json!({"alias":candidate.alias,"targetHost":format!("ssh.lw-env-{}.svc",candidate.environment)}))
            .bind(&connection).execute(&mut *tx).await?;
        tx.commit().await?;
        assert_eq!(
            load_session(&roles.access, session_id)
                .await
                .map_err(api_error)?
                .state,
            GatewaySessionState::Active
        );

        // Drive the exact authority check called by heartbeat_gateway_session, without a timer/sleep.
        if let Err(error) = super::super::object_authorization::recheck_gateway(
            &roles.access,
            session_id,
            "gateway-runtime",
            &connection,
        )
        .await
        {
            return Err(std::io::Error::other(format!(
                "heartbeat authority check failed (course={associated}): {error:?}\n{}",
                roles.database_errors().await?
            ))
            .into());
        }
        let terminating = load_session(&roles.access, session_id)
            .await
            .map_err(api_error)?;
        assert_eq!(terminating.state, GatewaySessionState::Terminating);
        assert!(terminating.revision > Revision::new(1)?);
        assert!(terminating.termination_requested_at.is_some());
        assert!(terminating.terminate_by.is_some());
        let exact_bound: bool = sqlx::query_scalar(
            "SELECT state='terminating' AND terminate_by-termination_requested_at=interval '60 seconds' \
             FROM access.gateway_sessions WHERE session_id=$1",
        )
        .bind(session_id.as_uuid())
        .fetch_one(&roles.access)
        .await?;
        assert!(
            exact_bound,
            "persisted termination deadline must be exactly 60 seconds"
        );

        let mut next_tx = roles.access.begin().await?;
        let denied = load_gateway_session_candidate(
            &mut next_tx,
            candidate.alias,
            candidate.key,
            candidate.actor,
            candidate.now,
        )
        .await
        .err()
        .ok_or_else(|| std::io::Error::other("revoked membership admitted a new session"))?;
        assert_eq!(denied.diagnostic, "LW_ACCESS_SSH_DENIED");
        next_tx.rollback().await?;
        if let Some(course) = candidate.course {
            let mut tx = roles.access.begin().await?;
            let denied = load_course_membership(
                &mut tx,
                course,
                candidate.actor,
                EnvironmentAccessSubjectKind::Owner,
                candidate.now,
            )
            .await
            .err()
            .ok_or_else(|| {
                std::io::Error::other("revoked course membership passed the real reader")
            })?;
            assert_eq!(denied.diagnostic, "LW_ACCESS_SSH_DENIED");
            tx.rollback().await?;
        }
        // Gateway's non-active heartbeat result stops the target, then reports close with this revision.
        let closed_at = utc_timestamp(OffsetDateTime::now_utc()).map_err(api_error)?;
        let request = CloseGatewaySessionRequest {
            gateway_identity: "gateway-runtime".to_owned(),
            connection_id: connection,
            expected_revision: terminating.revision,
            closed_at,
            reason_code: "target_failed".to_owned(),
        };
        let closed = close_gateway_session_owned(
            &roles.access,
            session_id,
            "gateway-runtime",
            &request,
            request.expected_revision,
            closed_at.get(),
        )
        .await
        .map_err(api_error)?;
        assert_eq!(closed.state, GatewaySessionState::Closed);
        assert!(closed.closed_at.is_some());
        assert_eq!(
            load_session(&roles.access, session_id)
                .await
                .map_err(api_error)?
                .state,
            GatewaySessionState::Closed
        );
    }
    Ok(())
}
