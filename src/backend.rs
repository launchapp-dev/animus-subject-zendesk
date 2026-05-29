use std::collections::{BTreeMap, BTreeSet};

use animus_plugin_protocol::{HealthCheckResult, HealthStatus};
use animus_subject_protocol::{
    BackendError, CustomFieldKind, CustomFieldSpec, EventStream, Subject, SubjectBackend,
    SubjectFilter, SubjectId, SubjectList, SubjectPatch, SubjectSchema, SubjectStatus,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use serde_json::{json, Value};

use crate::config::ZendeskConfig;

const ID_PREFIX: &str = "zendesk:";
const KIND_TICKET: &str = "ticket";

pub struct ZendeskBackend {
    config: ZendeskConfig,
    client: reqwest::Client,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NativeId {
    ticket_id: u64,
}

impl NativeId {
    fn parse(id: &SubjectId) -> Result<Self, BackendError> {
        let raw = id.as_str();
        let rest = raw.strip_prefix(ID_PREFIX).ok_or_else(|| {
            BackendError::InvalidRequest(format!(
                "expected Zendesk subject id shaped zendesk:<ticket-id>, got {raw}"
            ))
        })?;
        let ticket_id = rest.parse::<u64>().map_err(|_| {
            BackendError::InvalidRequest(format!("invalid Zendesk ticket id in {raw}"))
        })?;
        Ok(Self { ticket_id })
    }

    fn subject_id(ticket_id: u64) -> SubjectId {
        SubjectId::new(format!("{ID_PREFIX}{ticket_id}"))
    }
}

impl ZendeskBackend {
    pub fn new(config: ZendeskConfig) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(concat!(
                "animus-subject-zendesk/",
                env!("CARGO_PKG_VERSION")
            ))
            .build()?;
        Ok(Self { config, client })
    }

    fn ensure_configured(&self) -> Result<(), BackendError> {
        if self.config.base_url.is_empty() {
            return Err(BackendError::InvalidRequest(
                "ZENDESK_BASE_URL or ZENDESK_SUBDOMAIN must be set".into(),
            ));
        }
        if self.config.email.is_none() || self.config.api_token.is_none() {
            return Err(BackendError::PermissionDenied(
                "ZENDESK_EMAIL and ZENDESK_API_TOKEN must be set".into(),
            ));
        }
        Ok(())
    }

    fn request(
        &self,
        method: reqwest::Method,
        path: &str,
    ) -> Result<reqwest::RequestBuilder, BackendError> {
        self.ensure_configured()?;
        let url = format!("{}{}", self.config.base_url, path);
        let username = format!("{}/token", self.config.email.as_deref().unwrap_or_default());
        Ok(self
            .client
            .request(method, url)
            .header("Accept", "application/json")
            .basic_auth(username, self.config.api_token.as_deref()))
    }

    async fn json_request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, BackendError> {
        let mut req = self.request(method, path)?;
        if let Some(body) = body {
            req = req.json(&body);
        }
        let response = req
            .send()
            .await
            .map_err(|e| BackendError::Unavailable(e.to_string()))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| BackendError::Unavailable(e.to_string()))?;

        if status == StatusCode::NOT_FOUND {
            return Err(BackendError::NotFound(path.to_string()));
        }
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(BackendError::PermissionDenied(format!(
                "Zendesk API returned {status}: {text}"
            )));
        }
        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(BackendError::Unavailable(format!(
                "Zendesk API rate limited request: {text}"
            )));
        }
        if !status.is_success() {
            return Err(BackendError::Unavailable(format!(
                "Zendesk API returned {status}: {text}"
            )));
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).map_err(|e| BackendError::Other(e.into()))
    }

    async fn fetch_ticket(&self, id: &NativeId) -> Result<Value, BackendError> {
        let value = self
            .json_request(
                reqwest::Method::GET,
                &format!("/api/v2/tickets/{}.json", id.ticket_id),
                None,
            )
            .await?;
        value
            .get("ticket")
            .cloned()
            .ok_or_else(|| BackendError::Other(anyhow::anyhow!("Zendesk response missing ticket")))
    }

    fn ticket_to_subject(&self, ticket: &Value) -> Result<Subject, BackendError> {
        let ticket_id = ticket.get("id").and_then(Value::as_u64).ok_or_else(|| {
            BackendError::Other(anyhow::anyhow!("Zendesk ticket missing id: {ticket}"))
        })?;
        let status = ticket
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("new")
            .to_string();

        let labels = ticket
            .get("tags")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect::<Vec<_>>();

        let priority = ticket
            .get("priority")
            .and_then(Value::as_str)
            .and_then(priority_from_native);

        let requester_id = ticket.get("requester_id").and_then(Value::as_u64);
        let submitter_id = ticket.get("submitter_id").and_then(Value::as_u64);
        let group_id = ticket.get("group_id").and_then(Value::as_u64);
        let assignee = ticket
            .get("assignee_id")
            .and_then(Value::as_u64)
            .map(|id| id.to_string());

        let mut custom = BTreeMap::new();
        if let Some(requester_id) = requester_id {
            custom.insert("requester_id".to_string(), json!(requester_id));
        }
        if let Some(submitter_id) = submitter_id {
            custom.insert("submitter_id".to_string(), json!(submitter_id));
        }
        if let Some(group_id) = group_id {
            custom.insert("group_id".to_string(), json!(group_id));
        }
        if let Some(priority) = ticket.get("priority").and_then(Value::as_str) {
            custom.insert("priority".to_string(), json!(priority));
        }

        Ok(Subject {
            id: NativeId::subject_id(ticket_id),
            kind: KIND_TICKET.to_string(),
            title: ticket
                .get("subject")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            description: ticket
                .get("description")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            status: map_status(&status),
            priority,
            assignee,
            labels,
            parent: None,
            children: Vec::new(),
            url: Some(format!(
                "{}/agent/tickets/{ticket_id}",
                self.config.base_url
            )),
            created_at: parse_ts(ticket.get("created_at"))?,
            updated_at: parse_ts(ticket.get("updated_at"))?,
            custom,
            native_status: Some(status),
            status_metadata: Value::Null,
            attachments: Vec::new(),
        })
    }

    fn subject_matches_filter(subject: &Subject, filter: &SubjectFilter) -> bool {
        if !filter.kind.is_empty() && !filter.kind.contains(&subject.kind) {
            return false;
        }
        if !filter.status.is_empty() && !filter.status.contains(&subject.status) {
            return false;
        }
        if !filter.assignee.is_empty() {
            match &subject.assignee {
                Some(assignee) if filter.assignee.contains(assignee) => {}
                _ => return false,
            }
        }
        if !filter.labels_any.is_empty()
            && !filter
                .labels_any
                .iter()
                .any(|label| subject.labels.contains(label))
        {
            return false;
        }
        if !filter
            .labels_all
            .iter()
            .all(|label| subject.labels.contains(label))
        {
            return false;
        }
        if let Some(updated_since) = filter.updated_since {
            if subject.updated_at < updated_since {
                return false;
            }
        }
        true
    }
}

