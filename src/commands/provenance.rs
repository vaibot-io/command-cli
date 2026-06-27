//! `provenance` group (alias `receipts`).
//!   list   [REAL]      — GET /v2/receipts (filtered).
//!   show   [REAL]      — GET /v2/receipts/:id/events (full chain).
//!   tail   [REAL,SSE]  — GET /v2/receipts/stream with [3,6,12,24,48]s backoff.
//!   anchor [STUB].

use clap::Subcommand;
use futures_util::StreamExt;

use crate::api::provenance::ReceiptFilters;
use crate::api::ApiResult;
use crate::error::CliError;

use super::{api_base, resolve_api_client};

#[derive(Subcommand, Debug)]
pub enum ProvenanceCmd {
    /// Browse historical governance receipts.
    List {
        /// Filter to a specific agent.
        #[arg(long)]
        agent: Option<String>,
        /// Filter by risk: low | medium | high | critical.
        #[arg(long)]
        risk: Option<String>,
        /// Filter by decision: allow | approval_required | deny.
        #[arg(long)]
        decision: Option<String>,
        /// Only receipts awaiting approval.
        #[arg(long)]
        pending: bool,
        /// Max receipts to return (default 20).
        #[arg(long, default_value = "20")]
        limit: String,
    },
    /// Show the full event chain for one receipt.
    Show {
        /// receipt_id or content_hash (prefix ok).
        id: String,
    },
    /// Stream live governance decisions in real time.
    Tail {
        /// Filter to a specific agent.
        #[arg(long)]
        agent: Option<String>,
        /// Filter by risk level.
        #[arg(long)]
        risk: Option<String>,
        /// Filter by decision.
        #[arg(long)]
        decision: Option<String>,
        /// Comma-separated event types.
        #[arg(long = "type")]
        types: Option<String>,
        /// Follow one receipt's full lifecycle.
        #[arg(long)]
        hash: Option<String>,
    },
    /// Show on-chain anchoring status (not yet wired).
    Anchor {
        /// Show batcher anchoring status.
        #[arg(long)]
        status: bool,
    },
}

pub async fn dispatch(cmd: ProvenanceCmd, api_url: Option<String>) -> Result<(), CliError> {
    match cmd {
        ProvenanceCmd::List {
            agent,
            risk,
            decision,
            pending,
            limit,
        } => list(agent, risk, decision, pending, limit, api_url).await,
        ProvenanceCmd::Show { id } => show(id, api_url).await,
        ProvenanceCmd::Tail {
            agent,
            risk,
            decision,
            types,
            hash,
        } => tail(agent, risk, decision, types, hash, api_url).await,
        ProvenanceCmd::Anchor { status: _ } => Err(CliError::stub("provenance anchor")),
    }
}

#[allow(clippy::too_many_arguments)]
async fn list(
    agent: Option<String>,
    risk: Option<String>,
    decision: Option<String>,
    pending: bool,
    limit: String,
    api_url: Option<String>,
) -> Result<(), CliError> {
    let limit_n = limit.parse::<u32>().unwrap_or(20);
    let filters = ReceiptFilters {
        approval_status: pending.then(|| "pending".to_string()),
        risk_level: risk,
        decision,
        tool: None, // agent filter is applied client-side below
        limit: Some(limit_n),
    };
    let client = resolve_api_client(api_url.as_deref(), None).await?;
    match client.list_receipts(&filters).await {
        ApiResult::Ok { data, .. } => {
            let mut rows = data.receipts;
            if let Some(a) = &agent {
                rows.retain(|r| &r.agent_id == a || &r.agent_name == a);
            }
            if rows.is_empty() {
                println!("No receipts found.");
                return Ok(());
            }
            println!("\n  {:<14}  {:<18}  {:<10}  {:<8}  tool", "time", "agent", "risk", "decision");
            for r in rows {
                println!(
                    "  {:<14}  {:<18}  {:<10}  {:<8}  {}",
                    truncate(&r.created_at, 14),
                    truncate(&r.agent_name, 18),
                    truncate(&r.risk_level, 10),
                    truncate(&r.decision, 8),
                    r.tool
                );
            }
            Ok(())
        }
        ApiResult::Err { error, status } => {
            Err(CliError::Runtime(format!("receipts list failed ({status}): {error}")))
        }
    }
}

