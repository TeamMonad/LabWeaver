//! Durable coordination for connected package/deploy/replay operations.
//!
//! The ledger is deliberately local to the approved controller and contains only hashes,
//! run identities and stable diagnostics. A create-new lock is used instead of a best-effort
//! PID file: if the controller dies while changing state, the next invocation must stop and
//! require an explicit operator inspection rather than start a second operation against an
//! unknown cluster state.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionIdentity {
    pub operation: String,
    pub environment: String,
    pub source_commit: String,
    pub configuration_sha256: Option<String>,
    pub package_sha256: Option<String>,
    pub deployment_sha256: Option<String>,
    pub run_id: String,
    pub testflight_run_id: Option<String>,
}

#[derive(Debug)]
pub enum LedgerError {
    Io,
    Corrupt,
    InProgress,
    DuplicateAttempt,
    AttemptBudgetExhausted,
    OperationBudgetExhausted,
}

impl LedgerError {
    #[must_use]
    #[allow(dead_code)]
    pub const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::Io => "LW_EXECUTION_LEDGER_IO_FAILED",
            Self::Corrupt => "LW_EXECUTION_LEDGER_CORRUPT",
            Self::InProgress => "LW_EXECUTION_IN_PROGRESS",
            Self::DuplicateAttempt => "LW_EXECUTION_ATTEMPT_DUPLICATE",
            Self::AttemptBudgetExhausted => "LW_EXECUTION_ATTEMPT_BUDGET_EXHAUSTED",
            Self::OperationBudgetExhausted => "LW_EXECUTION_OPERATION_BUDGET_EXHAUSTED",
        }
    }
}

#[derive(Debug)]
pub struct ExecutionLease {
    ledger_path: PathBuf,
    lock_path: PathBuf,
    entries: Vec<Entry>,
    entry: Entry,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    candidate_key: String,
    attempt_key: String,
    operation: String,
    environment: String,
    source_commit: String,
    configuration_sha256: Option<String>,
    package_sha256: Option<String>,
    deployment_sha256: Option<String>,
    run_id: String,
    testflight_run_id: Option<String>,
    state: EntryState,
    started_at_unix: u64,
    finished_at_unix: Option<u64>,
    diagnostic: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EntryState {
    Running,
    Succeeded,
    Failed,
}

/// Acquire the single controller operation lock and reserve one bounded attempt.
///
/// `max_attempts` limits retries for one exact candidate. `max_operation_attempts`
/// limits all candidates in this operation ledger, so changing a commit, release
/// label, or locator cannot turn a bounded validation into an unbounded loop.
pub fn acquire(
    root: &Path,
    identity: ExecutionIdentity,
    max_attempts: u32,
    max_operation_attempts: u32,
) -> Result<ExecutionLease, LedgerError> {
    if max_attempts == 0
        || max_operation_attempts == 0
        || identity.operation.trim().is_empty()
        || identity.environment.trim().is_empty()
    {
        return Err(LedgerError::Corrupt);
    }
    fs::create_dir_all(root).map_err(|_| LedgerError::Io)?;
    #[cfg(unix)]
    {
        fs::set_permissions(root, fs::Permissions::from_mode(0o700))
            .map_err(|_| LedgerError::Io)?;
    }
    let stem = slug(&format!("{}-{}", identity.operation, identity.environment));
    let ledger_path = root.join(format!("{stem}.json"));
    let lock_path = root.join(format!("{stem}.lock"));
    let mut lock = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(LedgerError::InProgress);
        }
        Err(_) => return Err(LedgerError::Io),
    };

    let candidate_key = candidate_key(&identity);
    let attempt_key = attempt_key(&identity);
    let entries = match fs::read(&ledger_path) {
        Ok(bytes) => serde_json::from_slice::<Vec<Entry>>(&bytes).map_err(|_| LedgerError::Corrupt),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(_) => Err(LedgerError::Io),
    }?;
    if entries.iter().any(|entry| entry.attempt_key == attempt_key) {
        let _ = fs::remove_file(&lock_path);
        return Err(LedgerError::DuplicateAttempt);
    }
    if entries.len() >= usize::try_from(max_operation_attempts).map_err(|_| LedgerError::Corrupt)? {
        let _ = fs::remove_file(&lock_path);
        return Err(LedgerError::OperationBudgetExhausted);
    }
    let candidate_attempts = entries
        .iter()
        .filter(|entry| entry.candidate_key == candidate_key)
        .count();
    if candidate_attempts >= usize::try_from(max_attempts).map_err(|_| LedgerError::Corrupt)? {
        let _ = fs::remove_file(&lock_path);
        return Err(LedgerError::AttemptBudgetExhausted);
    }
    let started_at_unix = now_unix();
    let entry = Entry {
        candidate_key,
        attempt_key,
        operation: identity.operation,
        environment: identity.environment,
        source_commit: identity.source_commit,
        configuration_sha256: identity.configuration_sha256,
        package_sha256: identity.package_sha256,
        deployment_sha256: identity.deployment_sha256,
        run_id: identity.run_id,
        testflight_run_id: identity.testflight_run_id,
        state: EntryState::Running,
        started_at_unix,
        finished_at_unix: None,
        diagnostic: None,
    };
    lock.write_all(b"running\n").map_err(|_| LedgerError::Io)?;
    lock.flush().map_err(|_| LedgerError::Io)?;
    drop(lock);
    let mut updated = entries.clone();
    updated.push(entry.clone());
    write_entries(&ledger_path, &updated)?;
    Ok(ExecutionLease {
        ledger_path,
        lock_path,
        entries: updated,
        entry,
    })
}

