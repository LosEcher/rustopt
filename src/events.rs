//! Event-sourced run ledger.
//!
//! Append-only JSONL is the single source of truth; `rustopt ledger` is a derived
//! projection. A dangling `run.start` (crash before `run.finish`) is reported as
//! `interrupted` rather than silently dropped.
//!
//! Evidence discipline: compiler stdout/stderr is **never** stored — only a
//! fingerprint and a byte count. Compiler logs can contain environment details
//! that do not belong in an audit trail.

use serde::Serialize;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn new_run_id() -> String {
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("run-{:013}-{seq}", now_ms())
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "t")]
pub enum Event {
    #[serde(rename = "run.start")]
    RunStart {
        run_id: String,
        version: String,
        ts: u64,
        command: String,
        cwd: String,
        manifest: String,
        variants: Vec<String>,
    },
    #[serde(rename = "variant.start")]
    VariantStart {
        run_id: String,
        variant: String,
        ts: u64,
        command: String,
    },
    #[serde(rename = "variant.finish")]
    VariantFinish {
        run_id: String,
        variant: String,
        ts: u64,
        status: String,
        bytes: Option<u64>,
        duration_ms: u64,
        stderr_hash: String,
        stderr_len: usize,
    },
    #[serde(rename = "guard.finding")]
    GuardFinding {
        run_id: String,
        id: String,
        severity: String,
        evidence: String,
    },
    #[serde(rename = "run.finish")]
    RunFinish {
        run_id: String,
        ts: u64,
        verdict: String,
        measured_bytes: Option<u64>,
        budget_bytes: Option<u64>,
        recommended: Option<String>,
    },
}

/// Append one event. Creates the parent directory on first use.
pub fn append(path: &Path, ev: &Event) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create ledger dir {}: {e}", parent.display()))?;
        }
    }
    let line = serde_json::to_string(ev).map_err(|e| format!("cannot encode event: {e}"))?;
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("cannot open ledger {}: {e}", path.display()))?;
    writeln!(f, "{line}").map_err(|e| format!("cannot write ledger {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_ids_are_unique_and_ordered() {
        let a = new_run_id();
        let b = new_run_id();
        assert_ne!(a, b);
        assert!(a.starts_with("run-"));
    }

    #[test]
    fn appended_events_are_tagged_jsonl() {
        let p = std::env::temp_dir().join(format!("rustopt-ev-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&p);
        append(
            &p,
            &Event::RunStart {
                run_id: "run-1".to_string(),
                version: "0.1.0".to_string(),
                ts: 5,
                command: "plan".to_string(),
                cwd: "/x".to_string(),
                manifest: "/x".to_string(),
                variants: vec!["tuned".to_string()],
            },
        )
        .unwrap();
        append(
            &p,
            &Event::VariantFinish {
                run_id: "run-1".to_string(),
                variant: "tuned".to_string(),
                ts: 6,
                status: "ok".to_string(),
                bytes: Some(10),
                duration_ms: 1,
                stderr_hash: "ab".to_string(),
                stderr_len: 2,
            },
        )
        .unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("\"t\":\"run.start\""));
        assert!(lines[1].contains("\"t\":\"variant.finish\""));
        assert!(
            !text.contains("\"stderr\":"),
            "raw stderr must never be stored"
        );
        let _ = std::fs::remove_file(&p);
    }
}
