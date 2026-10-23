//! Bounded checks of current Control membership facts for established connections.

use contracts::{AuthorizationDecision, AuthorizationScope};
use sha2::{Digest, Sha256};
use sqlx::PgPool;

/// Identity and object binding verified at the connection's HTTP boundary.
#[derive(Clone)]
pub struct LiveAuthorization {
    pool: PgPool,
    decision: AuthorizationDecision,
    operation: &'static str,
    session_id: contracts::BffSessionId,
}

impl LiveAuthorization {
    /// Freezes the verified identity and scope. It cannot turn a denial into a permission.
    #[must_use]
    pub fn new(
        pool: PgPool,
        decision: AuthorizationDecision,
        operation: &'static str,
        session_id: contracts::BffSessionId,
    ) -> Self {
        Self {
            pool,
            decision,
            operation,
            session_id,
        }
    }

    /// Checks current membership with a hard timeout. Missing data, revision drift and failures deny.
    pub async fn active(&self) -> bool {
        tokio::time::timeout(
            std::time::Duration::from_secs(
                contracts::object_authorization::AUTHORIZATION_CHECK_TIMEOUT_SECONDS,
            ),
            self.check(),
        )
        .await
        .unwrap_or(false)
    }

    async fn check(&self) -> bool {
        let now = time::OffsetDateTime::now_utc();
        if self.decision.valid_until.get() <= now {
            return false;
        }
        let actor_active = sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM access.current_authorization_sessions WHERE session_sha256=$1 AND actor_id=$2 AND expires_at>clock_timestamp() AND idle_expires_at>clock_timestamp())")
            .bind(format!("{:x}",Sha256::digest(self.session_id.to_string().as_bytes()))).bind(self.decision.actor.actor_id.as_uuid()).fetch_one(&self.pool).await.unwrap_or(false);
        if !actor_active {
            return false;
        }
        let Ok(memberships) =
            crate::load_membership_snapshot(&self.pool, self.decision.actor.actor_id.as_uuid())
                .await
        else {
            return false;
        };
        let Some(operation) = contracts::operation_contract(self.operation) else {
            return false;
        };
        let Ok(current) = crate::object_authorization::authorize_operation(
            &crate::AuthorizationContext {
                actor: self.decision.actor.clone(),
                course_memberships: memberships.course_memberships,
                project_memberships: memberships.project_memberships,
                now,
            },
            self.decision.scope.clone(),
            operation,
        ) else {
            return false;
        };
        current.authorization_revision == self.decision.authorization_revision
            && current.scope == self.decision.scope
            && matches!(
                current.scope,
                AuthorizationScope::Project { .. } | AuthorizationScope::Course { .. }
            )
    }
}
