//! `PostgreSQL` compare-and-consume operations for one-time OIDC state.

use sqlx::{PgPool, Row};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{CsrfToken, EncryptedValue, KeyRing, OidcTransaction, OidcTransactionError};
use contracts::{
    ActorId, CourseId, CourseMembership, MembershipState, PlatformRole, ProjectId,
    ProjectMembership, Revision, UtcTimestamp,
};

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
    /// Absolute expiration bound to the ID-token expiry.
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
    /// Absolute session expiry.
    pub expires_at: OffsetDateTime,
    /// Idle timeout configured by deployment.
    pub idle_ttl: time::Duration,
    /// Optional OIDC provider session identifier.
    pub oidc_sid: Option<String>,
    /// Signed ID-token logout hint to encrypt server-side.
    pub logout_hint: String,
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

/// Ensures the mTLS principal is a currently active, non-expired registered service.
pub async fn require_service_identity(
    pool: &PgPool,
    san_uri: &str,
    now: OffsetDateTime,
) -> Result<(), RepositoryError> {
    let present: Option<bool> = sqlx::query_scalar(
        "SELECT true FROM access.service_identities \
         WHERE san_uri = $1 AND state = 'active' AND (expires_at IS NULL OR expires_at > $2)",
    )
    .bind(san_uri)
    .bind(now)
    .fetch_optional(pool)
    .await?;
    present.ok_or(RepositoryError::ServiceIdentityDenied)?;
    Ok(())
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
        course_id: course_id(row.try_get("course_id")?)?,
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
    let subject_hash = contracts::Sha256Digest::of_bytes(subject.as_bytes()).to_string();
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
    let session_id = Uuid::now_v7();
    let csrf_token = CsrfToken::generate().map_err(|_| RepositoryError::CsrfGeneration)?;
    let csrf = key_ring.encrypt(csrf_token.expose().as_bytes(), session_id.as_bytes())?;
    let logout_hint = key_ring.encrypt(input.logout_hint.as_bytes(), session_id.as_bytes())?;
    let idle_expires_at = std::cmp::min(now + input.idle_ttl, input.expires_at);
    if input.expires_at <= now
        || idle_expires_at <= now
        || input.authorization_revision <= 0
        || input.roles.is_empty()
    {
        return Err(RepositoryError::SessionInvalid);
    }
    let oidc_sid_sha256 = input
        .oidc_sid
        .as_deref()
        .map(|sid| contracts::Sha256Digest::of_bytes(sid.as_bytes()).to_string());
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
    let idle_expires_at = std::cmp::min(now + idle_ttl, expires_at);
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
    let sid = contracts::Sha256Digest::of_bytes(sid.as_bytes()).to_string();
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
    let jti_hash = contracts::Sha256Digest::of_bytes(jti.as_bytes()).to_string();
    let sid_hash = contracts::Sha256Digest::of_bytes(sid.as_bytes()).to_string();
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
    let hash = contracts::Sha256Digest::of_bytes(state.as_bytes()).to_string();
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
