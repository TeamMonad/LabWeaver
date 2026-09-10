//! `PostgreSQL` compare-and-consume operations for one-time OIDC state.

use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{CsrfToken, EncryptedValue, KeyRing, OidcTransaction, OidcTransactionError};
use contracts::{
    ActorId, CourseId, CourseMembership, MembershipState, PlatformRole, ProjectId,
    ProjectMembership, Revision, UtcTimestamp,
};

fn hex_sha256(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

/// Local actor row created from a verified OIDC issuer/subject pair.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalActor {
    /// Durable actor identity.
    pub actor_id: Uuid,
}

/// Server-side session data required to authenticate a browser request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BffSession {
    /// Opaque session identifier held by the browser cookie.
    pub session_id: Uuid,
    /// Durable actor identity.
    pub actor_id: Uuid,
    /// Base roles verified from the signed ID token at session creation.
    pub roles: Vec<PlatformRole>,
    /// Current authorization revision.
    pub authorization_revision: i64,
    /// Absolute expiration selected by the Access browser-session policy.
    /// This remains independent of the provider ID-token expiry after the
    /// callback has verified that token.
    pub expires_at: OffsetDateTime,
    /// Idle expiration managed by the Access authority.
    pub idle_expires_at: OffsetDateTime,
    /// Decrypted synchronizer token, never logged.
    pub csrf_token: CsrfToken,
}

/// All values needed to create a new rotated BFF session.
#[derive(Clone, Debug)]
pub struct CreateBffSession {
    /// Durable actor identity.
    pub actor_id: Uuid,
    /// Base roles authenticated by OIDC.
    pub roles: Vec<PlatformRole>,
    /// Current effective authorization revision.
    pub authorization_revision: i64,
    /// Absolute session expiry selected by Access from configured
    /// `session_ttl_seconds`, independent of the provider ID-token expiry.
    pub expires_at: OffsetDateTime,
    /// Idle timeout configured by deployment.
    pub idle_ttl: time::Duration,
    /// Optional OIDC provider session identifier.
    pub oidc_sid: Option<String>,
    /// Signed ID-token logout hint to encrypt server-side.
    pub logout_hint: String,
}

/// Computes the absolute local BFF session expiry from the configured Access
/// lifetime. The provider ID-token expiry is intentionally not an input: it is
/// validated during callback authentication, while this value governs the
/// local session after that event.
pub fn configured_session_expiry(
    issued_at: OffsetDateTime,
    session_ttl: time::Duration,
) -> Result<OffsetDateTime, RepositoryError> {
    if session_ttl <= time::Duration::ZERO {
        return Err(RepositoryError::SessionInvalid);
    }
    issued_at
        .checked_add(session_ttl)
        .filter(|expires_at| *expires_at > issued_at)
        .ok_or(RepositoryError::SessionInvalid)
}

/// Bounds an idle renewal by the already-selected absolute session expiry.
/// Renewals can move the idle deadline forward, but never extend the session's
/// absolute lifetime.
pub fn bounded_idle_expiry(
    now: OffsetDateTime,
    absolute_expiry: OffsetDateTime,
    idle_ttl: time::Duration,
) -> Result<OffsetDateTime, RepositoryError> {
    if absolute_expiry <= now || idle_ttl <= time::Duration::ZERO {
        return Err(RepositoryError::SessionInvalid);
    }
    let candidate = now
        .checked_add(idle_ttl)
        .ok_or(RepositoryError::SessionInvalid)?;
    let idle_expiry = std::cmp::min(candidate, absolute_expiry);
    if idle_expiry <= now {
        return Err(RepositoryError::SessionInvalid);
    }
    Ok(idle_expiry)
}

/// Authoritative memberships read for a single authorization decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MembershipSnapshot {
    /// Active, suspended, and revoked course memberships; callers apply state
    /// and expiry checks at the decision time.
    pub course_memberships: Vec<CourseMembership>,
    /// Active, suspended, and revoked project memberships.
    pub project_memberships: Vec<ProjectMembership>,
}

/// Counts returned by one bounded expired-auth-state cleanup transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthCleanupReport {
    /// Sessions newly marked expired.
    pub sessions_revoked: u64,
    /// Retained revoked sessions removed after the configured retention.
    pub sessions_deleted: u64,
    /// Expired OIDC transactions removed.
    pub transactions_deleted: u64,
    /// Expired logout replay reservations removed.
    pub logout_events_deleted: u64,
}

