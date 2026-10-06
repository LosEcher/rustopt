//! Ledger projections: turn `runs.jsonl` into run summaries, and format them.

use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
pub struct VariantLine {
    pub variant: String,
    pub status: Option<String>,
    pub bytes: Option<u64>,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FindingLine {
    pub id: String,
    pub severity: String,
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct RunSummary {
    pub run_id: String,
    pub command: String,
    pub manifest: String,
    pub started_ts: Option<u64>,
    pub finished_ts: Option<u64>,
    pub verdict: Option<String>,
    pub recommended: Option<String>,
    pub measured_bytes: Option<u64>,
    pub budget_bytes: Option<u64>,
    pub variants: Vec<VariantLine>,
    pub findings: Vec<FindingLine>,
    pub interrupted: bool,
}

/// Read a ledger. Unparseable lines are skipped (a crash can leave a torn last
/// line); a missing file is an empty ledger, not an error.
pub fn read(path: &Path) -> Result<Vec<RunSummary>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("cannot read ledger {}: {e}", path.display())),
    };
    let mut order: Vec<String> = Vec::new();
    let mut map: BTreeMap<String, RunSummary> = BTreeMap::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(t) = v.get("t").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let Some(run_id) = v.get("run_id").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if !map.contains_key(run_id) {
            order.push(run_id.to_string());
        }
        let entry = map.entry(run_id.to_string()).or_insert_with(|| RunSummary {
            run_id: run_id.to_string(),
            ..RunSummary::default()
        });
        match t {
            "run.start" => {
                entry.started_ts = v.get("ts").and_then(serde_json::Value::as_u64);
                entry.command = str_of(&v, "command");
                entry.manifest = str_of(&v, "manifest");
            }
            "variant.start" => {
                let name = str_of(&v, "variant");
                if !entry.variants.iter().any(|x| x.variant == name) {
                    entry.variants.push(VariantLine {
                        variant: name,
                        status: None,
                        bytes: None,
                        duration_ms: None,
                    });
                }
            }
            "variant.finish" => {
                let name = str_of(&v, "variant");
                let status = Some(str_of(&v, "status"));
                let bytes = v.get("bytes").and_then(serde_json::Value::as_u64);
                let dur = v.get("duration_ms").and_then(serde_json::Value::as_u64);
                match entry.variants.iter_mut().find(|x| x.variant == name) {
                    Some(slot) => {
                        slot.status = status;
                        slot.bytes = bytes;
                        slot.duration_ms = dur;
                    }
                    None => entry.variants.push(VariantLine {
                        variant: name,
                        status,
                        bytes,
                        duration_ms: dur,
                    }),
                }
            }
            "guard.finding" => entry.findings.push(FindingLine {
                id: str_of(&v, "id"),
                severity: str_of(&v, "severity"),
                evidence: str_of(&v, "evidence"),
            }),
            "run.finish" => {
                entry.finished_ts = v.get("ts").and_then(serde_json::Value::as_u64);
                entry.verdict = Some(str_of(&v, "verdict"));
                entry.recommended = v
                    .get("recommended")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
                entry.measured_bytes = v.get("measured_bytes").and_then(serde_json::Value::as_u64);
                entry.budget_bytes = v.get("budget_bytes").and_then(serde_json::Value::as_u64);
            }
            _ => {}
        }
    }

    Ok(order
        .into_iter()
        .filter_map(|id| map.remove(&id))
        .map(|mut r| {
            r.interrupted = r.finished_ts.is_none();
            r
        })
        .collect())
}

fn str_of(v: &serde_json::Value, key: &str) -> String {
    v.get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// Last `tail` runs, optionally restricted to one run id.
#[must_use]
pub fn select<'a>(
    runs: &'a [RunSummary],
    tail: Option<usize>,
    only: Option<&str>,
) -> Vec<&'a RunSummary> {
    let mut filtered: Vec<&RunSummary> = runs
        .iter()
        .filter(|r| only.is_none_or(|id| r.run_id == id))
        .collect();
    if let Some(n) = tail {
        if filtered.len() > n {
            filtered = filtered.split_off(filtered.len() - n);
        }
    }
    filtered
}

