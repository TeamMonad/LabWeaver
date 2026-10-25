//! Optional SMTP boundary. A delivery claim cannot issue business commands.
use super::{Notification, NotificationError};
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    transport::smtp::{
        authentication::Credentials,
        client::{Certificate, CertificateStore, Tls, TlsParameters},
    },
};
use serde::Deserialize;
use sqlx::{PgPool, Row};
use std::{path::Path, time::Duration};
use uuid::Uuid;

/// Private, Bundle-derived runtime notification settings.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NotificationConfiguration {
    /// Optional transport; absence leaves the complete in-app inbox enabled.
    pub smtp: Option<SmtpConfiguration>,
    /// Reminder windows derived from the same Rust-validated lifecycle settings, including first install.
    pub expiry_reminder_seconds: Vec<u64>,
}
/// Validated SMTP connection fields; credential contents remain in private files.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SmtpConfiguration {
    /// TLS peer name and relay host.
    pub host: String,
    /// Relay TCP port.
    pub port: u16,
    /// Required TLS mode: tls or starttls.
    pub tls: String,
    /// Relay authentication username, not a recipient mapping.
    pub username: String,
    /// Mounted private relay password file.
    pub password_file: String,
    /// Mounted PEM trust anchor file.
    pub ca_file: String,
    /// Administrator-owned sender mailbox.
    pub from: String,
    /// HTTPS portal origin used for permission-checked object links.
    pub portal_base_url: String,
}
/// Bounded SMTP sender, independent of billing and runtime operations.
pub struct SmtpWorker {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: lettre::message::Mailbox,
    portal: String,
}
impl NotificationConfiguration {
    /// Reads the Bundle-derived transport and reminder settings without an alternate config source.
    pub fn load(path: &Path) -> Result<Self, NotificationError> {
        let bytes = std::fs::read(path).map_err(|_| NotificationError::Invalid)?;
        let config: Self =
            contracts::parse_strict_json(&bytes).map_err(|_| NotificationError::Invalid)?;
        let windows = &config.expiry_reminder_seconds;
        if windows.len() > 16
            || windows.iter().enumerate().any(|(index, window)| {
                !(1..=31_536_000).contains(window) || windows[..index].contains(window)
            })
        {
            return Err(NotificationError::Invalid);
        }
        Ok(config)
    }
}
impl SmtpWorker {
    /// Loads credential files and builds a mandatory-TLS SMTP connection.
    pub fn new(config: SmtpConfiguration) -> Result<Self, NotificationError> {
        let portal =
            reqwest::Url::parse(&config.portal_base_url).map_err(|_| NotificationError::Invalid)?;
        if config.host.is_empty()
            || config.port == 0
            || portal.scheme() != "https"
            || portal.host_str().is_none()
            || !portal.username().is_empty()
            || portal.password().is_some()
            || portal.query().is_some()
            || portal.fragment().is_some()
            || portal.path() != "/"
        {
            return Err(NotificationError::Invalid);
        }
        let password = std::fs::read_to_string(&config.password_file)
            .map_err(|_| NotificationError::Invalid)?;
        if password.trim().is_empty() || password.len() > 16_384 {
            return Err(NotificationError::Invalid);
        }
        let ca = Certificate::from_pem(
            &std::fs::read(&config.ca_file).map_err(|_| NotificationError::Invalid)?,
        )
        .map_err(|_| NotificationError::Invalid)?;
        let tls = TlsParameters::builder(config.host.clone())
            .certificate_store(CertificateStore::None)
            .add_root_certificate(ca)
            .build()
            .map_err(|_| NotificationError::Invalid)?;
        let tls = match config.tls.as_str() {
            "tls" => Tls::Wrapper(tls),
            "starttls" => Tls::Required(tls),
            _ => return Err(NotificationError::Invalid),
        };
        let transport = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&config.host)
            .port(config.port)
            .tls(tls)
            .credentials(Credentials::new(
                config.username,
                password.trim_end().to_owned(),
            ))
            .timeout(Some(Duration::from_secs(5)))
            .build();
        Ok(Self {
            transport,
            from: config
                .from
                .parse()
                .map_err(|_| NotificationError::Invalid)?,
            portal: portal.as_str().trim_end_matches('/').to_owned(),
        })
    }
    /// Attempts SMTP at most once when delivery is uncertain. Only an explicit temporary rejection
    /// may retry; expired in-flight claims remain visible as unknown and are never resent.
    pub async fn deliver_one(
        &self,
        pool: &PgPool,
        shutdown: &tokio::sync::watch::Receiver<bool>,
    ) -> Result<bool, NotificationError> {
        if *shutdown.borrow() {
            return Ok(false);
        }
        sqlx::query("UPDATE control.notifications SET email_state='delivery_unknown',email_diagnostic='LW_NOTIFICATION_SMTP_DELIVERY_UNKNOWN',email_claim_id=NULL,email_next_at=NULL WHERE email_state='sending' AND email_next_at<=clock_timestamp()")
            .execute(pool).await?;
        // Expiry recovery can wait on PostgreSQL. A signal received during that wait must
        // stop acquisition before the next, independent claim statement starts.
        if *shutdown.borrow() {
            return Ok(false);
        }
        let claim = Uuid::now_v7();
        let row = sqlx::query("UPDATE control.notifications n SET email_state='sending',email_claim_id=$1,email_attempts=email_attempts+1,email_next_at=clock_timestamp()+interval '60 seconds' WHERE notification_id=(SELECT q.notification_id FROM control.notifications q WHERE NOT q.cancelled AND q.due_at<=clock_timestamp() AND (q.reminder_seconds<=0 OR q.deadline_at>clock_timestamp()) AND q.email_state IN ('pending','failed') AND q.email_attempts<5 AND q.email_next_at<=clock_timestamp() AND EXISTS(SELECT 1 FROM control.project_memberships m WHERE m.project_id=q.project_id AND m.actor_id=q.recipient_id AND m.state='active' AND (m.expires_at IS NULL OR m.expires_at>clock_timestamp())) AND (q.course_id IS NULL OR EXISTS(SELECT 1 FROM control.course_memberships cm WHERE cm.course_id=q.course_id AND cm.actor_id=q.recipient_id AND cm.state='active' AND (cm.expires_at IS NULL OR cm.expires_at>clock_timestamp()))) ORDER BY q.email_next_at,q.ordinal FOR UPDATE SKIP LOCKED LIMIT 1) RETURNING n.*")
            .bind(claim).fetch_optional(pool).await?;
        let Some(row) = row else {
            return Ok(false);
        };
        let id: Uuid = row.try_get("notification_id")?;
        let recipient: Uuid = row.try_get("recipient_id")?;
        let email: Option<String> = sqlx::query_scalar(
            "SELECT verified_email FROM access.notification_recipients WHERE actor_id=$1",
        )
        .bind(recipient)
        .fetch_optional(pool)
        .await?
        .flatten();
        let (state, diagnostic, retry) = if let Some(email) = email {
            let notice: Notification = serde_json::from_value(row.try_get("contract")?)?;
            let message = email.parse().ok().and_then(|to| {
                Message::builder()
                    .from(self.from.clone())
                    .to(to)
                    .subject("LabWeaver 通知")
                    .message_id(Some(format!("<{id}@notifications.labweaver>")))
                    .body(format!(
                        "{}\n{}{}\n\n打开链接仍需登录并拥有当前对象权限。",
                        notice.summary, self.portal, notice.object_url
                    ))
                    .ok()
            });
            if let Some(message) = message {
                match tokio::time::timeout(Duration::from_secs(10), self.transport.send(message))
                    .await
                {
                    Ok(Ok(_)) => ("sent", None, false),
                    Ok(Err(error)) if error.is_transient() => {
                        if row.try_get::<i16, _>("email_attempts")? >= 5 {
                            ("exhausted", Some("LW_NOTIFICATION_SMTP_FAILED"), false)
                        } else {
                            ("failed", Some("LW_NOTIFICATION_SMTP_FAILED"), true)
                        }
                    }
                    Ok(Err(error)) if error.is_permanent() => {
                        ("failed", Some("LW_NOTIFICATION_SMTP_FAILED"), false)
                    }
                    _ => (
                        "delivery_unknown",
                        Some("LW_NOTIFICATION_SMTP_DELIVERY_UNKNOWN"),
                        false,
                    ),
                }
            } else {
                ("failed", Some("LW_NOTIFICATION_SMTP_FAILED"), false)
            }
        } else {
            ("no_verified_email", None, false)
        };
        sqlx::query("UPDATE control.notifications SET email_state=$3,email_diagnostic=$4,email_next_at=CASE WHEN $5 THEN clock_timestamp()+interval '30 seconds' ELSE NULL END,email_claim_id=NULL WHERE notification_id=$1 AND email_claim_id=$2")
            .bind(id).bind(claim).bind(state).bind(diagnostic).bind(retry).execute(pool).await?;
        Ok(true)
    }
    /// Runs until the supervisor requests shutdown. A claimed message is completed before exit;
    /// the persisted claim timeout makes a crash recoverable without owner side effects.
    pub async fn run(
        self,
        pool: PgPool,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Result<(), NotificationError> {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        loop {
            if *shutdown.borrow() {
                return Ok(());
            }
            tokio::select! {
                biased;
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { return Ok(()); }
                }
                _ = interval.tick() => {
                    if let Err(error) = self.deliver_one(&pool, &shutdown).await { tracing::error!(diagnostic=%error,"notification email persistence unavailable"); }
                }
            }
        }
    }
}