/// Cleans bounded ephemeral authentication state while retaining revoked
/// sessions for the deployment-configured audit window.
pub async fn cleanup_expired_auth_state(
    pool: &PgPool,
    now: OffsetDateTime,
    session_retention: time::Duration,
) -> Result<AuthCleanupReport, RepositoryError> {
    if session_retention <= time::Duration::ZERO {
        return Err(RepositoryError::SessionInvalid);
    }
    let retention_before = now - session_retention;
    let mut transaction = pool.begin().await?;
    let sessions_revoked = sqlx::query(
        "UPDATE access.bff_sessions SET revoked_at=$1, revoke_diagnostic='LW_AUTH_SESSION_REVOKED' \
         WHERE revoked_at IS NULL AND (expires_at <= $1 OR idle_expires_at <= $1)",
    )
    .bind(now)
    .execute(&mut *transaction)
    .await?
    .rows_affected();
    let sessions_deleted = sqlx::query(
        "DELETE FROM access.bff_sessions WHERE revoked_at IS NOT NULL AND revoked_at < $1",
    )
    .bind(retention_before)
    .execute(&mut *transaction)
    .await?
    .rows_affected();
    let transactions_deleted =
        sqlx::query("DELETE FROM access.oidc_transactions WHERE expires_at <= $1")
            .bind(now)
            .execute(&mut *transaction)
            .await?
            .rows_affected();
    let logout_events_deleted =
        sqlx::query("DELETE FROM access.backchannel_logout_events WHERE expires_at <= $1")
            .bind(now)
            .execute(&mut *transaction)
            .await?
            .rows_affected();
    transaction.commit().await?;
    Ok(AuthCleanupReport {
        sessions_revoked,
        sessions_deleted,
        transactions_deleted,
        logout_events_deleted,
    })
}

/// Loads the complete membership truth for an actor without using an
/// authorization-extending cache.
pub async fn load_membership_snapshot(
    pool: &PgPool,
    actor_id: Uuid,
) -> Result<MembershipSnapshot, RepositoryError> {
    let actor_id: ActorId = actor_id
        .to_string()
        .parse()
        .map_err(|_| RepositoryError::MembershipInvalid)?;
    let course_rows = sqlx::query(
        "SELECT course_id, actor_id, role, state, revision, expires_at \
         FROM access.course_memberships WHERE actor_id = $1",
    )
    .bind(actor_id.as_uuid())
    .fetch_all(pool)
    .await?;
    let project_rows = sqlx::query(
        "SELECT course_id, project_id, actor_id, role, state, revision, expires_at \
         FROM access.project_memberships WHERE actor_id = $1",
    )
    .bind(actor_id.as_uuid())
    .fetch_all(pool)
    .await?;
    let course_memberships = course_rows
        .iter()
        .map(course_membership_from_row)
        .collect::<Result<Vec<_>, _>>()?;
    let project_memberships = project_rows
        .iter()
        .map(project_membership_from_row)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(MembershipSnapshot {
        course_memberships,
        project_memberships,
    })
}

/// Inserts the initial active Project membership for a newly-created project.
///
/// Control calls this Access-owned helper while its project row is held in the
/// same Postgres transaction. The database grant for the Control runtime is
/// intentionally limited to this table; the helper therefore does not expose
/// a generic membership mutation surface.
pub async fn insert_project_owner_membership(
    transaction: &mut Transaction<'_, Postgres>,
    project_id: ProjectId,
    actor_id: ActorId,
    course_id: Option<CourseId>,
    role: PlatformRole,
) -> Result<ProjectMembership, RepositoryError> {
    let revision = Revision::new(1).map_err(|_| RepositoryError::MembershipInvalid)?;
    sqlx::query(
        "INSERT INTO access.project_memberships \
         (course_id, project_id, actor_id, role, state, revision, expires_at) \
         VALUES ($1,$2,$3,$4,'active',1,NULL)",
    )
    .bind(course_id.map(CourseId::as_uuid))
    .bind(project_id.as_uuid())
    .bind(actor_id.as_uuid())
    .bind(role_name(role))
    .execute(&mut **transaction)
    .await?;
    Ok(ProjectMembership {
        course_id,
        project_id,
        actor_id,
        role,
        state: MembershipState::Active,
        revision,
        expires_at: None,
    })
}

