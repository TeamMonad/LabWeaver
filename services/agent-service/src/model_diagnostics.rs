//! Object-scoped, sanitized model failure details with a finite retention boundary.
//!
//! Usage/checkpoint receipts remain durable. This worker deletes only the separate detail rows
//! and abandoned model-owned temporary contexts, never resources, grades or user volumes.

use std::{path::Path, time::Duration};

use contracts::{
    AgentRunId, UtcTimestamp,
    http::{AgentRunDiagnostic, AgentRunDiagnostics},
};
use sqlx::{Postgres, Row, Transaction};

use crate::{
    claude_code::ClaudeCodeAudit,
    run_store::{AgentRunStoreError, PostgresAgentRunStore},
};

const RETENTION_DAYS: i64 = 30;
const CONTEXT_MARKER: &str = ".labweaver-model-context";
const CONTEXT_MARKER_VALUE: &[u8] = b"labweaver-model-context/v1\n";

pub(crate) async fn record_failure(
    transaction: &mut Transaction<'_, Postgres>,
    run_id: AgentRunId,
    attempt: u32,
    audit: &ClaudeCodeAudit,
    guidance: (&str, &str),
    now: UtcTimestamp,
) -> Result<(), AgentRunStoreError> {
    let expires_at = UtcTimestamp::from_utc(now.get() + time::Duration::days(RETENTION_DAYS))
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
    let detail = AgentRunDiagnostic {
        track: audit.track,
        preparation_failed: false,
        attempt,
        diagnostic_code: audit
            .diagnostic_code
            .clone()
            .ok_or(AgentRunStoreError::InvalidContract)?,
        reason: guidance.0.to_owned(),
        next_step: guidance.1.to_owned(),
        model: audit.model.clone(),
        runtime_binding: audit.runtime_binding.clone(),
        package_id: audit.package_id,
        package_revision: audit.package_revision,
        policy_id: audit.policy_id,
        policy_revision: audit.policy_revision,
        input_sha256: Some(audit.input_sha256.to_string()),
        output_sha256: audit.output_sha256.map(|hash| hash.to_string()),
        usage: audit.usage_observed.then_some(audit.usage),
        created_at: now,
        expires_at,
    };
    insert_detail(transaction, run_id, &detail).await
}

// Metadata projection remains readable when a legacy dispatch is denied for missing context.
// It does not deserialize file contents or treat identity as an authorization grant.
#[derive(serde::Deserialize)]
struct PackageIdentity {
    id: contracts::ProblemPackageId,
    revision: contracts::Revision,
}
pub(crate) async fn record_preparation_failure(
    transaction: &mut Transaction<'_, Postgres>,
    run_id: AgentRunId,
    track: contracts::authoring::AgentTrackKind,
    attempt: u32,
    diagnostic_code: &str,
    now: UtcTimestamp,
) -> Result<(), AgentRunStoreError> {
    let row = sqlx::query("SELECT package,policy FROM agent.agent_run_dispatches WHERE run_id=$1")
        .bind(run_id.as_uuid())
        .fetch_one(&mut **transaction)
        .await
        .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
    let package: PackageIdentity = serde_json::from_value(
        row.try_get("package")
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?,
    )
    .map_err(|_| AgentRunStoreError::InvalidContract)?;
    let policy: contracts::authoring::ProjectLlmEgressPolicy = serde_json::from_value(
        row.try_get("policy")
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?,
    )
    .map_err(|_| AgentRunStoreError::InvalidContract)?;
    let (reason, next_step) = match diagnostic_code {
        "LW_MODEL_INPUT_AUTHORIZATION_DENIED" | "LW_MODEL_INPUT_AUTHORIZATION_REQUIRED" => (
            "原会话或当前模型输入授权不再有效，未开始新的模型调用。",
            "检查当前对象权限；需要新的会话或输入时新建显式请求。",
        ),
        "LW_MODEL_INPUT_AUTHORIZATION_UNAVAILABLE" => (
            "模型输入授权服务连接不可用，未开始模型调用。",
            "请管理员检查 Agent 到 Control 的 HTTPS、服务身份与权限配置。",
        ),
        "LW_LLM_CANCELLED" => (
            "任务在模型输入准备阶段被取消。",
            "需要继续时显式新建任务；取消不删除已有用量记录。",
        ),
        _ => (
            "所选材料未通过模型输入准备检查。",
            "检查精确文件版本、敏感信息、策略及大小限制后重新提交。",
        ),
    };
    let expires_at = UtcTimestamp::from_utc(now.get() + time::Duration::days(RETENTION_DAYS))
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
    let detail = AgentRunDiagnostic {
        track,
        attempt,
        preparation_failed: true,
        diagnostic_code: diagnostic_code.to_owned(),
        reason: reason.to_owned(),
        next_step: next_step.to_owned(),
        model: policy.binding.model,
        runtime_binding: policy.binding.runtime_binding,
        package_id: package.id,
        package_revision: package.revision,
        policy_id: policy.id,
        policy_revision: policy.revision,
        input_sha256: None,
        output_sha256: None,
        usage: None,
        created_at: now,
        expires_at,
    };
    insert_detail(transaction, run_id, &detail).await
}

