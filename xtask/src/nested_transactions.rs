//! Static guard for SQLx nested transactions.
//!
//! SQLx 0.9's PostgreSQL driver fix (#4394) still has the nested transaction
//! regression tracked as upstream #4445. Production code therefore must not
//! call `begin()` on an existing `Transaction`. This check intentionally scans
//! production `src/` trees only; integration tests may exercise the boundary.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Rejects the syntactic forms used to start a nested transaction in production.
pub fn check(repository_root: &Path) -> Result<(), String> {
    let mut files = Vec::new();
    for directory in ["crates", "services", "access-gateway"] {
        collect_source_files(&repository_root.join(directory), &mut files)?;
    }

    for path in files {
        check_file(&path)?;
    }
    Ok(())
}

fn collect_source_files(directory: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    if !directory.exists() {
        return Ok(());
    }
    let entries = fs::read_dir(directory)
        .map_err(|error| format!("cannot read {}: {error}", directory.display()))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("cannot inspect {}: {error}", directory.display()))?;
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "tests") {
                continue;
            }
            collect_source_files(&path, files)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    Ok(())
}

fn check_file(path: &Path) -> Result<(), String> {
    let source = fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let mut transaction_variables = BTreeSet::new();
    let mut pending_variable = None;

    for (line_number, line) in source.lines().enumerate() {
        if let Some(variable) = pending_variable.take() {
            if line.contains(".begin(") || line.contains(".begin_with(") {
                transaction_variables.insert(variable);
            } else if line.contains(';') {
                pending_variable = None;
            } else {
                pending_variable = Some(variable);
            }
        }

        if let Some(rest) = line.split_once("let mut ").map(|(_, rest)| rest)
            && let Some(variable) = rest.split_whitespace().next()
        {
            let variable = variable.trim_end_matches(|character: char| {
                !character.is_ascii_alphanumeric() && character != '_'
            });
            if line.contains(".begin(") || line.contains(".begin_with(") {
                transaction_variables.insert(variable.to_owned());
            } else if !variable.is_empty() {
                pending_variable = Some(variable.to_owned());
            }
        }

        for variable in transaction_variables.iter() {
            let nested_forms = [
                format!("(*{variable}).begin("),
                format!("(*{variable}).begin_with("),
                format!("(&mut *{variable}).begin("),
                format!("(&mut *{variable}).begin_with("),
            ];
            if nested_forms.iter().any(|form| line.contains(form)) {
                return Err(format!(
                    "{}:{}: nested SQLx transaction begin is forbidden before upstream #4445 is fixed",
                    path.display(),
                    line_number + 1
                ));
            }
        }
    }
    Ok(())
}