/// Verifies that an actor has a live course membership with the role used to
/// associate a newly-created project. The check runs on the caller's
/// transaction so project creation cannot commit an unvalidated association.
pub async fn require_course_membership(
    transaction: &mut Transaction<'_, Postgres>,
    course_id: CourseId,
    actor_id: ActorId,
    role: PlatformRole,
    now: OffsetDateTime,
) -> Result<(), RepositoryError> {
    let exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM access.course_memberships \
         WHERE course_id=$1 AND actor_id=$2 AND role=$3 AND state='active' \
           AND (expires_at IS NULL OR expires_at > $4))",
    )
    .bind(course_id.as_uuid())
    .bind(actor_id.as_uuid())
    .bind(role_name(role))
    .bind(now)
    .fetch_one(&mut **transaction)
    .await?;
    if !exists {
        return Err(RepositoryError::MembershipInvalid);
    }
    Ok(())
}

fn course_membership_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<CourseMembership, RepositoryError> {
    Ok(CourseMembership {
        course_id: course_id(row.try_get("course_id")?)?,
        actor_id: actor_id(row.try_get("actor_id")?)?,
        role: parse_role(row.try_get::<String, _>("role")?.as_str())?,
        state: parse_membership_state(row.try_get::<String, _>("state")?.as_str())?,
        revision: revision(row.try_get("revision")?)?,
        expires_at: timestamp(row.try_get("expires_at")?)?,
    })
}

fn project_membership_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<ProjectMembership, RepositoryError> {
    Ok(ProjectMembership {
        course_id: row
            .try_get::<Option<Uuid>, _>("course_id")?
            .map(course_id)
            .transpose()?,
        project_id: project_id(row.try_get("project_id")?)?,
        actor_id: actor_id(row.try_get("actor_id")?)?,
        role: parse_role(row.try_get::<String, _>("role")?.as_str())?,
        state: parse_membership_state(row.try_get::<String, _>("state")?.as_str())?,
        revision: revision(row.try_get("revision")?)?,
        expires_at: timestamp(row.try_get("expires_at")?)?,
    })
}

fn actor_id(value: Uuid) -> Result<ActorId, RepositoryError> {
    value
        .to_string()
        .parse()
        .map_err(|_| RepositoryError::MembershipInvalid)
}

fn course_id(value: Uuid) -> Result<CourseId, RepositoryError> {
    value
        .to_string()
        .parse()
        .map_err(|_| RepositoryError::MembershipInvalid)
}

fn project_id(value: Uuid) -> Result<ProjectId, RepositoryError> {
    value
        .to_string()
        .parse()
        .map_err(|_| RepositoryError::MembershipInvalid)
}

fn revision(value: i64) -> Result<Revision, RepositoryError> {
    Revision::new(u64::try_from(value).map_err(|_| RepositoryError::MembershipInvalid)?)
        .map_err(|_| RepositoryError::MembershipInvalid)
}

fn timestamp(value: Option<OffsetDateTime>) -> Result<Option<UtcTimestamp>, RepositoryError> {
    value
        .map(UtcTimestamp::from_utc)
        .transpose()
        .map_err(|_| RepositoryError::MembershipInvalid)
}

fn parse_membership_state(value: &str) -> Result<MembershipState, RepositoryError> {
    match value {
        "active" => Ok(MembershipState::Active),
        "suspended" => Ok(MembershipState::Suspended),
        "revoked" => Ok(MembershipState::Revoked),
        _ => Err(RepositoryError::MembershipInvalid),
    }
}

/// Creates or finds the durable actor only after a verified OIDC token.
pub async fn upsert_actor(
    pool: &PgPool,
    issuer: &str,
    subject: &str,
) -> Result<LocalActor, RepositoryError> {
    let subject_hash = hex_sha256(subject.as_bytes());
    let actor_id = Uuid::now_v7();
    let row = sqlx::query(
        "INSERT INTO access.actors (actor_id, issuer, subject_sha256) VALUES ($1, $2, $3) \
         ON CONFLICT (issuer, subject_sha256) DO UPDATE SET issuer = EXCLUDED.issuer \
         WHERE access.actors.disabled_at IS NULL RETURNING actor_id",
    )
    .bind(actor_id)
    .bind(issuer)
    .bind(subject_hash)
    .fetch_optional(pool)
    .await?
    .ok_or(RepositoryError::ActorDisabled)?;
    Ok(LocalActor {
        actor_id: row.try_get("actor_id")?,
    })
}

