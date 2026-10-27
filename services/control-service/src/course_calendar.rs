#![allow(
    dead_code,
    reason = "The Control boundary is staged before the shared public contract is generated."
)]
//! Course calendar facts and roster import invariants.
//!
//! This module intentionally has no HTTP-contract dependency. It is the Control
//! domain boundary for values that are later exposed through the public SDK.

use std::collections::BTreeMap;

use time::OffsetDateTime;
use uuid::Uuid;

/// The policy applied after the due instant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LatePolicy {
    HardClose,
    AllowWithMark,
}

impl LatePolicy {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::HardClose => "hard_close",
            Self::AllowWithMark => "allow_with_mark",
        }
    }
}

/// Persisted course scheduling facts. Instants are always UTC; `time_zone` is
/// retained as the IANA display/input zone and never used as a server-local TZ.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CourseCalendarFacts {
    pub course_id: Uuid,
    pub revision: u64,
    pub open_at: Option<OffsetDateTime>,
    pub due_at: OffsetDateTime,
    pub time_zone: String,
    pub late_policy: LatePolicy,
    pub late_until: Option<OffsetDateTime>,
}

/// The effective state at an instant. The open boundary is inclusive and the
/// due boundary is exclusive, so a request exactly at due is late or closed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CalendarState {
    NotOpen,
    Open,
    Late,
    Closed,
}

impl CourseCalendarFacts {
    pub(crate) fn validate(&self) -> Result<(), CalendarError> {
        if self.revision == 0 {
            return Err(CalendarError::InvalidRevision);
        }
        if self.time_zone.trim().is_empty() || self.time_zone.chars().any(char::is_control) {
            return Err(CalendarError::InvalidTimeZone);
        }
        if self.open_at.is_some_and(|open| open >= self.due_at) {
            return Err(CalendarError::InvalidRange);
        }
        if self.late_policy == LatePolicy::HardClose && self.late_until.is_some() {
            return Err(CalendarError::LateUntilNotAllowed);
        }
        if self
            .late_until
            .is_some_and(|late_until| late_until <= self.due_at)
        {
            return Err(CalendarError::InvalidLateRange);
        }
        Ok(())
    }

    pub(crate) fn state_at(&self, now: OffsetDateTime) -> Result<CalendarState, CalendarError> {
        self.validate()?;
        if self.open_at.is_some_and(|open| now < open) {
            return Ok(CalendarState::NotOpen);
        }
        if now < self.due_at {
            return Ok(CalendarState::Open);
        }
        match self.late_policy {
            LatePolicy::HardClose => Ok(CalendarState::Closed),
            LatePolicy::AllowWithMark => {
                if self.late_until.is_none_or(|until| now < until) {
                    Ok(CalendarState::Late)
                } else {
                    Ok(CalendarState::Closed)
                }
            }
        }
    }
}

/// A normalized roster row before directory resolution. The username is kept
/// case-preserving; directory resolution owns campus case-folding semantics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RosterRow {
    pub row_number: usize,
    pub username: String,
    pub role: Option<String>,
}

/// Per-row deterministic result used by the eventual import response.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RosterRowState {
    Valid,
    DuplicateInFile,
    Invalid,
}

/// Validates rows and marks duplicate usernames without silently dropping any
/// row. Duplicate detection is case-sensitive until the directory's canonical
/// username resolution has run.
pub(crate) fn classify_roster_rows(rows: &[RosterRow]) -> BTreeMap<usize, RosterRowState> {
    let mut seen = BTreeMap::<String, usize>::new();
    let mut result = BTreeMap::new();
    for row in rows {
        let username = row.username.trim();
        if username.is_empty() || username.chars().any(char::is_control) {
            result.insert(row.row_number, RosterRowState::Invalid);
            continue;
        }
        if seen.insert(username.to_owned(), row.row_number).is_some() {
            result.insert(row.row_number, RosterRowState::DuplicateInFile);
        } else {
            result.insert(row.row_number, RosterRowState::Valid);
        }
    }
    result
}

/// Validates a configured IANA-like zone name without pretending a fixed
/// offset is an IANA database. Offset parsing is useful for boundary tests and
/// input forms; named-zone validation is deliberately non-empty/control-safe.
pub(crate) fn validate_time_zone(value: &str) -> Result<(), CalendarError> {
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        return Err(CalendarError::InvalidTimeZone);
    }
    if value.contains('/') || value == "UTC" {
        Ok(())
    } else {
        Err(CalendarError::InvalidTimeZone)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CalendarError {
    InvalidRevision,
    InvalidRange,
    InvalidLateRange,
    LateUntilNotAllowed,
    InvalidTimeZone,
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn calendar(policy: LatePolicy, late_until: Option<OffsetDateTime>) -> CourseCalendarFacts {
        CourseCalendarFacts {
            course_id: Uuid::nil(),
            revision: 1,
            open_at: Some(datetime!(2026-10-01 09:00 UTC)),
            due_at: datetime!(2026-10-01 10:00 UTC),
            time_zone: "Asia/Shanghai".to_owned(),
            late_policy: policy,
            late_until,
        }
    }

    #[test]
    fn due_boundary_is_late_only_when_policy_allows_it() {
        let now = datetime!(2026-10-01 10:00 UTC);
        assert_eq!(
            calendar(LatePolicy::HardClose, None).state_at(now),
            Ok(CalendarState::Closed)
        );
        assert_eq!(
            calendar(
                LatePolicy::AllowWithMark,
                Some(datetime!(2026-10-01 11:00 UTC))
            )
            .state_at(now),
            Ok(CalendarState::Late)
        );
    }

    #[test]
    fn late_until_is_a_hard_boundary() {
        let facts = calendar(
            LatePolicy::AllowWithMark,
            Some(datetime!(2026-10-01 11:00 UTC)),
        );
        assert_eq!(
            facts.state_at(datetime!(2026-10-01 10:59:59 UTC)),
            Ok(CalendarState::Late)
        );
        assert_eq!(
            facts.state_at(datetime!(2026-10-01 11:00 UTC)),
            Ok(CalendarState::Closed)
        );
    }

    #[test]
    fn roster_classification_preserves_duplicate_row_results() {
        let states = classify_roster_rows(&[
            RosterRow {
                row_number: 2,
                username: "alice".to_owned(),
                role: None,
            },
            RosterRow {
                row_number: 3,
                username: "alice".to_owned(),
                role: None,
            },
            RosterRow {
                row_number: 4,
                username: String::new(),
                role: None,
            },
        ]);
        assert_eq!(states.get(&2), Some(&RosterRowState::Valid));
        assert_eq!(states.get(&3), Some(&RosterRowState::DuplicateInFile));
        assert_eq!(states.get(&4), Some(&RosterRowState::Invalid));
    }

    #[test]
    fn calendar_rejects_late_until_for_hard_close() {
        assert_eq!(
            calendar(LatePolicy::HardClose, Some(datetime!(2026-10-01 11:00 UTC))).validate(),
            Err(CalendarError::LateUntilNotAllowed)
        );
    }
}
