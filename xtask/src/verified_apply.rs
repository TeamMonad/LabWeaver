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
    // A succeeded attempt is final: re-entry after a lost success response only confirms the
    // live replicas and never deploys again. New writes need a new authorized attempt.
    match executor("verified") {
        Ok(()) => return Ok(()),
        Err(error) if is_pending(&error) => {}
        Err(error) => return Err(error),
    }
    executor("prepare")?;
    if let Err(error) = deploy() {
        // External effects may already exist. If Control is unreachable its previous
        // Applying fact remains unverified; never claim rollback. A failed report is surfaced
        // with the original deployment error rather than discarded.
        if let Err(report) = executor("fail") {
            return Err(AppError::PlatformImage {
                code: "LW_PLATFORM_CONFIGURATION_EXECUTOR_FAILED",
                detail: format!(
                    "{} (failure report not recorded: {})",
                    error.diagnostic_code(),
                    report.diagnostic_code()
                ),
            });
        }
        return Err(error);
    }
    executor("finish")
}

fn is_pending(error: &AppError) -> bool {
    matches!(error, AppError::PlatformImage { detail, .. }
        if detail == "LW_PLATFORM_CONFIGURATION_APPLY_PENDING:verified")
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

    fn pending() -> AppError {
        AppError::PlatformImage {
            code: "LW_PLATFORM_CONFIGURATION_EXECUTOR_FAILED",
            detail: "LW_PLATFORM_CONFIGURATION_APPLY_PENDING:verified".to_owned(),
        }
    }

    #[test]
    fn verified_apply_brackets_service_deployment_and_stops_on_failure() {
        for failure in [None, Some("prepare"), Some("deploy"), Some("finish")] {
            let seen = RefCell::new(Vec::new());
            let result = execute(
                false,
                |action| {
                    seen.borrow_mut().push(action.to_owned());
                    if action == "verified" {
                        Err(pending())
                    } else if failure == Some(action) {
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
                Some("prepare") => vec!["verified", "prepare"],
                Some("deploy") => vec!["verified", "prepare", "deploy", "fail"],
                _ => vec!["verified", "prepare", "deploy", "finish"],
            };
            assert_eq!(*seen.borrow(), expected);
        }
    }

    #[test]
    fn succeeded_attempt_replay_never_deploys_or_reports() {
        let seen = RefCell::new(Vec::new());
        execute(
            false,
            |action| {
                seen.borrow_mut().push(action.to_owned());
                Ok(())
            },
            || panic!("a succeeded attempt must not deploy again"),
        )
        .expect("confirmed succeeded attempt");
        assert_eq!(*seen.borrow(), vec!["verified"]);
    }

    #[test]
    fn unreadable_intent_stops_before_any_write() {
        let seen = RefCell::new(Vec::new());
        let result = execute(
            false,
            |action| {
                seen.borrow_mut().push(action.to_owned());
                Err(AppError::BundleConfiguration { code: "LW_TEST" })
            },
            || panic!("an unreadable intent must not deploy"),
        );
        assert!(result.is_err());
        assert_eq!(*seen.borrow(), vec!["verified"]);
    }

    #[test]
    fn failed_failure_report_is_surfaced_with_the_deployment_error() {
        let result = execute(
            false,
            |action| match action {
                "verified" => Err(pending()),
                "fail" => Err(AppError::BundleConfiguration {
                    code: "LW_REPORT_UNREACHABLE",
                }),
                _ => Ok(()),
            },
            || Err(AppError::BundleConfiguration { code: "LW_DEPLOY" }),
        );
        let error = result.unwrap_err();
        assert_eq!(
            error.diagnostic_code(),
            "LW_PLATFORM_CONFIGURATION_EXECUTOR_FAILED"
        );
        let text = error.to_string();
        assert!(text.contains("LW_DEPLOY") && text.contains("LW_REPORT_UNREACHABLE"));
    }
}