/// Rotates the browser session identifier and stores all secrets AEAD-encrypted.
pub async fn create_bff_session(
    pool: &PgPool,
    key_ring: &KeyRing,
    input: CreateBffSession,
    now: OffsetDateTime,
) -> Result<BffSession, RepositoryError> {
    let idle_expires_at = bounded_idle_expiry(now, input.expires_at, input.idle_ttl)?;
    let session_id = Uuid::now_v7();
    let csrf_token = CsrfToken::generate().map_err(|_| RepositoryError::CsrfGeneration)?;
    let csrf = key_ring.encrypt(csrf_token.expose().as_bytes(), session_id.as_bytes())?;
    let logout_hint = key_ring.encrypt(input.logout_hint.as_bytes(), session_id.as_bytes())?;
    if input.authorization_revision <= 0 || input.roles.is_empty() {
        return Err(RepositoryError::SessionInvalid);
    }
    let oidc_sid_sha256 = input
        .oidc_sid
        .as_deref()
        .map(|sid| hex_sha256(sid.as_bytes()));
    sqlx::query(
        "INSERT INTO access.bff_sessions \
         (session_id, actor_id, platform_roles, oidc_sid_sha256, authorization_revision, issued_at, expires_at, idle_expires_at, \
          encrypted_csrf_token, csrf_encryption_key_id, encrypted_logout_hint, encryption_key_id) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
    )
    .bind(session_id)
    .bind(input.actor_id)
    .bind(input.roles.iter().copied().map(role_name).collect::<Vec<_>>())
    .bind(oidc_sid_sha256)
    .bind(input.authorization_revision)
    .bind(now)
    .bind(input.expires_at)
    .bind(idle_expires_at)
    .bind(csrf.payload)
    .bind(csrf.key_id)
    .bind(logout_hint.payload)
    .bind(logout_hint.key_id)
    .execute(pool)
    .await?;
    Ok(BffSession {
        session_id,
        actor_id: input.actor_id,
        roles: input.roles,
        authorization_revision: input.authorization_revision,
        expires_at: input.expires_at,
        idle_expires_at,
        csrf_token,
    })
}

/// Reads a live session and atomically renews only its idle deadline.
pub async fn load_bff_session(
    pool: &PgPool,
    key_ring: &KeyRing,
    session_id: Uuid,
    idle_ttl: time::Duration,
    now: OffsetDateTime,
) -> Result<BffSession, RepositoryError> {
    let mut transaction = pool.begin().await?;
    let row = sqlx::query(
        "SELECT session_id, actor_id, platform_roles, authorization_revision, expires_at, idle_expires_at, \
         encrypted_csrf_token, csrf_encryption_key_id FROM access.bff_sessions \
         WHERE session_id = $1 AND revoked_at IS NULL AND expires_at > $2 AND idle_expires_at > $2 FOR UPDATE",
    )
    .bind(session_id)
    .bind(now)
    .fetch_optional(&mut *transaction)
    .await?
    .ok_or(RepositoryError::SessionRejected)?;
    let expires_at: OffsetDateTime = row.try_get("expires_at")?;
    let idle_expires_at = bounded_idle_expiry(now, expires_at, idle_ttl)?;
    sqlx::query("UPDATE access.bff_sessions SET idle_expires_at = $2 WHERE session_id = $1")
        .bind(session_id)
        .bind(idle_expires_at)
        .execute(&mut *transaction)
        .await?;
    let encrypted = EncryptedValue {
        key_id: row.try_get("csrf_encryption_key_id")?,
        payload: row.try_get("encrypted_csrf_token")?,
    };
    let csrf = String::from_utf8(key_ring.decrypt(&encrypted, session_id.as_bytes())?)
        .map_err(|_| RepositoryError::SessionRejected)?;
    transaction.commit().await?;
    Ok(BffSession {
        session_id,
        actor_id: row.try_get("actor_id")?,
        roles: row
            .try_get::<Vec<String>, _>("platform_roles")?
            .into_iter()
            .map(|value| parse_role(&value))
            .collect::<Result<_, _>>()?,
        authorization_revision: row.try_get("authorization_revision")?,
        expires_at,
        idle_expires_at,
        csrf_token: CsrfToken::from_secret(csrf),
    })
}