impl ExecutionLease {
    /// Finish the reserved operation and release the controller lock.
    pub fn finish(mut self, succeeded: bool, diagnostic: Option<&str>) -> Result<(), LedgerError> {
        self.entry.state = if succeeded {
            EntryState::Succeeded
        } else {
            EntryState::Failed
        };
        self.entry.finished_at_unix = Some(now_unix());
        self.entry.diagnostic = diagnostic.map(ToOwned::to_owned);
        let mut updated = self.entries;
        let Some(existing) = updated
            .iter_mut()
            .find(|entry| entry.attempt_key == self.entry.attempt_key)
        else {
            return Err(LedgerError::Corrupt);
        };
        *existing = self.entry;
        write_entries(&self.ledger_path, &updated)?;
        fs::remove_file(&self.lock_path).map_err(|_| LedgerError::Io)
    }
}

fn write_entries(path: &Path, entries: &[Entry]) -> Result<(), LedgerError> {
    let temporary = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(entries).map_err(|_| LedgerError::Corrupt)?;
    fs::write(&temporary, bytes).map_err(|_| LedgerError::Io)?;
    fs::rename(temporary, path).map_err(|_| LedgerError::Io)
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn slug(value: &str) -> String {
    let mut result = String::with_capacity(value.len().min(96));
    for byte in value.bytes() {
        if result.len() >= 96 {
            break;
        }
        if byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' {
            result.push(char::from(byte.to_ascii_lowercase()));
        } else {
            result.push('-');
        }
    }
    if result.is_empty() {
        "execution".to_owned()
    } else {
        result
    }
}

fn digest(fields: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for field in fields {
        hasher.update(field.as_bytes());
        hasher.update([0]);
    }
    format!("sha256:{:x}", hasher.finalize())
}

fn candidate_key(identity: &ExecutionIdentity) -> String {
    digest(&[
        &identity.operation,
        &identity.environment,
        &identity.source_commit,
        identity.configuration_sha256.as_deref().unwrap_or(""),
        identity.package_sha256.as_deref().unwrap_or(""),
        identity.deployment_sha256.as_deref().unwrap_or(""),
    ])
}

fn attempt_key(identity: &ExecutionIdentity) -> String {
    digest(&[
        &candidate_key(identity),
        &identity.run_id,
        identity.testflight_run_id.as_deref().unwrap_or(""),
    ])
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::{ExecutionIdentity, LedgerError, acquire};
    use tempfile::tempdir;

    fn identity(run_id: &str) -> ExecutionIdentity {
        identity_with_source(run_id, "a")
    }

    fn identity_with_source(run_id: &str, source: &str) -> ExecutionIdentity {
        ExecutionIdentity {
            operation: "resource-replay".to_owned(),
            environment: "demo".to_owned(),
            source_commit: source.repeat(40),
            configuration_sha256: Some("sha256:configuration".to_owned()),
            package_sha256: Some("sha256:package".to_owned()),
            deployment_sha256: Some("sha256:deployment".to_owned()),
            run_id: run_id.to_owned(),
            testflight_run_id: None,
        }
    }

    #[test]
    fn concurrent_execution_is_rejected_without_second_process() {
        let root = tempdir().expect("tempdir");
        let first = acquire(root.path(), identity("run-one"), 1, 3).expect("first lease");
        assert!(matches!(
            acquire(root.path(), identity("run-two"), 1, 3),
            Err(LedgerError::InProgress)
        ));
        first
            .finish(false, Some("LW_TEST_BLOCKED"))
            .expect("finish");
    }

    #[test]
    fn failed_attempt_can_be_repaired_once_but_not_repeated_forever() {
        let root = tempdir().expect("tempdir");
        let first = acquire(root.path(), identity("run-one"), 2, 3).expect("first lease");
        first
            .finish(false, Some("LW_TEST_BLOCKED"))
            .expect("finish");
        let second = acquire(root.path(), identity("run-two"), 2, 3).expect("repair lease");
        second.finish(true, None).expect("finish");
        assert!(matches!(
            acquire(root.path(), identity("run-three"), 2, 3),
            Err(LedgerError::AttemptBudgetExhausted)
        ));
    }

    #[test]
    fn operation_budget_stops_new_candidates_after_three_cycles() {
        let root = tempdir().expect("tempdir");
        for (run_id, source) in [("run-one", "a"), ("run-two", "b"), ("run-three", "c")] {
            let lease = acquire(root.path(), identity_with_source(run_id, source), 1, 3)
                .expect("cycle lease");
            lease
                .finish(false, Some("LW_TEST_BLOCKED"))
                .expect("finish");
        }
        assert!(matches!(
            acquire(root.path(), identity_with_source("run-four", "d"), 1, 3),
            Err(LedgerError::OperationBudgetExhausted)
        ));
    }

    #[test]
    fn exact_attempt_identity_cannot_be_reused() {
        let root = tempdir().expect("tempdir");
        let first = acquire(root.path(), identity("run-one"), 2, 3).expect("first lease");
        first
            .finish(false, Some("LW_TEST_BLOCKED"))
            .expect("finish");
        assert!(matches!(
            acquire(root.path(), identity("run-one"), 2, 3),
            Err(LedgerError::DuplicateAttempt)
        ));
    }
}
