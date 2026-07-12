//! Stable domain primitives shared across `LabWeaver` service boundaries.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Version of the initial public HTTP contract.
pub const API_VERSION: &str = "v1";

/// Opaque identifier used at public service boundaries.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct EntityId(Uuid);

impl EntityId {
    /// Creates a globally unique identifier.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for EntityId {
    fn default() -> Self {
        Self::new()
    }
}

/// Errors produced while validating shared domain values.
#[derive(Debug, Error)]
pub enum DomainError {
    /// A required public value was empty.
    #[error("required value is empty")]
    EmptyRequiredValue,
}
