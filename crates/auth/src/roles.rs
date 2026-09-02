//! Configured Keycloak realm-role extraction with a fail-closed allowlist.

use std::collections::{BTreeMap, BTreeSet};

use contracts::PlatformRole;
/// Deployment-configured Keycloak realm-role mappings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoleMappings {
    roles: BTreeMap<String, PlatformRole>,
}

impl RoleMappings {
    /// Parses a non-empty mapping from Keycloak roles to platform roles.
    pub fn parse(values: BTreeMap<String, String>) -> Result<Self, RoleClaimError> {
        let mut roles = BTreeMap::new();
        for (keycloak_role, platform_role) in values {
            let mapped = match platform_role.as_str() {
                "teacher" => PlatformRole::Teacher,
                "student" => PlatformRole::Student,
                "platform_admin" => PlatformRole::PlatformAdmin,
                _ => return Err(RoleClaimError::MappingInvalid),
            };
            if keycloak_role.trim().is_empty() || roles.insert(keycloak_role, mapped).is_some() {
                return Err(RoleClaimError::MappingInvalid);
            }
        }
        if roles.is_empty() {
            return Err(RoleClaimError::MappingInvalid);
        }
        Ok(Self { roles })
    }
}

/// Extracts approved platform roles from the configured signed claim mapping.
pub fn extract_platform_roles(
    claims: &serde_json::Value,
    claim_path: &[String],
    mappings: &RoleMappings,
) -> Result<BTreeSet<PlatformRole>, RoleClaimError> {
    if claim_path.is_empty() || claim_path.iter().any(String::is_empty) {
        return Err(RoleClaimError::Malformed);
    }
    let value = claim_path.iter().try_fold(claims, |current, segment| {
        current.get(segment).ok_or(RoleClaimError::Missing)
    })?;
    let roles: Vec<String> =
        serde_json::from_value(value.clone()).map_err(|_| RoleClaimError::Malformed)?;
    let mapped = roles
        .into_iter()
        .filter_map(|role| mappings.roles.get(&role).copied())
        .collect::<BTreeSet<_>>();
    if mapped.is_empty() {
        return Err(RoleClaimError::Denied);
    }
    Ok(mapped)
}

/// Role-claim failures deny authentication.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RoleClaimError {
    /// Claim encoding was incompatible with Keycloak.
    #[error("LW_AUTH_TOKEN_INVALID")]
    Malformed,
    /// The signed token lacks the required realm-role claim.
    #[error("LW_AUTH_ROLE_DENIED")]
    Missing,
    /// The token has no role allowed by deployment configuration.
    #[error("LW_AUTH_ROLE_DENIED")]
    Denied,
    /// Deployment role mapping is empty, ambiguous, or invalid.
    #[error("LW_AUTH_CONFIG_BINDING_MISSING")]
    MappingInvalid,
}

#[cfg(test)]
mod tests {
    use super::{RoleMappings, extract_platform_roles};
    use contracts::PlatformRole;
    use serde_json::json;
    use std::collections::BTreeMap;

    #[test]
    fn deployment_mapping_controls_approved_realm_roles() -> Result<(), Box<dyn std::error::Error>>
    {
        let mappings = RoleMappings::parse(BTreeMap::from([(
            "course-teacher".into(),
            "teacher".into(),
        )]))?;
        let roles = extract_platform_roles(
            &json!({"realm_access":{"roles":["course-teacher","unknown"]}}),
            &["realm_access".into(), "roles".into()],
            &mappings,
        )?;
        assert!(roles.contains(&PlatformRole::Teacher));
        assert!(
            extract_platform_roles(
                &json!({"realm_access":{"roles":["teacher"]}}),
                &["realm_access".into(), "roles".into()],
                &mappings,
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn accepts_explicit_keycloak_platform_role_aliases() -> Result<(), Box<dyn std::error::Error>> {
        let mappings = RoleMappings::parse(BTreeMap::from([
            ("platform_admin".into(), "platform_admin".into()),
            ("platform-admin".into(), "platform_admin".into()),
        ]))?;
        for keycloak_role in ["platform_admin", "platform-admin"] {
            let roles = extract_platform_roles(
                &json!({"realm_access":{"roles":[keycloak_role]}}),
                &["realm_access".into(), "roles".into()],
                &mappings,
            )?;
            assert!(roles.contains(&PlatformRole::PlatformAdmin));
        }
        Ok(())
    }
}
