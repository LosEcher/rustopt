//! Planning: run the variant matrix, apply the guards, and say what is worth
//! doing — with the numbers that were actually measured.

use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::events::{self, Event};
use crate::guard::{self, Finding};
use crate::measure::{self, BuildOpts, Outcome, TargetSize};
use crate::variants::{self, Variant};

#[derive(Debug, Clone, Serialize)]
pub struct VariantResult {
    #[serde(flatten)]
    pub outcome: Outcome,
    pub banned: bool,
    pub ban_reason: Option<String>,
    pub delta_vs_default_pct: Option<f64>,
    pub delta_vs_current_pct: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Rejected {
    pub knob: String,
    pub value: String,
    pub reason: String,
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Recommendation {
    pub variant: String,
    pub bytes: u64,
    /// Signed byte delta against the `default` variant (negative = smaller).
    pub vs_default_bytes: Option<i64>,
    pub vs_default_pct: Option<f64>,
    /// Signed byte delta against what the package ships today.
    pub vs_current_bytes: Option<i64>,
    pub vs_current_pct: Option<f64>,
    /// What to write under `[profile.release]` (TOML literal values, quotes included).
    pub profile: BTreeMap<String, String>,
    /// Always `false` here: `plan` never edits the package.
    pub applied: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub tool: String,
    pub version: String,
    pub run_id: String,
    pub manifest_dir: String,
    pub package: String,
    pub package_version: String,
    pub rustc: String,
    pub host: String,
    pub variants: Vec<VariantResult>,
    pub findings: Vec<Finding>,
    pub rejected: Vec<Rejected>,
    pub recommendation: Option<Recommendation>,
    pub notes: Vec<String>,
    /// `ok` | `partial` | `error`
    pub verdict: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckReport {
    pub tool: String,
    pub version: String,
    pub run_id: String,
    pub manifest_dir: String,
    pub package: String,
    pub package_version: String,
    pub rustc: String,
    pub host: String,
    pub variant: String,
    pub command: String,
    pub stderr_hash: String,
    pub budget_bytes: u64,
    pub measured_bytes: u64,
    pub headroom_bytes: i64,
    pub targets: Vec<TargetSize>,
    /// `pass` | `fail`
    pub verdict: String,
    pub notes: Vec<String>,
}

pub struct PlanOpts {
    pub variants: Vec<&'static Variant>,
    pub work_dir: PathBuf,
    pub build: BuildOpts,
    pub log: Option<PathBuf>,
    pub run_id: String,
}

pub const TOOL: &str = "rustopt";

fn log_to(log: &Option<PathBuf>, ev: &Event) -> Result<(), String> {
    match log {
        Some(p) => events::append(p, ev),
        None => Ok(()),
    }
}

/// Measure the whole variant matrix and turn it into a plan.
pub fn run(repo: &Path, opts: &PlanOpts) -> Result<Plan, String> {
    let canonical = std::fs::canonicalize(repo)
        .map_err(|e| format!("cannot resolve manifest dir {}: {e}", repo.display()))?;
    let meta = measure::package_meta(&canonical, opts.build.offline)?;
    if meta.bin_targets.is_empty() {
        return Err(format!(
            "package {:?} declares no bin target; rustopt measures executables only",
            meta.name
        ));
    }
    let tc = measure::toolchain()?;
    let findings = guard::scan(&canonical)?;
    let banned = guard::banned_knobs(&findings);

    log_to(
        &opts.log,
        &Event::RunStart {
            run_id: opts.run_id.clone(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            ts: events::now_ms(),
            command: "plan".to_string(),
            cwd: std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            manifest: canonical.display().to_string(),
            variants: opts.variants.iter().map(|v| v.name.to_string()).collect(),
        },
    )?;
    for f in &findings {
        log_to(
            &opts.log,
            &Event::GuardFinding {
                run_id: opts.run_id.clone(),
                id: f.id.clone(),
                severity: f.severity.clone(),
                evidence: f.evidence.clone(),
            },
        )?;
    }

    let mut measured: Vec<(&'static Variant, Outcome)> = Vec::new();
    for v in &opts.variants {
        let vdir = opts.work_dir.join(v.name);
        log_to(
            &opts.log,
            &Event::VariantStart {
                run_id: opts.run_id.clone(),
                variant: v.name.to_string(),
                ts: events::now_ms(),
                command: measure::cargo_argv(v, &opts.build, canonical.join("Cargo.lock").exists())
                    .join(" "),
            },
        )?;
        let outcome = measure::build(&canonical, v, &vdir, &opts.build)?;
        log_to(
            &opts.log,
            &Event::VariantFinish {
                run_id: opts.run_id.clone(),
                variant: v.name.to_string(),
                ts: events::now_ms(),
                status: outcome.status.clone(),
                bytes: outcome.bytes,
                duration_ms: outcome.duration_ms,
                stderr_hash: outcome.stderr_hash.clone(),
                stderr_len: outcome.stderr_len,
            },
        )?;
        measured.push((v, outcome));
    }

    let bytes_of = |name: &str| -> Option<u64> {
        measured
            .iter()
            .find(|(v, _)| v.name == name)
            .and_then(|(_, o)| o.bytes)
    };
    let default_bytes = bytes_of("default");
    let current_bytes = bytes_of("current");

    // Which variants are off the table because of a guard?
    let ban_for = |v: &Variant| -> Option<String> {
        for (key, val) in variants::composed_knobs(v) {
            for (bk, bv, reason, _) in &banned {
                if key == bk && val == bv {
                    return Some(reason.clone());
                }
            }
        }
        None
    };

    let mut results: Vec<VariantResult> = Vec::new();
    for (v, o) in &measured {
        let ban = ban_for(v);
        results.push(VariantResult {
            outcome: (*o).clone(),
            banned: ban.is_some(),
            ban_reason: ban,
            delta_vs_default_pct: match (default_bytes, o.bytes) {
                // A variant compared with itself is not a delta.
                (Some(d), Some(b)) if v.name != "default" => crate::budget::pct_delta(d, b),
                _ => None,
            },
            delta_vs_current_pct: match (current_bytes, o.bytes) {
                (Some(c), Some(b)) if v.name != "current" => crate::budget::pct_delta(c, b),
                _ => None,
            },
        });
    }

    let mut rejected: Vec<Rejected> = Vec::new();
    for (k, v, reason, evidence) in &banned {
        rejected.push(Rejected {
            knob: k.clone(),
            value: v.clone(),
            reason: reason.clone(),
            evidence: evidence.clone(),
        });
    }

    // Recommendation: smallest measured variant that no guard forbids.
    let mut best: Option<(&'static Variant, u64)> = None;
    for (v, o) in &measured {
        let Some(b) = o.bytes else { continue };
        if o.status != "ok" || ban_for(v).is_some() {
            continue;
        }
        if best.is_none_or(|(_, bb)| b < bb) {
            best = Some((v, b));
        }
    }
    let recommendation = best.map(|(v, b)| Recommendation {
        variant: v.name.to_string(),
        bytes: b,
        vs_default_bytes: default_bytes.map(|d| b as i64 - d as i64),
        vs_default_pct: default_bytes.and_then(|d| crate::budget::pct_delta(d, b)),
        vs_current_bytes: current_bytes.map(|c| b as i64 - c as i64),
        vs_current_pct: current_bytes.and_then(|c| crate::budget::pct_delta(c, b)),
        profile: variants::manifest_knobs(v)
            .iter()
            .map(|(k, val)| ((*k).to_string(), (*val).to_string()))
            .collect(),
        applied: false,
    });

    let mut notes = Vec::new();
    if let (Some(d), Some(z)) = (default_bytes, bytes_of("z")) {
        if z > d {
            notes.push(format!(
                "trap: opt-level=\"z\" on its own is {} bytes LARGER than cargo's default \
                 ({:+.1}%). Single-knob comparisons mislead; only a measured combination counts.",
                z - d,
                crate::budget::pct_delta(d, z).unwrap_or(0.0)
            ));
        }
    }
    if let (Some(d), Some(l)) = (default_bytes, bytes_of("lto")) {
        if l < d {
            notes.push(format!(
                "lto=\"fat\" + codegen-units=1 alone: {} -> {} ({:+.1}%)",
                crate::budget::fmt_bytes(d),
                crate::budget::fmt_bytes(l),
                crate::budget::pct_delta(d, l).unwrap_or(0.0)
            ));
        }
    }
    if let (Some(c), Some(r)) = (current_bytes, recommendation.as_ref()) {
        if r.variant == "current" {
            notes.push(
                "the package's current configuration is already the smallest measured variant"
                    .to_string(),
            );
        } else {
            notes.push(format!(
                "current configuration {} -> recommended {} ({:+.1}%)",
                crate::budget::fmt_bytes(c),
                crate::budget::fmt_bytes(r.bytes),
                crate::budget::pct_delta(c, r.bytes).unwrap_or(0.0)
            ));
        }
    }
    for f in &findings {
        if f.severity == guard::SEV_WARN {
            notes.push(format!("guard {}: {} ({})", f.id, f.message, f.evidence));
        }
    }
    if let Some(r) = recommendation.as_ref() {
        if r.profile.contains_key("strip") && findings.iter().any(|f| f.id == "backtrace-use") {
            notes.push(
                "recommendation includes strip=\"symbols\" while backtraces are referenced: \
                 the artifact shrinks but symbolization gets worse"
                    .to_string(),
            );
        }
    }

    let ok_count = measured.iter().filter(|(_, o)| o.status == "ok").count();
    let failed = measured
        .iter()
        .filter(|(_, o)| o.status == "failed")
        .count();
    let dry = !measured.is_empty() && measured.iter().all(|(_, o)| o.status == "dry-run");
    let verdict = if dry {
        "dry-run"
    } else if ok_count == 0 {
        "error"
    } else if failed > 0 {
        "partial"
    } else {
        "ok"
    };

    let plan = Plan {
        tool: TOOL.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        run_id: opts.run_id.clone(),
        manifest_dir: canonical.display().to_string(),
        package: meta.name,
        package_version: meta.version,
        rustc: tc.rustc,
        host: tc.host,
        variants: results,
        findings,
        rejected,
        recommendation: recommendation.clone(),
        notes,
        verdict: verdict.to_string(),
    };

    log_to(
        &opts.log,
        &Event::RunFinish {
            run_id: opts.run_id.clone(),
            ts: events::now_ms(),
            verdict: verdict.to_string(),
            measured_bytes: recommendation.as_ref().map(|r| r.bytes),
            budget_bytes: None,
            recommended: recommendation.as_ref().map(|r| r.variant.clone()),
        },
    )?;

    Ok(plan)
}

/// Measure what the package ships **today** and compare it with a budget.
pub fn check(repo: &Path, opts: &PlanOpts, budget_bytes: u64) -> Result<CheckReport, String> {
    let canonical = std::fs::canonicalize(repo)
        .map_err(|e| format!("cannot resolve manifest dir {}: {e}", repo.display()))?;
    let meta = measure::package_meta(&canonical, opts.build.offline)?;
    if meta.bin_targets.is_empty() {
        return Err(format!(
            "package {:?} declares no bin target; rustopt measures executables only",
            meta.name
        ));
    }
    let tc = measure::toolchain()?;
    let current = variants::by_name("current").ok_or("internal: no `current` variant")?;

    log_to(
        &opts.log,
        &Event::RunStart {
            run_id: opts.run_id.clone(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            ts: events::now_ms(),
            command: "check".to_string(),
            cwd: std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            manifest: canonical.display().to_string(),
            variants: vec!["current".to_string()],
        },
    )?;

    let outcome = measure::build(
        &canonical,
        current,
        &opts.work_dir.join(current.name),
        &opts.build,
    )?;
    log_to(
        &opts.log,
        &Event::VariantFinish {
            run_id: opts.run_id.clone(),
            variant: current.name.to_string(),
            ts: events::now_ms(),
            status: outcome.status.clone(),
            bytes: outcome.bytes,
            duration_ms: outcome.duration_ms,
            stderr_hash: outcome.stderr_hash.clone(),
            stderr_len: outcome.stderr_len,
        },
    )?;

    if outcome.status != "ok" {
        return Err(format!(
            "could not measure the current configuration: {}",
            outcome.error.unwrap_or_else(|| "build failed".to_string())
        ));
    }
    let measured_bytes = outcome.bytes.unwrap_or(0);
    let headroom = budget_bytes as i64 - measured_bytes as i64;
    let verdict = if measured_bytes <= budget_bytes {
        "pass"
    } else {
        "fail"
    };

    let mut notes = Vec::new();
    if verdict == "fail" {
        notes.push(
            "run `rustopt plan` to see which measured variant would fit and what it costs"
                .to_string(),
        );
    }

    let report = CheckReport {
        tool: TOOL.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        run_id: opts.run_id.clone(),
        manifest_dir: canonical.display().to_string(),
        package: meta.name,
        package_version: meta.version,
        rustc: tc.rustc,
        host: tc.host,
        variant: current.name.to_string(),
        command: outcome.command.clone(),
        stderr_hash: outcome.stderr_hash.clone(),
        budget_bytes,
        measured_bytes,
        headroom_bytes: headroom,
        targets: outcome.targets.clone(),
        verdict: verdict.to_string(),
        notes,
    };

    log_to(
        &opts.log,
        &Event::RunFinish {
            run_id: opts.run_id.clone(),
            ts: events::now_ms(),
            verdict: verdict.to_string(),
            measured_bytes: Some(measured_bytes),
            budget_bytes: Some(budget_bytes),
            recommended: None,
        },
    )?;

    Ok(report)
}
