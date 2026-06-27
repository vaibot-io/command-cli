//! Provenance endpoints: GET /v2/receipts (filtered list),
//! GET /v2/receipts/:id/events (one receipt's full chain), and the SSE stream
//! GET /v2/receipts/stream (consumed by `provenance tail`).

use serde::Deserialize;

use super::{ApiClient, ApiResult};

#[derive(Debug, Clone, Deserialize)]
pub struct ReceiptDetail {
    pub content_hash: String,
    pub receipt_id: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub agent_id: String,
    #[serde(default)]
    pub agent_name: String,
    #[serde(default)]
    pub tool: String,
    #[serde(default)]
    pub action_summary: String,
    #[serde(default)]
    pub risk_level: String,
    #[serde(default)]
    pub decision: String,
    #[serde(default)]
    pub approval_status: String,
    #[serde(default)]
    pub outcome: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ListReceiptsResponse {
    pub receipts: Vec<ReceiptDetail>,
    #[serde(default)]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EventEntry {
    pub event_name: String,
    #[serde(default)]
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EventsResponse {
    pub receipt: ReceiptDetail,
    #[serde(default)]
    pub events: Vec<EventEntry>,
}

/// Filters for the receipt list (mirrors the TS `ReceiptFilters`).
#[derive(Debug, Clone, Default)]
pub struct ReceiptFilters {
    pub approval_status: Option<String>,
    pub risk_level: Option<String>,
    pub decision: Option<String>,
    pub tool: Option<String>,
    pub limit: Option<u32>,
}

impl ApiClient {
    /// GET /v2/receipts with optional filters.
    pub async fn list_receipts(&self, filters: &ReceiptFilters) -> ApiResult<ListReceiptsResponse> {
        let mut params = vec![format!("limit={}", filters.limit.unwrap_or(50))];
        if let Some(s) = &filters.approval_status {
            params.push(format!("approval_status={}", urlencoding(s)));
        }
        if let Some(s) = &filters.risk_level {
            params.push(format!("risk_level={}", urlencoding(s)));
        }
        if let Some(s) = &filters.decision {
            params.push(format!("decision={}", urlencoding(s)));
        }
        if let Some(s) = &filters.tool {
            params.push(format!("tool={}", urlencoding(s)));
        }
        let path = format!("/v2/receipts?{}", params.join("&"));
        self.get(&path).await
    }

    /// GET /v2/receipts/:identifier/events — full chain for one receipt.
    pub async fn receipt_events(&self, identifier: &str) -> ApiResult<EventsResponse> {
        let path = format!("/v2/receipts/{}/events", urlencoding(identifier));
        self.get(&path).await
    }
}

/// Minimal percent-encoding for query/path segments (no extra dep).
fn urlencoding(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urlencoding_escapes_specials() {
        assert_eq!(urlencoding("a b/c"), "a%20b%2Fc");
        assert_eq!(urlencoding("grcpt_abc-1"), "grcpt_abc-1");
    }
}