#[async_trait]
impl SubjectBackend for ZendeskBackend {
    async fn list(&self, filter: SubjectFilter) -> Result<SubjectList, BackendError> {
        let per_page = filter.limit.unwrap_or(50).clamp(1, 100);
        let value = if let Some(query) = &self.config.query {
            self.json_request(
                reqwest::Method::GET,
                &format!(
                    "/api/v2/search.json?query={}&per_page={per_page}",
                    encode(query)
                ),
                None,
            )
            .await?
        } else {
            self.json_request(
                reqwest::Method::GET,
                &format!(
                    "/api/v2/tickets.json?per_page={per_page}&sort_by=updated_at&sort_order=desc"
                ),
                None,
            )
            .await?
        };

        let tickets = value
            .get("tickets")
            .or_else(|| value.get("results"))
            .and_then(Value::as_array)
            .ok_or_else(|| {
                BackendError::Other(anyhow::anyhow!(
                    "Zendesk ticket list missing tickets/results"
                ))
            })?;
        let mut subjects = Vec::new();
        for ticket in tickets {
            let subject = self.ticket_to_subject(ticket)?;
            if Self::subject_matches_filter(&subject, &filter) {
                subjects.push(subject);
            }
        }

        Ok(SubjectList {
            subjects,
            next_cursor: None,
            fetched_at: Utc::now(),
        })
    }

