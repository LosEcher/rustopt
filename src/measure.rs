//! Real builds in an isolated target directory, and measurement of the artifacts
//! cargo reports.
//!
//! Two rules this module exists to enforce:
//!
//! 1. **Measure, never estimate.** Every number in a plan comes from an actual
//!    `cargo build`.
//! 2. **Never write into the package being measured.** The only thing redirected
//!    is `CARGO_TARGET_DIR`; `--locked` is passed whenever a `Cargo.lock` exists
//!    so cargo cannot rewrite the working tree, and raw compiler output is never
//!    stored (only a fingerprint), because compiler logs can carry environment
//!    details that do not belong in a ledger.

use serde::Serialize;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

use crate::fnv;
use crate::variants::{self, Variant};

#[derive(Debug, Clone, Serialize)]
pub struct TargetSize {
    pub target: String,
    pub bytes: u64,
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Outcome {
    pub variant: String,
    /// The cargo argv, verbatim (reproducible evidence).
    pub command: String,
    pub work_dir: String,
    /// `ok` | `failed` | `dry-run`
    pub status: String,
    pub bytes: Option<u64>,
    pub targets: Vec<TargetSize>,
    pub duration_ms: u64,
    /// Fingerprint of compiler stderr. The text itself is deliberately not kept.
    pub stderr_hash: String,
    pub stderr_len: usize,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct BuildOpts {
    pub dry_run: bool,
    pub offline: bool,
    pub jobs: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Toolchain {
    pub rustc: String,
    pub host: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PkgMeta {
    pub name: String,
    pub version: String,
    pub manifest_path: String,
    pub bin_targets: Vec<String>,
}

/// `cargo build --release --message-format=json` argv for one variant.
#[must_use]
pub fn cargo_argv(variant: &Variant, opts: &BuildOpts, locked: bool) -> Vec<String> {
    let mut argv = vec![
        "build".to_string(),
        "--release".to_string(),
        "--message-format=json".to_string(),
    ];
    if locked {
        argv.push("--locked".to_string());
    }
    if opts.offline {
        argv.push("--offline".to_string());
    }
    if let Some(j) = opts.jobs {
        argv.push("-j".to_string());
        argv.push(j.to_string());
    }
    argv.extend(variants::config_args(variant));
    argv
}

fn render_command(argv: &[String]) -> String {
    let mut s = String::from("cargo");
    for a in argv {
        s.push(' ');
        s.push_str(a);
    }
    s
}

/// Run one variant and measure the executables cargo reports.
///
/// `Err` means the measurement could not be taken at all (spawn/IO). A build that
/// ran and failed comes back as `Ok(Outcome { status: "failed", .. })` so a plan
/// can still report the other variants.
pub fn build(
    repo: &Path,
    variant: &Variant,
    work_dir: &Path,
    opts: &BuildOpts,
) -> Result<Outcome, String> {
    let locked = repo.join("Cargo.lock").exists();
    let argv = cargo_argv(variant, opts, locked);
    let command = render_command(&argv);
    let work = work_dir.to_string_lossy().to_string();

    if opts.dry_run {
        return Ok(Outcome {
            variant: variant.name.to_string(),
            command,
            work_dir: work,
            status: "dry-run".to_string(),
            bytes: None,
            targets: Vec::new(),
            duration_ms: 0,
            stderr_hash: String::new(),
            stderr_len: 0,
            error: None,
        });
    }

    std::fs::create_dir_all(work_dir)
        .map_err(|e| format!("cannot create work dir {}: {e}", work_dir.display()))?;

    let started = Instant::now();
    let out = Command::new("cargo")
        .args(&argv)
        .current_dir(repo)
        .env("CARGO_TARGET_DIR", work_dir)
        .env("CARGO_TERM_COLOR", "never")
        .output()
        .map_err(|e| format!("cannot run cargo: {e}"))?;
    let duration_ms = started.elapsed().as_millis() as u64;

    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let stderr_hash = fnv::hash_bytes(&out.stderr);
    let stderr_len = out.stderr.len();

    if !out.status.success() {
        let tail: Vec<&str> = stderr.lines().rev().take(20).collect();
        eprintln!(
            "rustopt: cargo failed for variant {} (exit {:?}); last {} stderr line(s):",
            variant.name,
            out.status.code(),
            tail.len()
        );
        for line in tail.iter().rev() {
            eprintln!("  {line}");
        }
        return Ok(Outcome {
            variant: variant.name.to_string(),
            command,
            work_dir: work,
            status: "failed".to_string(),
            bytes: None,
            targets: Vec::new(),
            duration_ms,
            stderr_hash,
            stderr_len,
            error: Some(format!("cargo build failed (exit {:?})", out.status.code())),
        });
    }

    let collected = collect_bin_artifacts(&out.stdout);
    if collected.is_empty() {
        return Ok(Outcome {
            variant: variant.name.to_string(),
            command,
            work_dir: work,
            status: "failed".to_string(),
            bytes: None,
            targets: Vec::new(),
            duration_ms,
            stderr_hash,
            stderr_len,
            error: Some("build succeeded but no executable artifact was reported".to_string()),
        });
    }

    let mut targets = Vec::new();
    let mut total: u64 = 0;
    for (name, path) in collected {
        let bytes = std::fs::metadata(&path)
            .map_err(|e| format!("cannot stat artifact {path}: {e}"))?
            .len();
        total = total.saturating_add(bytes);
        targets.push(TargetSize {
            target: name,
            bytes,
            path,
        });
    }

    Ok(Outcome {
        variant: variant.name.to_string(),
        command,
        work_dir: work,
        status: "ok".to_string(),
        bytes: Some(total),
        targets,
        duration_ms,
        stderr_hash,
        stderr_len,
        error: None,
    })
}

/// Pull `(target name, executable path)` for every `bin` artifact in cargo's
/// JSON message stream. Last occurrence wins (cargo re-reports fresh artifacts).
fn collect_bin_artifacts(stdout: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(stdout);
    let mut found: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("reason").and_then(serde_json::Value::as_str) != Some("compiler-artifact") {
            continue;
        }
        let Some(exe) = v.get("executable").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let target = v.get("target");
        let is_bin = target
            .and_then(|t| t.get("kind"))
            .and_then(serde_json::Value::as_array)
            .map(|ks| ks.iter().any(|k| k.as_str() == Some("bin")))
            .unwrap_or(false);
        if !is_bin {
            continue;
        }
        let name = target
            .and_then(|t| t.get("name"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?")
            .to_string();
        let exe = exe.to_string();
        match found.iter_mut().find(|(n, _)| *n == name) {
            Some(slot) => slot.1 = exe,
            None => found.push((name, exe)),
        }
    }
    found
}

/// `rustc -vV`, reduced to the two facts a plan should record.
pub fn toolchain() -> Result<Toolchain, String> {
    let out = Command::new("rustc")
        .arg("-vV")
        .output()
        .map_err(|e| format!("cannot run rustc: {e}"))?;
    if !out.status.success() {
        return Err("rustc -vV failed".to_string());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut rustc = None;
    let mut host = None;
    for line in text.lines() {
        if let Some(r) = line.strip_prefix("release: ") {
            rustc = Some(r.trim().to_string());
        }
        if let Some(h) = line.strip_prefix("host: ") {
            host = Some(h.trim().to_string());
        }
    }
    Ok(Toolchain {
        rustc: rustc.unwrap_or_else(|| "unknown".to_string()),
        host: host.unwrap_or_else(|| "unknown".to_string()),
    })
}

/// Package identity + bin targets, read through `cargo metadata` (never by
/// guessing from the directory name: a package may be called one thing and
/// produce a binary called another).
pub fn package_meta(repo: &Path, offline: bool) -> Result<PkgMeta, String> {
    let mut cmd = Command::new("cargo");
    cmd.arg("metadata")
        .arg("--no-deps")
        .arg("--format-version")
        .arg("1");
    if offline {
        cmd.arg("--offline");
    }
    let out = cmd
        .current_dir(repo)
        .env("CARGO_TERM_COLOR", "never")
        .output()
        .map_err(|e| format!("cannot run cargo metadata: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let first = err.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        return Err(format!("cargo metadata failed: {first}"));
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("cannot parse cargo metadata output: {e}"))?;
    let packages = v
        .get("packages")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "cargo metadata output has no packages".to_string())?;

    let want = std::fs::canonicalize(repo.join("Cargo.toml")).ok();
    let mut chosen: Option<&serde_json::Value> = None;
    for p in packages {
        let mp = p
            .get("manifest_path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let matched = match (&want, std::fs::canonicalize(mp).ok()) {
            (Some(a), Some(b)) => *a == b,
            _ => false,
        };
        if matched {
            chosen = Some(p);
            break;
        }
    }
    if chosen.is_none() && packages.len() == 1 {
        chosen = packages.first();
    }
    let p = chosen.ok_or_else(|| {
        format!(
            "workspace with {} packages: point --manifest at the directory holding the \
             package's own Cargo.toml",
            packages.len()
        )
    })?;

    let name = p
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let version = p
        .get("version")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let manifest_path = p
        .get("manifest_path")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();

    let mut bins = Vec::new();
    if let Some(targets) = p.get("targets").and_then(serde_json::Value::as_array) {
        for t in targets {
            let is_bin = t
                .get("kind")
                .and_then(serde_json::Value::as_array)
                .map(|ks| ks.iter().any(|k| k.as_str() == Some("bin")))
                .unwrap_or(false);
            if is_bin {
                if let Some(n) = t.get("name").and_then(serde_json::Value::as_str) {
                    bins.push(n.to_string());
                }
            }
        }
    }

    Ok(PkgMeta {
        name,
        version,
        manifest_path,
        bin_targets: bins,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::variants::by_name;

    fn artifact_msg(name: &str, kind: &str, exe: Option<&str>) -> String {
        let exe = match exe {
            Some(e) => format!("\"{e}\""),
            None => "null".to_string(),
        };
        format!(
            "{{\"reason\":\"compiler-artifact\",\"target\":{{\"name\":\"{name}\",\"kind\":[\"{kind}\"]}},\"executable\":{exe}}}"
        )
    }

    #[test]
    fn collects_only_bins_and_takes_the_last_occurrence() {
        let stdout = format!(
            "{}\n{}\n{}\nnot json\n{}\n",
            artifact_msg("mylib", "lib", None),
            artifact_msg("mytool", "bin", Some("/tmp/a")),
            artifact_msg("mytool", "bin", Some("/tmp/b")),
            artifact_msg("other", "example", Some("/tmp/c")),
        );
        let got = collect_bin_artifacts(stdout.as_bytes());
        assert_eq!(got, vec![("mytool".to_string(), "/tmp/b".to_string())]);
    }

    #[test]
    fn no_bins_yields_empty() {
        let stdout = format!("{}\n", artifact_msg("mylib", "lib", None));
        assert!(collect_bin_artifacts(stdout.as_bytes()).is_empty());
    }

    #[test]
    fn argv_shape_is_stable() {
        let opts = BuildOpts {
            dry_run: false,
            offline: true,
            jobs: Some(4),
        };
        let argv = cargo_argv(by_name("tuned").unwrap(), &opts, true);
        assert_eq!(argv[0], "build");
        assert!(argv.contains(&"--release".to_string()));
        assert!(argv.contains(&"--message-format=json".to_string()));
        assert!(argv.contains(&"--locked".to_string()));
        assert!(argv.contains(&"--offline".to_string()));
        assert!(argv.contains(&"4".to_string()));
        assert!(argv.contains(&"profile.release.strip=\"symbols\"".to_string()));
        let rendered = render_command(&argv);
        assert!(rendered.starts_with("cargo build --release"));
    }

    #[test]
    fn current_variant_adds_no_config() {
        let argv = cargo_argv(by_name("current").unwrap(), &BuildOpts::default(), false);
        assert!(!argv.iter().any(|a| a.starts_with("profile.")));
    }
}