async fn insert_detail(
    transaction: &mut Transaction<'_, Postgres>,
    run_id: AgentRunId,
    detail: &AgentRunDiagnostic,
) -> Result<(), AgentRunStoreError> {
    let track = match detail.track {
        contracts::authoring::AgentTrackKind::Environment => "environment",
        contracts::authoring::AgentTrackKind::Evaluation => "evaluation",
        contracts::authoring::AgentTrackKind::WorkConfiguration => "work_configuration",
    };
    sqlx::query("INSERT INTO agent.model_diagnostics(run_id,track,attempt,detail,created_at,expires_at) VALUES($1,$2,$3,$4,$5,$6)")
        .bind(run_id.as_uuid()).bind(track).bind(i64::from(detail.attempt))
        .bind(serde_json::to_value(detail).map_err(|_| AgentRunStoreError::InvalidContract)?)
        .bind(detail.created_at.get()).bind(detail.expires_at.get())
        .execute(&mut **transaction).await.map_err(|_| AgentRunStoreError::PersistenceFailed)?;
    Ok(())
}

impl PostgresAgentRunStore {
    /// Reads only unexpired details. Control must authorize the current object before calling.
    pub async fn diagnostics(
        &self,
        run_id: AgentRunId,
    ) -> Result<AgentRunDiagnostics, AgentRunStoreError> {
        let run = self.load(run_id).await?;
        let rows = sqlx::query("SELECT detail FROM agent.model_diagnostics WHERE run_id=$1 AND expires_at>clock_timestamp() ORDER BY created_at DESC,track,attempt DESC LIMIT 100")
            .bind(run_id.as_uuid()).fetch_all(self.pool()).await.map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        let items = rows
            .into_iter()
            .map(|row| {
                serde_json::from_value(
                    row.try_get("detail")
                        .map_err(|_| AgentRunStoreError::PersistenceFailed)?,
                )
                .map_err(|_| AgentRunStoreError::InvalidContract)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(AgentRunDiagnostics {
            run_id,
            project_id: run.project_id,
            course_id: run.course_id,
            items,
        })
    }

    /// Bounded physical deletion; filtering in `diagnostics` enforces expiry even while offline.
    pub async fn purge_expired_model_diagnostics(&self) -> Result<u64, AgentRunStoreError> {
        sqlx::query("DELETE FROM agent.model_diagnostics WHERE (run_id,track,attempt) IN (SELECT run_id,track,attempt FROM agent.model_diagnostics WHERE expires_at<=clock_timestamp() ORDER BY expires_at LIMIT 100 FOR UPDATE SKIP LOCKED)")
            .execute(self.pool()).await.map(|result| result.rows_affected())
            .map_err(|_| AgentRunStoreError::PersistenceFailed)
    }

    /// The process supervisor owns this loop; database or filesystem failures are not ignored.
    pub async fn run_model_retention(&self) -> Result<(), AgentRunStoreError> {
        let mut interval = tokio::time::interval(Duration::from_mins(1));
        loop {
            interval.tick().await;
            self.purge_expired_model_diagnostics().await?;
            tokio::task::spawn_blocking(|| {
                purge_abandoned_contexts(&std::env::temp_dir(), std::time::SystemTime::now())
            })
            .await
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        }
    }
}

/// Marker records ownership, not arbitrary caller-selected paths.
pub(crate) fn mark_context(path: &Path) -> std::io::Result<()> {
    std::fs::write(path.join(CONTEXT_MARKER), CONTEXT_MARKER_VALUE)
}

fn purge_abandoned_contexts(root: &Path, now: std::time::SystemTime) -> std::io::Result<usize> {
    let mut removed = 0;
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with("labweaver-claude-")
            || !entry.file_type()?.is_dir()
        {
            continue;
        }
        let marker = entry.path().join(CONTEXT_MARKER);
        let Ok(metadata) = std::fs::symlink_metadata(&marker) else {
            continue;
        };
        if !metadata.is_file()
            || metadata.len() != CONTEXT_MARKER_VALUE.len() as u64
            || now.duration_since(metadata.modified()?).unwrap_or_default()
                < Duration::from_hours(30 * 24)
            || std::fs::read(&marker)? != CONTEXT_MARKER_VALUE
        {
            continue;
        }
        std::fs::remove_dir_all(entry.path())?;
        removed += 1;
        if removed == 100 {
            break;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abandoned_context_cleanup_preserves_unowned_and_recent_data()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let abandoned = root.path().join("labweaver-claude-abandoned");
        let recent = root.path().join("labweaver-claude-recent");
        let unowned = root.path().join("labweaver-claude-user-volume");
        let grades = root.path().join("grades");
        for directory in [&abandoned, &recent, &unowned, &grades] {
            std::fs::create_dir(directory)?;
            std::fs::write(directory.join("data"), "sensitive-marker")?;
        }
        mark_context(&abandoned)?;
        mark_context(&recent)?;
        let now = std::time::SystemTime::now();
        let old = now - Duration::from_hours(31 * 24);
        std::fs::File::open(abandoned.join(CONTEXT_MARKER))?
            .set_times(std::fs::FileTimes::new().set_modified(old))?;
        assert_eq!(purge_abandoned_contexts(root.path(), now)?, 1);
        assert!(!abandoned.exists());
        for directory in [&recent, &unowned, &grades] {
            assert_eq!(
                std::fs::read_to_string(directory.join("data"))?,
                "sensitive-marker"
            );
        }
        Ok(())
    }
}