    async fn get(&self, id: &SubjectId) -> Result<Subject, BackendError> {
        let native = NativeId::parse(id)?;
        let ticket = self.fetch_ticket(&native).await?;
        self.ticket_to_subject(&ticket)
    }

    async fn update(&self, id: &SubjectId, patch: SubjectPatch) -> Result<Subject, BackendError> {
        let native = NativeId::parse(id)?;
        let current = self.get(id).await?;
        let mut ticket = serde_json::Map::new();

        if let Some(status) = patch.status {
            ticket.insert("status".to_string(), json!(status_to_native(status)));
        }

        if let Some(assignee) = patch.assignee {
            match assignee {
                Some(raw_id) => {
                    let parsed = raw_id.parse::<u64>().map_err(|_| {
                        BackendError::InvalidRequest(
                            "Zendesk assignee must be a numeric user id".into(),
                        )
                    })?;
                    ticket.insert("assignee_id".to_string(), json!(parsed));
                }
                None => {
                    ticket.insert("assignee_id".to_string(), Value::Null);
                }
            }
        }

        if !patch.labels_add.is_empty() || !patch.labels_remove.is_empty() {
            let mut labels: BTreeSet<String> = current.labels.into_iter().collect();
            for label in patch.labels_add {
                labels.insert(label);
            }
            for label in patch.labels_remove {
                labels.remove(&label);
            }
            ticket.insert(
                "tags".to_string(),
                json!(labels.into_iter().collect::<Vec<_>>()),
            );
        }

        if let Some(comment) = patch.comment.filter(|s| !s.is_empty()) {
            ticket.insert(
                "comment".to_string(),
                json!({
                    "body": comment,
                    "public": false
                }),
            );
        }

        if !ticket.is_empty() {
            self.json_request(
                reqwest::Method::PUT,
                &format!("/api/v2/tickets/{}.json", native.ticket_id),
                Some(json!({ "ticket": ticket })),
            )
            .await?;
        }

        self.get(id).await
    }

    async fn watch(&self) -> Option<EventStream> {
        None
    }

    fn schema(&self) -> SubjectSchema {
        SubjectSchema {
            kinds: vec![KIND_TICKET.to_string()],
            status_values: vec![
                SubjectStatus::Ready,
                SubjectStatus::InProgress,
                SubjectStatus::Blocked,
                SubjectStatus::Done,
                SubjectStatus::Cancelled,
            ],
            supports_watch: false,
            supports_create: false,
            supports_pagination: false,
            native_status_values: vec![
                "new".to_string(),
                "open".to_string(),
                "pending".to_string(),
                "hold".to_string(),
                "solved".to_string(),
                "closed".to_string(),
            ],
            status_dispatch_hints: Vec::new(),
            custom_fields: vec![
                CustomFieldSpec {
                    key: "requester_id".to_string(),
                    kind: CustomFieldKind::Number,
                    values: None,
                },
                CustomFieldSpec {
                    key: "submitter_id".to_string(),
                    kind: CustomFieldKind::Number,
                    values: None,
                },
                CustomFieldSpec {
                    key: "group_id".to_string(),
                    kind: CustomFieldKind::Number,
                    values: None,
                },
                CustomFieldSpec {
                    key: "priority".to_string(),
                    kind: CustomFieldKind::String,
                    values: Some(vec![
                        "low".to_string(),
                        "normal".to_string(),
                        "high".to_string(),
                        "urgent".to_string(),
                    ]),
                },
            ],
        }
    }

