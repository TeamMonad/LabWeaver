//! One verified-apply stage around the existing service deployment plan.
use std::path::Path;
use std::process::Command;

use crate::AppError;

pub fn run(root: &Path, action: &str) -> Result<(), AppError> {
    let output = Command::new("python3")
        .arg(root.join("tools/verified_apply.py"))
        .arg(action)
        .output()
        .map_err(|_| AppError::BundleConfiguration {
            code: "LW_PLATFORM_CONFIGURATION_EXECUTOR_UNAVAILABLE",
        })?;
    if output.status.success() {
        return Ok(());
    }
    // Never forward arbitrary child output: the executor exposes stable codes
    // and stage names only, and dependencies may contain credential diagnostics.
    let detail = std::str::from_utf8(&output.stderr)
        .ok()
        .map(str::trim)
        .filter(|value| {
            value.len() <= 256
                && value.starts_with("LW_")
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_:-".contains(&byte))
        })
        .unwrap_or("LW_PLATFORM_CONFIGURATION_EXECUTOR_FAILED:transport");
    Err(AppError::PlatformImage {
        code: "LW_PLATFORM_CONFIGURATION_EXECUTOR_FAILED",
        detail: detail.to_owned(),
    })
}

pub fn execute(
    initial_configuration: bool,
    mut executor: impl FnMut(&str) -> Result<(), AppError>,
    deploy: impl FnOnce() -> Result<(), AppError>,
) -> Result<(), AppError> {
    if initial_configuration {
        deploy()?;
        return Err(AppError::PlatformImage {
            code: "LW_PLATFORM_CONFIGURATION_ADMIN_INITIALIZATION_REQUIRED",
            detail: "LW_PLATFORM_CONFIGURATION_ADMIN_INITIALIZATION_REQUIRED:prepare".to_owned(),
        });
    }
    executor("prepare")?;
    if let Err(error) = deploy() {
        // External effects may already exist. If Control is unreachable its
        // previous Applying fact remains unverified; never claim rollback.
        let _ = executor("fail");
        return Err(error);
    }
    executor("finish")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn initial_services_start_but_only_a_real_administrator_can_initialize_configuration() {
        let deployed = std::cell::Cell::new(false);
        let result = execute(
            true,
            |_| panic!("initial installation cannot report verified"),
            || {
                deployed.set(true);
                Ok(())
            },
        );
        assert!(deployed.get());
        assert_eq!(
            result.unwrap_err().diagnostic_code(),
            "LW_PLATFORM_CONFIGURATION_ADMIN_INITIALIZATION_REQUIRED"
        );
    }

    #[test]
    fn verified_apply_brackets_service_deployment_and_stops_on_failure() {
        for failure in [None, Some("prepare"), Some("deploy"), Some("finish")] {
            let seen = RefCell::new(Vec::new());
            let result = execute(
                false,
                |action| {
                    seen.borrow_mut().push(action.to_owned());
                    if failure == Some(action) {
                        Err(AppError::BundleConfiguration { code: "LW_TEST" })
                    } else {
                        Ok(())
                    }
                },
                || {
                    seen.borrow_mut().push("deploy".to_owned());
                    if failure == Some("deploy") {
                        Err(AppError::BundleConfiguration { code: "LW_TEST" })
                    } else {
                        Ok(())
                    }
                },
            );
            assert_eq!(result.is_err(), failure.is_some());
            let expected = match failure {
                Some("prepare") => vec!["prepare"],
                Some("deploy") => vec!["prepare", "deploy", "fail"],
                _ => vec!["prepare", "deploy", "finish"],
            };
            assert_eq!(*seen.borrow(), expected);
        }
    }
}
