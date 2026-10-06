//! rustopt — size-aware release-profile ablation and binary-size budget gates.
//!
//! `plan` measures a matrix of `profile.release` variants by actually building
//! them (never by estimating), applies semantic guards to decide what may be
//! recommended, and prints the smallest measured option. `check` measures what
//! the package ships today and fails with exit 1 when it is over budget.
//!
//! Exit codes: `0` ok/pass, `1` budget exceeded, `2` tool error. The G0 rule is
//! enforced here: an internal panic becomes exit 2, never a false "pass".

mod budget;
mod events;
mod fnv;
mod guard;
mod ledger;
mod measure;
mod plan;
mod report;
mod variants;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_LEDGER: &str = ".rustopt/runs.jsonl";

const USAGE_BASE: &str = "\
rustopt - measure release-profile variants, recommend a smaller artifact, gate on a budget

USAGE
  rustopt plan   [--manifest DIR] [--variants a,b,c] [--work-dir DIR] [--ephemeral]
                 [--dry-run] [--offline] [--jobs N] [--emit json|pretty]
                 [--log PATH | --no-log]
  rustopt check  [--manifest DIR] --budget SIZE [--work-dir DIR] [--ephemeral]
                 [--offline] [--jobs N] [--emit json|pretty] [--log PATH | --no-log]
  rustopt ledger [--tail N] [--run ID] [--log PATH] [--emit json|pretty]
  rustopt clean  [--repo DIR] [--work-dir DIR] [--apply] [--emit json|pretty]

SIZE
  1500000 | 1.5MB | 2MiB | 900KB      (KB = 1000 bytes, KiB = 1024 bytes)

EXIT
  0  ok / within budget
  1  budget exceeded
  2  tool error (usage, I/O, no bin target, build that could not be measured)

NOTES
  plan measures every selected variant with a real `cargo build --release` in an
  isolated CARGO_TARGET_DIR; it never edits the package. The first run is
  therefore slow and later runs reuse the per-variant work dirs.
  Variants marked (default) are measured by `plan` unless --variants is given.
  The only file written into the working directory is the ledger (--no-log
  disables it; --log PATH moves it).";

const VALUE_OPTS: &[&str] = &[
    "--manifest",
    "--variants",
    "--work-dir",
    "--emit",
    "--log",
    "--jobs",
    "--budget",
    "--tail",
    "--run",
    "--repo",
];
const FLAG_OPTS: &[&str] = &[
    "--dry-run",
    "--ephemeral",
    "--offline",
    "--no-log",
    "--apply",
    "--help",
    "-h",
    "--version",
    "-V",
];

struct Args {
    values: BTreeMap<String, String>,
    flags: Vec<String>,
}

impl Args {
    fn parse(argv: &[String]) -> Result<Args, String> {
        let mut values = BTreeMap::new();
        let mut flags = Vec::new();
        let mut i = 0;
        while i < argv.len() {
            let a = argv[i].clone();
            if a.starts_with("--") {
                if let Some(eq) = a.find('=') {
                    let key = &a[..eq];
                    if VALUE_OPTS.contains(&key) {
                        values.insert(key.to_string(), a[eq + 1..].to_string());
                        i += 1;
                        continue;
                    }
                }
            }
            if VALUE_OPTS.contains(&a.as_str()) {
                let v = argv
                    .get(i + 1)
                    .ok_or_else(|| format!("{a} needs a value"))?
                    .clone();
                values.insert(a, v);
                i += 2;
                continue;
            }
            if FLAG_OPTS.contains(&a.as_str()) {
                flags.push(a);
                i += 1;
                continue;
            }
            return Err(format!("unexpected argument {a:?} (see --help)"));
        }
        Ok(Args { values, flags })
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    fn has(&self, flag: &str) -> bool {
        self.flags.iter().any(|f| f == flag)
    }

    fn emit(&self) -> Result<&str, String> {
        let e = self.get("--emit").unwrap_or("pretty");
        if e == "json" || e == "pretty" {
            Ok(e)
        } else {
            Err(format!("--emit must be json or pretty, got {e:?}"))
        }
    }
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match std::panic::catch_unwind(|| dispatch(argv)) {
        Ok(Ok(code)) => code,
        Ok(Err(e)) => {
            eprintln!("rustopt: {e}");
            ExitCode::from(2)
        }
        Err(_) => {
            eprintln!("rustopt: tool error: internal panic (exit 2 by contract)");
            ExitCode::from(2)
        }
    }
}

fn dispatch(argv: Vec<String>) -> Result<ExitCode, String> {
    let Some(cmd) = argv.first().cloned() else {
        eprintln!("{}", usage());
        return Ok(ExitCode::from(2));
    };
    if cmd == "-h" || cmd == "--help" || cmd == "help" {
        println!("{}", usage());
        return Ok(ExitCode::SUCCESS);
    }
    if cmd == "-V" || cmd == "--version" {
        println!("rustopt {VERSION}");
        return Ok(ExitCode::SUCCESS);
    }
    let args = Args::parse(&argv[1..])?;
    if args.has("--help") || args.has("-h") {
        println!("{}", usage());
        return Ok(ExitCode::SUCCESS);
    }
    if args.has("--version") || args.has("-V") {
        println!("rustopt {VERSION}");
        return Ok(ExitCode::SUCCESS);
    }
    match cmd.as_str() {
        "plan" => cmd_plan(&args),
        "check" => cmd_check(&args),
        "ledger" => cmd_ledger(&args),
        "clean" => cmd_clean(&args),
        other => Err(format!("unknown command {other:?} (see --help)")),
    }
}

fn usage() -> String {
    let mut s = String::from(USAGE_BASE);
    s.push_str("\n\nVARIANTS\n");
    for v in variants::all() {
        s.push_str(&format!(
            "  {:<8} {:<10} {}\n",
            v.name,
            if v.default_set { "(default)" } else { "" },
            v.note
        ));
    }
    s
}

fn cache_root() -> PathBuf {
    for (var, sub) in [
        ("XDG_CACHE_HOME", ""),
        ("HOME", ".cache"),
        ("LOCALAPPDATA", ""),
    ] {
        if let Ok(v) = std::env::var(var) {
            if !v.is_empty() {
                let base = PathBuf::from(v);
                return if sub.is_empty() {
                    base.join("rustopt")
                } else {
                    base.join(sub).join("rustopt")
                };
            }
        }
    }
    std::env::temp_dir().join("rustopt")
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Per-package work root: stable across runs so variant builds are reused.
fn work_root_for(manifest: &Path) -> Result<PathBuf, String> {
    let canonical = std::fs::canonicalize(manifest)
        .map_err(|e| format!("cannot resolve manifest dir {}: {e}", manifest.display()))?;
    let leaf = canonical
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("package");
    let tag = &fnv::hash_str(&canonical.display().to_string())[..8];
    Ok(cache_root()
        .join("work")
        .join(format!("{}-{tag}", sanitize(leaf))))
}

fn log_path(args: &Args) -> Option<PathBuf> {
    if args.has("--no-log") {
        return None;
    }
    Some(
        args.get("--log")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_LEDGER)),
    )
}