/// Human-readable list. Timestamps are rendered as UTC.
#[must_use]
pub fn render_text(runs: &[&RunSummary]) -> String {
    let mut s = String::new();
    if runs.is_empty() {
        s.push_str("(no runs)\n");
        return s;
    }
    for r in runs {
        let verdict = r.verdict.clone().unwrap_or_else(|| {
            if r.interrupted {
                "interrupted".to_string()
            } else {
                "?".to_string()
            }
        });
        let when = r.started_ts.map(iso8601).unwrap_or_else(|| "-".to_string());
        s.push_str(&format!(
            "{}  {:<6} {:<12} {}\n",
            r.run_id, r.command, verdict, when
        ));
        if let Some(b) = r.measured_bytes {
            match r.budget_bytes {
                Some(budget) => s.push_str(&format!(
                    "    measured {} / budget {}\n",
                    crate::budget::fmt_bytes(b),
                    crate::budget::fmt_bytes(budget)
                )),
                None => s.push_str(&format!(
                    "    recommended {} ({})\n",
                    r.recommended.clone().unwrap_or_else(|| "-".to_string()),
                    crate::budget::fmt_bytes(b)
                )),
            }
        }
        if !r.variants.is_empty() {
            let parts: Vec<String> = r
                .variants
                .iter()
                .map(|v| match v.bytes {
                    Some(b) => format!("{}={}", v.variant, b),
                    None => format!("{}={}", v.variant, v.status.clone().unwrap_or_default()),
                })
                .collect();
            s.push_str(&format!("    variants {}\n", parts.join(" ")));
        }
        for f in &r.findings {
            s.push_str(&format!(
                "    guard {} [{}] {}\n",
                f.id, f.severity, f.evidence
            ));
        }
    }
    s
}

/// `1759700000000` -> `2025-10-05T20:13:20Z` (UTC, no external crates).
#[must_use]
pub fn iso8601(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Howard Hinnant's `civil_from_days`.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_ledger(tag: &str, body: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "rustopt-ledger-{tag}-{}-{}.jsonl",
            std::process::id(),
            crate::fnv::hash_str(tag)
        ));
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn missing_file_is_an_empty_ledger() {
        let p = std::env::temp_dir().join("rustopt-ledger-does-not-exist.jsonl");
        let _ = std::fs::remove_file(&p);
        assert!(read(&p).unwrap().is_empty());
    }

    #[test]
    fn projection_groups_events_and_detects_interruption() {
        let body = concat!(
            r#"{"t":"run.start","run_id":"run-a","version":"0.1.0","ts":1000,"command":"plan","cwd":"/x","manifest":"/x","variants":["current","tuned"]}"#,
            "\n",
            r#"{"t":"variant.start","run_id":"run-a","variant":"current","ts":1001,"command":"cargo build"}"#,
            "\n",
            r#"{"t":"variant.finish","run_id":"run-a","variant":"current","ts":1002,"status":"ok","bytes":100,"duration_ms":1,"stderr_hash":"h","stderr_len":1}"#,
            "\n",
            "this line is torn and must be skipped",
            "\n",
            r#"{"t":"run.start","run_id":"run-b","version":"0.1.0","ts":2000,"command":"check","cwd":"/x","manifest":"/x","variants":["current"]}"#,
            "\n",
            r#"{"t":"run.finish","run_id":"run-a","ts":1003,"verdict":"ok","measured_bytes":90,"budget_bytes":null,"recommended":"tuned"}"#,
            "\n",
        );
        let p = write_ledger("proj", body);
        let runs = read(&p).unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].run_id, "run-a");
        assert_eq!(runs[0].verdict.as_deref(), Some("ok"));
        assert!(!runs[0].interrupted);
        assert_eq!(runs[0].recommended.as_deref(), Some("tuned"));
        assert_eq!(runs[0].variants.len(), 1);
        assert_eq!(runs[0].variants[0].bytes, Some(100));
        assert_eq!(runs[1].run_id, "run-b");
        assert!(
            runs[1].interrupted,
            "dangling run.start must be interrupted"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn select_takes_the_tail() {
        let body = concat!(
            r#"{"t":"run.start","run_id":"a","ts":1,"command":"plan","cwd":"","manifest":"","variants":[]}"#,
            "\n",
            r#"{"t":"run.start","run_id":"b","ts":2,"command":"plan","cwd":"","manifest":"","variants":[]}"#,
            "\n",
            r#"{"t":"run.start","run_id":"c","ts":3,"command":"plan","cwd":"","manifest":"","variants":[]}"#,
            "\n",
        );
        let p = write_ledger("tail", body);
        let runs = read(&p).unwrap();
        let last2 = select(&runs, Some(2), None);
        assert_eq!(last2.len(), 2);
        assert_eq!(last2[0].run_id, "b");
        let one = select(&runs, None, Some("c"));
        assert_eq!(one.len(), 1);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn iso8601_is_correct_utc() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(1_767_225_600_000), "2026-01-01T00:00:00Z");
        assert_eq!(iso8601(1_000_000_000_000), "2001-09-09T01:46:40Z");
    }
}