/// Decrypts the verified ID-token logout hint for the authenticated session.
/// The caller must use it only to construct the provider logout redirect and
/// must never return it through an API or log it.
pub async fn load_logout_hint(
    pool: &PgPool,
    key_ring: &KeyRing,
    session_id: Uuid,
) -> Result<String, RepositoryError> {
    let row = sqlx::query(
        "SELECT encrypted_logout_hint, encryption_key_id FROM access.bff_sessions \
         WHERE session_id = $1 AND revoked_at IS NULL",
    )
    .bind(session_id)
    .fetch_optional(pool)
    .await?
    .ok_or(RepositoryError::SessionRejected)?;
    let encrypted = EncryptedValue {
        key_id: row.try_get("encryption_key_id")?,
        payload: row.try_get("encrypted_logout_hint")?,
    };
    String::from_utf8(key_ring.decrypt(&encrypted, session_id.as_bytes())?)
        .map_err(|_| RepositoryError::SessionRejected)
}

fn role_name(role: PlatformRole) -> &'static str {
    match role {
        PlatformRole::Teacher => "teacher",
        PlatformRole::Student => "student",
        PlatformRole::PlatformAdmin => "platform_admin",
    }
}

fn parse_role(value: &str) -> Result<PlatformRole, RepositoryError> {
    match value {
        "teacher" => Ok(PlatformRole::Teacher),
        "student" => Ok(PlatformRole::Student),
        "platform_admin" => Ok(PlatformRole::PlatformAdmin),
        _ => Err(RepositoryError::SessionRejected),
    }
}

/// Revokes a single browser session. Revocation is idempotent and never deletes audit state.
pub async fn revoke_bff_session(
    pool: &PgPool,
    session_id: Uuid,
    diagnostic: &str,
    now: OffsetDateTime,
) -> Result<(), RepositoryError> {
    sqlx::query(
        "UPDATE access.bff_sessions SET revoked_at = COALESCE(revoked_at, $2), \
         revoke_diagnostic = COALESCE(revoke_diagnostic, $3) WHERE session_id = $1",
    )
    .bind(session_id)
    .bind(now)
    .bind(diagnostic)
    .execute(pool)
    .await?;
    Ok(())
}