fn build_opts(args: &Args) -> Result<measure::BuildOpts, String> {
    let jobs = match args.get("--jobs") {
        Some(s) => Some(
            s.parse::<u32>()
                .map_err(|_| format!("--jobs expects a number, got {s:?}"))?,
        ),
        None => None,
    };
    Ok(measure::BuildOpts {
        dry_run: args.has("--dry-run"),
        offline: args.has("--offline"),
        jobs,
    })
}

fn emit_json<T: serde::Serialize>(v: &T) -> Result<(), String> {
    let s = serde_json::to_string_pretty(v).map_err(|e| format!("cannot encode report: {e}"))?;
    println!("{s}");
    Ok(())
}

fn cmd_plan(args: &Args) -> Result<ExitCode, String> {
    let manifest = args.get("--manifest").unwrap_or(".").to_string();
    let repo = PathBuf::from(&manifest);
    let selected = match args.get("--variants") {
        Some(s) => variants::parse_list(s)?,
        None => variants::default_set(),
    };
    let work = match args.get("--work-dir") {
        Some(w) => PathBuf::from(w),
        None => work_root_for(&repo)?,
    };
    let opts = plan::PlanOpts {
        variants: selected,
        work_dir: work.clone(),
        build: build_opts(args)?,
        log: log_path(args),
        run_id: events::new_run_id(),
    };
    let p = plan::run(&repo, &opts)?;
    match args.emit()? {
        "json" => emit_json(&p)?,
        _ => print!("{}", report::plan_pretty(&p)),
    }
    if args.has("--ephemeral") {
        let _ = std::fs::remove_dir_all(&work);
    }
    if p.verdict == "error" {
        eprintln!("rustopt: no variant produced a measurement (exit 2)");
        return Ok(ExitCode::from(2));
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_check(args: &Args) -> Result<ExitCode, String> {
    let manifest = args.get("--manifest").unwrap_or(".").to_string();
    let repo = PathBuf::from(&manifest);
    let budget_str = args
        .get("--budget")
        .ok_or("check needs --budget SIZE (e.g. --budget 2.5MB)")?;
    let budget_bytes = budget::parse_size(budget_str)?;
    let work = match args.get("--work-dir") {
        Some(w) => PathBuf::from(w),
        None => work_root_for(&repo)?,
    };
    let opts = plan::PlanOpts {
        variants: vec![variants::by_name("current").ok_or("internal: no current variant")?],
        work_dir: work.clone(),
        build: build_opts(args)?,
        log: log_path(args),
        run_id: events::new_run_id(),
    };
    let r = plan::check(&repo, &opts, budget_bytes)?;
    match args.emit()? {
        "json" => emit_json(&r)?,
        _ => print!("{}", report::check_pretty(&r)),
    }
    if args.has("--ephemeral") {
        let _ = std::fs::remove_dir_all(&work);
    }
    if r.verdict == "fail" {
        return Ok(ExitCode::from(1));
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_ledger(args: &Args) -> Result<ExitCode, String> {
    let path = args
        .get("--log")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_LEDGER));
    let runs = ledger::read(&path)?;
    let tail = match args.get("--tail") {
        Some(s) => Some(
            s.parse::<usize>()
                .map_err(|_| format!("--tail expects a number, got {s:?}"))?,
        ),
        None => None,
    };
    let selected = ledger::select(&runs, tail, args.get("--run"));
    match args.emit()? {
        "json" => emit_json(&selected)?,
        _ => {
            println!(
                "rustopt {VERSION} - ledger {} ({} run(s))",
                path.display(),
                runs.len()
            );
            print!("{}", ledger::render_text(&selected));
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn dir_size(path: &Path) -> Result<u64, String> {
    let mut total = 0u64;
    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(_) => return Ok(0),
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            total = total.saturating_add(dir_size(&p)?);
        } else if let Ok(m) = std::fs::metadata(&p) {
            total = total.saturating_add(m.len());
        }
    }
    Ok(total)
}

fn cmd_clean(args: &Args) -> Result<ExitCode, String> {
    let root = match args.get("--work-dir") {
        Some(w) => PathBuf::from(w),
        None => match args.get("--repo") {
            Some(r) => work_root_for(Path::new(r))?,
            None => cache_root().join("work"),
        },
    };
    let mut entries: Vec<(PathBuf, u64)> = Vec::new();
    if root.is_dir() {
        for e in std::fs::read_dir(&root)
            .map_err(|e| format!("cannot read {}: {e}", root.display()))?
            .flatten()
        {
            let p = e.path();
            if p.is_dir() {
                let n = dir_size(&p)?;
                entries.push((p, n));
            }
        }
    }
    entries.sort();
    let total: u64 = entries.iter().map(|(_, n)| *n).sum();
    let apply = args.has("--apply");
    if args.emit()? == "json" {
        let payload = serde_json::json!({
            "work_root": root.display().to_string(),
            "entries": entries.iter().map(|(p, n)| serde_json::json!({"path": p.display().to_string(), "logical_bytes": n})).collect::<Vec<_>>(),
            "logical_bytes": total,
            "applied": apply,
        });
        emit_json(&payload)?;
    } else if entries.is_empty() {
        println!("nothing to clean under {}", root.display());
    } else {
        for (p, n) in &entries {
            println!("{:<10} {}", budget::fmt_bytes(*n), p.display());
        }
        println!(
            "total {} (logical; hard-linked build artifacts can occupy less on disk)",
            budget::fmt_bytes(total)
        );
    }
    if !apply {
        if !entries.is_empty() {
            eprintln!("rustopt clean: preview only; re-run with --apply to remove");
        }
        return Ok(ExitCode::SUCCESS);
    }
    if entries.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }
    std::fs::remove_dir_all(&root).map_err(|e| format!("cannot remove {}: {e}", root.display()))?;
    if args.emit()? != "json" {
        println!("removed {} ({})", root.display(), budget::fmt_bytes(total));
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| (*x).to_string()).collect()
    }

    #[test]
    fn parser_accepts_both_value_styles() {
        let a = Args::parse(&v(&["--budget", "2.5MB", "--emit=json", "--apply"])).unwrap();
        assert_eq!(a.get("--budget"), Some("2.5MB"));
        assert_eq!(a.get("--emit"), Some("json"));
        assert!(a.has("--apply"));
    }

    #[test]
    fn parser_rejects_unknown_and_missing_values() {
        assert!(Args::parse(&v(&["--nope"])).is_err());
        assert!(Args::parse(&v(&["--budget"])).is_err());
        assert!(Args::parse(&v(&["stray"])).is_err());
    }

    #[test]
    fn emit_is_validated() {
        let a = Args::parse(&v(&["--emit", "xml"])).unwrap();
        assert!(a.emit().is_err());
    }

    #[test]
    fn work_root_is_stable_and_package_tagged() {
        let here = PathBuf::from(".");
        let a = work_root_for(&here).unwrap();
        let b = work_root_for(&here).unwrap();
        assert_eq!(a, b);
        assert!(a.to_string_lossy().contains("work"));
    }

    #[test]
    fn sanitize_keeps_it_filesystem_safe() {
        assert_eq!(sanitize("my crate/v2"), "my-crate-v2");
        assert_eq!(sanitize("ok-name_1"), "ok-name_1");
    }

    #[test]
    fn dir_size_walks_recursively() {
        let d = std::env::temp_dir().join(format!("rustopt-size-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("a/b")).unwrap();
        std::fs::write(d.join("a/x"), vec![0u8; 10]).unwrap();
        std::fs::write(d.join("a/b/y"), vec![0u8; 5]).unwrap();
        assert_eq!(dir_size(&d).unwrap(), 15);
        let _ = std::fs::remove_dir_all(&d);
    }
}
