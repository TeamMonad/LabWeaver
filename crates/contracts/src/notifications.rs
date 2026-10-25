//! Private inbox metadata; object links never confer object authorization.
use crate::{EventId, ProjectId, UtcTimestamp};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationKind {
    ResourceDecision,
    LeaseChanged,
    LeaseExpiryReminder,
    ShortLease,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationEmailState {
    Disabled,
    Pending,
    Sent,
    Failed,
    Exhausted,
    NoVerifiedEmail,
    DeliveryUnknown,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Notification {
    pub id: EventId,
    pub event_id: EventId,
    pub project_id: ProjectId,
    pub kind: NotificationKind,
    pub object_url: String,
    pub summary: String,
    pub observed_at: UtcTimestamp,
    pub deadline: Option<UtcTimestamp>,
    pub read_at: Option<UtcTimestamp>,
    pub email_state: NotificationEmailState,
    pub email_attempts: u16,
    pub email_diagnostic: Option<String>,
    pub email_detail: Option<String>,
}
pub type NotificationPage = crate::http::CursorPage<Notification>;

#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NotificationListQuery {
    pub cursor: Option<String>,
    pub limit: Option<u16>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NotificationUnreadCount {
    pub unread_count: u64,
}