    async fn health(&self) -> Result<HealthCheckResult, BackendError> {
        let missing_base = self.config.base_url.is_empty();
        let missing_auth = self.config.email.is_none() || self.config.api_token.is_none();
        let status = if missing_base || missing_auth {
            HealthStatus::Unhealthy
        } else {
            HealthStatus::Healthy
        };
        let last_error = match (missing_base, missing_auth) {
            (true, true) => Some(
                "ZENDESK_BASE_URL or ZENDESK_SUBDOMAIN, ZENDESK_EMAIL, and ZENDESK_API_TOKEN unset"
                    .to_string(),
            ),
            (true, false) => Some("ZENDESK_BASE_URL or ZENDESK_SUBDOMAIN unset".to_string()),
            (false, true) => Some("ZENDESK_EMAIL or ZENDESK_API_TOKEN unset".to_string()),
            (false, false) => None,
        };
        Ok(HealthCheckResult {
            status,
            uptime_ms: None,
            memory_usage_bytes: None,
            last_error,
        })
    }
}

fn parse_ts(value: Option<&Value>) -> Result<DateTime<Utc>, BackendError> {
    let raw = value.and_then(Value::as_str).ok_or_else(|| {
        BackendError::Other(anyhow::anyhow!("Zendesk ticket missing timestamp field"))
    })?;
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| BackendError::Other(e.into()))
}

fn map_status(status: &str) -> SubjectStatus {
    match status {
        "new" => SubjectStatus::Ready,
        "open" => SubjectStatus::InProgress,
        "pending" | "hold" => SubjectStatus::Blocked,
        "solved" | "closed" => SubjectStatus::Done,
        _ => SubjectStatus::Ready,
    }
}

fn status_to_native(status: SubjectStatus) -> &'static str {
    match status {
        SubjectStatus::Ready | SubjectStatus::InProgress => "open",
        SubjectStatus::Blocked => "pending",
        SubjectStatus::Done | SubjectStatus::Cancelled => "solved",
    }
}

fn priority_from_native(priority: &str) -> Option<u8> {
    match priority {
        "urgent" => Some(4),
        "high" => Some(3),
        "normal" => Some(2),
        "low" => Some(1),
        _ => None,
    }
}

fn encode(value: &str) -> String {
    urlencoding::encode(value).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_ticket() -> Value {
        json!({
            "id": 42,
            "subject": "Checkout is broken",
            "description": "Customer cannot pay",
            "status": "open",
            "priority": "high",
            "url": "https://example.zendesk.com/api/v2/tickets/42.json",
            "created_at": "2026-05-28T00:00:00Z",
            "updated_at": "2026-05-28T01:00:00Z",
            "tags": ["checkout", "vip"],
            "assignee_id": 1234,
            "requester_id": 9876,
            "submitter_id": 9876,
            "group_id": 55
        })
    }

    #[test]
    fn native_id_parses() {
        let parsed = NativeId::parse(&SubjectId::new("zendesk:123")).unwrap();
        assert_eq!(parsed.ticket_id, 123);
    }

    #[test]
    fn ticket_maps_to_subject() {
        let backend =
            ZendeskBackend::new(ZendeskConfig::for_testing("https://example.zendesk.com")).unwrap();
        let subject = backend.ticket_to_subject(&sample_ticket()).unwrap();
        assert_eq!(subject.id.as_str(), "zendesk:42");
        assert_eq!(subject.kind, KIND_TICKET);
        assert_eq!(subject.status, SubjectStatus::InProgress);
        assert_eq!(subject.priority, Some(3));
        assert_eq!(subject.assignee.as_deref(), Some("1234"));
        assert_eq!(
            subject.url.as_deref(),
            Some("https://example.zendesk.com/agent/tickets/42")
        );
    }

    #[test]
    fn maps_native_statuses() {
        assert_eq!(map_status("new"), SubjectStatus::Ready);
        assert_eq!(map_status("open"), SubjectStatus::InProgress);
        assert_eq!(map_status("pending"), SubjectStatus::Blocked);
        assert_eq!(map_status("hold"), SubjectStatus::Blocked);
        assert_eq!(map_status("solved"), SubjectStatus::Done);
    }
}