/// Revokes all sessions associated with a verified provider back-channel logout SID.
pub async fn revoke_bff_sessions_by_sid(
    pool: &PgPool,
    sid: &str,
    now: OffsetDateTime,
) -> Result<u64, RepositoryError> {
    let sid = hex_sha256(sid.as_bytes());
    let result = sqlx::query(
        "UPDATE access.bff_sessions SET revoked_at = $2, revoke_diagnostic = 'LW_AUTH_SESSION_REVOKED' \
         WHERE oidc_sid_sha256 = $1 AND revoked_at IS NULL",
    )
    .bind(sid)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Atomically reserves a back-channel logout token identity and revokes every
/// matching provider session. A replay never repeats side effects.
pub async fn consume_backchannel_logout(
    pool: &PgPool,
    issuer: &str,
    jti: &str,
    sid: &str,
    expires_at: OffsetDateTime,
    now: OffsetDateTime,
) -> Result<u64, RepositoryError> {
    if issuer.is_empty() || jti.is_empty() || sid.is_empty() || expires_at <= now {
        return Err(RepositoryError::LogoutReplay);
    }
    let jti_hash = hex_sha256(jti.as_bytes());
    let sid_hash = hex_sha256(sid.as_bytes());
    let mut transaction = pool.begin().await?;
    let reservation = sqlx::query(
        "INSERT INTO access.backchannel_logout_events \
         (issuer, jti_sha256, received_at, expires_at) VALUES ($1,$2,$3,$4) \
         ON CONFLICT (issuer, jti_sha256) DO NOTHING",
    )
    .bind(issuer)
    .bind(jti_hash)
    .bind(now)
    .bind(expires_at)
    .execute(&mut *transaction)
    .await?;
    if reservation.rows_affected() != 1 {
        return Err(RepositoryError::LogoutReplay);
    }
    let revoked = sqlx::query(
        "UPDATE access.bff_sessions SET revoked_at=$2, revoke_diagnostic='LW_AUTH_SESSION_REVOKED' \
         WHERE oidc_sid_sha256=$1 AND revoked_at IS NULL",
    )
    .bind(sid_hash)
    .bind(now)
    .execute(&mut *transaction)
    .await?
    .rows_affected();
    transaction.commit().await?;
    Ok(revoked)
}

/// Loads and atomically consumes an unexpired OIDC transaction.
pub async fn consume_oidc_transaction(
    pool: &PgPool,
    key_ring: &KeyRing,
    state: &str,
    now: OffsetDateTime,
) -> Result<OidcTransaction, RepositoryError> {
    let hash = hex_sha256(state.as_bytes());
    let mut tx = pool.begin().await?;
    let row = sqlx::query(
        "SELECT transaction_id, encrypted_payload, encryption_key_id FROM access.oidc_transactions \
         WHERE state_sha256 = $1 AND consumed_at IS NULL AND expires_at > $2 FOR UPDATE",
    )
    .bind(hash)
    .bind(now)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(RepositoryError::StateRejected)?;
    let id: Uuid = row.try_get("transaction_id")?;
    let value = EncryptedValue {
        key_id: row.try_get("encryption_key_id")?,
        payload: row.try_get("encrypted_payload")?,
    };
    let plaintext = key_ring.decrypt(&value, id.as_bytes())?;
    let transaction: OidcTransaction = serde_json::from_slice(&plaintext)?;
    transaction.verify_state(Some(state))?;
    sqlx::query("UPDATE access.oidc_transactions SET consumed_at = $2 WHERE transaction_id = $1")
        .bind(id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(transaction)
}

/// Persistence failures never permit a callback to proceed.
#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    /// State did not identify a live unconsumed transaction.
    #[error("LW_AUTH_OIDC_STATE_REJECTED")]
    StateRejected,
    /// `PostgreSQL` query or transaction failure.
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    /// Stored ciphertext could not be authenticated.
    #[error(transparent)]
    Crypto(#[from] crate::CryptoError),
    /// Stored transaction did not match returned callback state.
    #[error(transparent)]
    Transaction(#[from] OidcTransactionError),
    /// Encrypted state did not contain a valid transaction DTO.
    #[error("LW_AUTH_OIDC_STATE_REJECTED")]
    Serialization(#[from] serde_json::Error),
    /// The verified actor has been administratively disabled.
    #[error("LW_AUTH_IDENTITY_REJECTED")]
    ActorDisabled,
    /// Secure CSRF token generation failed.
    #[error("LW_AUTH_CSRF_RANDOMNESS_UNAVAILABLE")]
    CsrfGeneration,
    /// A back-channel logout token identity was already consumed or invalid.
    #[error("LW_AUTH_LOGOUT_TOKEN_REPLAYED")]
    LogoutReplay,
    /// A session would violate an absolute or idle lifetime invariant.
    #[error("LW_AUTH_SESSION_REJECTED")]
    SessionInvalid,
    /// A session does not exist, is revoked, or has expired.
    #[error("LW_AUTH_SESSION_REJECTED")]
    SessionRejected,
    /// The peer certificate SAN is not an active registered service identity.
    #[error("LW_AUTH_SERVICE_IDENTITY_DENIED")]
    ServiceIdentityDenied,
    /// Authoritative membership data contained an invalid value.
    #[error("LW_AUTH_MEMBERSHIP_UNAVAILABLE")]
    MembershipInvalid,
}

#[cfg(test)]
mod tests {
    use time::{Duration, OffsetDateTime};

    use super::{RepositoryError, bounded_idle_expiry, configured_session_expiry};

    #[test]
    fn configured_session_expiry_is_independent_of_id_token_expiry() {
        let issued_at = OffsetDateTime::UNIX_EPOCH + Duration::seconds(1_000);
        let id_token_expiry = issued_at + Duration::seconds(5);
        assert!(matches!(
            configured_session_expiry(issued_at, Duration::seconds(900)),
            Ok(session_expiry)
                if session_expiry == issued_at + Duration::seconds(900)
                    && session_expiry > id_token_expiry
        ));
    }

    #[test]
    fn idle_renewal_never_extends_absolute_expiry() {
        let now = OffsetDateTime::UNIX_EPOCH + Duration::seconds(1_050);
        let absolute_expiry = OffsetDateTime::UNIX_EPOCH + Duration::seconds(1_060);

        assert!(matches!(
            bounded_idle_expiry(now, absolute_expiry, Duration::seconds(300)),
            Ok(idle_expiry) if idle_expiry == absolute_expiry
        ));
    }

    #[test]
    fn invalid_session_lifetime_is_rejected() {
        let now = OffsetDateTime::UNIX_EPOCH + Duration::seconds(1_000);
        assert!(matches!(
            configured_session_expiry(now, Duration::ZERO),
            Err(RepositoryError::SessionInvalid)
        ));
        assert!(matches!(
            bounded_idle_expiry(now, now, Duration::seconds(1)),
            Err(RepositoryError::SessionInvalid)
        ));
    }
}