async fn show(id: String, api_url: Option<String>) -> Result<(), CliError> {
    let client = resolve_api_client(api_url.as_deref(), None).await?;
    match client.receipt_events(&id).await {
        ApiResult::Ok { data, .. } => {
            let r = &data.receipt;
            println!("\nReceipt {}\n", r.receipt_id);
            println!("  content_hash:   {}", r.content_hash);
            println!("  agent:          {} ({})", r.agent_name, r.agent_id);
            println!("  tool:           {}", r.tool);
            println!("  action:         {}", r.action_summary);
            println!("  risk:           {}", r.risk_level);
            println!("  decision:       {}", r.decision);
            println!("  approval:       {}", r.approval_status);
            println!("  outcome:        {}", r.outcome);
            println!("  created_at:     {}", r.created_at);
            println!("\n  Events:");
            for e in &data.events {
                println!("    - {}", e.event_name);
            }
            Ok(())
        }
        ApiResult::Err { error, status } => {
            Err(CliError::Runtime(format!("receipt show failed ({status}): {error}")))
        }
    }
}

const BACKOFF_SECS: [u64; 5] = [3, 6, 12, 24, 48];

async fn tail(
    agent: Option<String>,
    risk: Option<String>,
    decision: Option<String>,
    types: Option<String>,
    hash: Option<String>,
    api_url: Option<String>,
) -> Result<(), CliError> {
    // Bearer through the broker; the stream client is built directly so we can
    // read a byte-stream (the ApiClient is JSON-oriented).
    let cred = crate::broker::get_broker().get(None).await?;
    let base = api_base(api_url.as_deref());

    let mut params: Vec<String> = Vec::new();
    if let Some(a) = &agent {
        params.push(format!("agent_id={a}"));
    }
    if let Some(r) = &risk {
        params.push(format!("risk_level={r}"));
    }
    if let Some(d) = &decision {
        params.push(format!("decision={d}"));
    }
    if let Some(t) = &types {
        params.push(format!("event_types={t}"));
    }
    if let Some(h) = &hash {
        params.push(format!("receipt_hash={h}"));
    }
    let qs = if params.is_empty() {
        String::new()
    } else {
        format!("?{}", params.join("&"))
    };
    let url = format!("{base}/v2/receipts/stream{qs}");

    let client = reqwest::Client::builder()
        .build()
        .map_err(|e| CliError::Runtime(format!("http client: {e}")))?;

    println!("\n  Watching governance stream — Ctrl+C to stop\n");

    let mut attempt = 0usize;
    loop {
        match connect_stream(&client, &url, &cred.access_token).await {
            StreamOutcome::Closed => break,
            StreamOutcome::Error => {
                attempt += 1;
                if attempt > BACKOFF_SECS.len() {
                    return Err(CliError::Runtime(format!(
                        "Stream disconnected after {attempt} retries. Giving up."
                    )));
                }
                let wait = BACKOFF_SECS[attempt - 1];
                println!("\n  Reconnecting in {wait}s...\n");
                tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
            }
        }
    }
    Ok(())
}

enum StreamOutcome {
    Closed,
    Error,
}

async fn connect_stream(client: &reqwest::Client, url: &str, bearer: &str) -> StreamOutcome {
    let resp = match client
        .get(url)
        .bearer_auth(bearer)
        .header("Accept", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return StreamOutcome::Error,
    };
    if !resp.status().is_success() {
        eprintln!("\n  Stream error {}", resp.status().as_u16());
        return StreamOutcome::Error;
    }

    let mut stream = resp.bytes_stream();
    let mut buffer = String::new();
    while let Some(chunk) = stream.next().await {
        let bytes = match chunk {
            Ok(b) => b,
            Err(_) => return StreamOutcome::Error,
        };
        buffer.push_str(&String::from_utf8_lossy(&bytes));
        // Split on SSE block boundaries.
        while let Some(idx) = buffer.find("\n\n") {
            let block: String = buffer.drain(..idx + 2).collect();
            if let Some((event, data)) = parse_sse_block(&block) {
                if event == "heartbeat" {
                    continue;
                }
                println!("  [{event}] {data}");
            }
        }
    }
    StreamOutcome::Closed
}

/// Parse an SSE block into (event, data). Returns `None` if no data line.
fn parse_sse_block(block: &str) -> Option<(String, String)> {
    let mut event = "message".to_string();
    let mut data = String::new();
    for line in block.lines() {
        if let Some(v) = line.strip_prefix("event:") {
            event = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(v.trim());
        }
    }
    if data.is_empty() {
        None
    } else {
        Some((event, data))
    }
}

fn truncate(s: &str, max: usize) -> String {
    // Char-boundary safe: server-returned fields (agent_name, etc.) may be
    // non-ASCII; byte-slicing would panic mid-list. Count + take by char.
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let kept: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{kept}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sse_block_extracts_event_and_data() {
        let block = "event: receipt.decided\ndata: {\"x\":1}\n\n";
        let (e, d) = parse_sse_block(block).unwrap();
        assert_eq!(e, "receipt.decided");
        assert_eq!(d, "{\"x\":1}");
    }

    #[test]
    fn parse_sse_block_without_data_is_none() {
        assert!(parse_sse_block("event: heartbeat\n\n").is_none());
    }
}
