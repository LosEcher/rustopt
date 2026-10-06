//! Semantic guards: what a size recommendation is *allowed* to touch.
//!
//! This is the part that separates a planner from a size reporter. A reporter
//! says "`panic = "abort"` saves 15%". A planner has to know that on a crate whose
//! exit-code contract is implemented with `std::panic::catch_unwind`, abort turns
//! a recoverable panic into SIGABRT and breaks that contract silently.
//!
//! Guards only read text; they never modify the package.

use serde::Serialize;
use std::path::{Path, PathBuf};

pub const SEV_ERROR: &str = "error";
pub const SEV_BAN: &str = "ban";
pub const SEV_WARN: &str = "warn";

/// Directories never worth scanning.
const SKIP_DIRS: &[&str] = &["target", ".git", "node_modules", ".rustopt", ".cargo"];
/// Source files larger than this are not scanned (generated/vendored blobs).
const MAX_SCAN_BYTES: u64 = 2 * 1024 * 1024;

/// Needles for the strongest claim this tool makes, so they must be precise. A
/// bare `catch_unwind` substring also matches identifiers such as
/// `catch_unwind_like()`, and a `ban` justified by an identifier name would be
/// worse than no ban at all. Match a path segment or a call site instead.
const CATCH_UNWIND_NEEDLES: &[&str] = &["::catch_unwind", "catch_unwind("];
const SHOULD_PANIC_NEEDLES: &[&str] = &["#[should_panic"];
const BACKTRACE_NEEDLES: &[&str] = &["::Backtrace", "backtrace::"];

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub id: String,
    /// `error` (pre-existing defect) | `ban` (knob must not be recommended) | `warn`
    pub severity: String,
    /// `path:line` (or `Cargo.toml`), relative to the package.
    pub evidence: String,
    pub message: String,
    pub occurrences: usize,
}

/// Scan the package for the conditions that constrain a size plan.
pub fn scan(repo: &Path) -> Result<Vec<Finding>, String> {
    let mut files: Vec<PathBuf> = Vec::new();
    walk(repo, &mut files)?;
    files.sort();

    let mut rs_files: Vec<PathBuf> = Vec::new();
    let mut manifest: Option<String> = None;
    for f in &files {
        if f.extension().and_then(|e| e.to_str()) == Some("rs") {
            rs_files.push(f.clone());
        }
        if f.file_name().and_then(|n| n.to_str()) == Some("Cargo.toml") && f.parent() == Some(repo)
        {
            manifest = std::fs::read_to_string(f).ok();
        }
    }

    let mut out = Vec::new();

    let catch = first_hit(repo, &rs_files, CATCH_UNWIND_NEEDLES);
    let cdylib = manifest
        .as_deref()
        .and_then(manifest_crate_type)
        .filter(|t| t == "cdylib" || t == "staticlib");
    let declares_abort = manifest
        .as_deref()
        .map(declares_panic_abort)
        .unwrap_or(false);

    if let Some((ev, n)) = &catch {
        // A package that already points `panic = "abort"` at its own catch_unwind
        // has a real defect: escalate from "do not recommend" to "this is broken".
        let (sev, id, msg) = if declares_abort {
            (
                SEV_ERROR,
                "abort-conflict",
                "Cargo.toml sets panic = \"abort\" while the source calls \
                 std::panic::catch_unwind: the guard can never run, and a panic \
                 aborts the process (SIGABRT / 134) instead of unwinding",
            )
        } else {
            (
                SEV_BAN,
                "catch-unwind",
                "std::panic::catch_unwind is used, so panic = \"abort\" must not be \
                 recommended: it changes crash behaviour from unwinding (exit 101) to \
                 SIGABRT (exit 134) and disables the guard",
            )
        };
        out.push(Finding {
            id: id.to_string(),
            severity: sev.to_string(),
            evidence: ev.clone(),
            message: msg.to_string(),
            occurrences: *n,
        });
    }

    if let Some(t) = &cdylib {
        out.push(Finding {
            id: "cdylib".to_string(),
            severity: SEV_BAN.to_string(),
            evidence: "Cargo.toml".to_string(),
            message: format!(
                "crate-type includes {t:?}: panics must unwind across the FFI boundary, \
                 so panic = \"abort\" must not be recommended"
            ),
            occurrences: 1,
        });
    }

    if let Some((ev, n)) = first_hit(repo, &rs_files, SHOULD_PANIC_NEEDLES) {
        out.push(Finding {
            id: "should-panic".to_string(),
            severity: SEV_WARN.to_string(),
            evidence: ev,
            message: "`#[should_panic]` tests exist; panic strategy is observable in tests"
                .to_string(),
            occurrences: n,
        });
    }

    if let Some((ev, n)) = first_hit(repo, &rs_files, BACKTRACE_NEEDLES) {
        out.push(Finding {
            id: "backtrace-use".to_string(),
            severity: SEV_WARN.to_string(),
            evidence: ev,
            message: "backtraces are used or referenced; strip = \"symbols\" will degrade \
                      symbolization even though it shrinks the artifact"
                .to_string(),
            occurrences: n,
        });
    }

    if !repo.join("Cargo.lock").exists() {
        out.push(Finding {
            id: "no-cargo-lock".to_string(),
            severity: SEV_WARN.to_string(),
            evidence: "Cargo.lock".to_string(),
            message: "no Cargo.lock: cargo may write one into the working tree during a \
                      build (rustopt cannot pass --locked)"
                .to_string(),
            occurrences: 1,
        });
    }

    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// Knobs a guard forbids: `(key, value)` -> reason + evidence.
#[must_use]
pub fn banned_knobs(findings: &[Finding]) -> Vec<(String, String, String, String)> {
    let mut out = Vec::new();
    for f in findings {
        if f.severity != SEV_BAN && f.severity != SEV_ERROR {
            continue;
        }
        if f.id == "catch-unwind" || f.id == "cdylib" || f.id == "abort-conflict" {
            let entry = (
                "panic".to_string(),
                "\"abort\"".to_string(),
                f.message.clone(),
                f.evidence.clone(),
            );
            if !out
                .iter()
                .any(|e: &(String, String, String, String)| e.1 == entry.1)
            {
                out.push(entry);
            }
        }
    }
    out
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    let mut names: Vec<PathBuf> = Vec::new();
    for e in entries {
        let e = e.map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
        names.push(e.path());
    }
    names.sort();
    for p in names {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if p.is_dir() {
            if SKIP_DIRS.contains(&name) {
                continue;
            }
            walk(&p, out)?;
        } else if p.is_file() {
            out.push(p);
        }
    }
    Ok(())
}

/// First `path:line` match for any marker, plus the total occurrence count.
///
/// Matching happens on **code only**: comments, string literals, char literals and
/// raw strings are blanked out first. Without that, a doc comment that merely
/// *mentions* `catch_unwind` (including this module's own documentation) is
/// reported as evidence — and evidence pointing at a comment is worse than none.
fn first_hit(repo: &Path, files: &[PathBuf], markers: &[&str]) -> Option<(String, usize)> {
    let mut first: Option<(String, usize)> = None;
    let mut total = 0usize;
    for f in files {
        let Ok(meta) = std::fs::metadata(f) else {
            continue;
        };
        if meta.len() > MAX_SCAN_BYTES {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(f) else {
            continue;
        };
        let code = strip_noncode(&text);
        for (i, line) in code.lines().enumerate() {
            if markers.iter().any(|m| line.contains(m)) {
                total += 1;
                if first.is_none() {
                    let rel = f.strip_prefix(repo).unwrap_or(f);
                    first = Some((format!("{}:{}", rel.display(), i + 1), 0));
                }
            }
        }
    }
    first.map(|(ev, _)| (ev, total))
}

/// Blank out comments and literals while preserving line structure, so line
/// numbers stay valid. Deliberately not a full lexer: it only has to be right
/// about the places a marker string can hide.
fn strip_noncode(src: &str) -> String {
    let b: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0usize;
    while i < b.len() {
        let c = b[i];
        // line comment
        if c == '/' && i + 1 < b.len() && b[i + 1] == '/' {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
            continue;
        }
        // block comment (Rust nests them)
        if c == '/' && i + 1 < b.len() && b[i + 1] == '*' {
            let mut depth = 1usize;
            i += 2;
            while i < b.len() && depth > 0 {
                if b[i] == '\n' {
                    out.push('\n');
                }
                if b[i] == '/' && i + 1 < b.len() && b[i + 1] == '*' {
                    depth += 1;
                    i += 2;
                    continue;
                }
                if b[i] == '*' && i + 1 < b.len() && b[i + 1] == '/' {
                    depth -= 1;
                    i += 2;
                    continue;
                }
                i += 1;
            }
            continue;
        }
        // raw string: r"..." / r#"..."# / br#"..."#
        if c == 'r' || (c == 'b' && i + 1 < b.len() && b[i + 1] == 'r') {
            let start = if c == 'b' { i + 1 } else { i };
            if start + 1 < b.len() && (b[start + 1] == '#' || b[start + 1] == '"') {
                let mut j = start + 1;
                let mut hashes = 0usize;
                while j < b.len() && b[j] == '#' {
                    hashes += 1;
                    j += 1;
                }
                if j < b.len() && b[j] == '"' {
                    j += 1;
                    loop {
                        if j >= b.len() {
                            break;
                        }
                        if b[j] == '\n' {
                            out.push('\n');
                        }
                        if b[j] == '"' {
                            let mut k = j + 1;
                            let mut seen = 0usize;
                            while k < b.len() && b[k] == '#' && seen < hashes {
                                seen += 1;
                                k += 1;
                            }
                            if seen == hashes {
                                j = k;
                                break;
                            }
                        }
                        j += 1;
                    }
                    i = j;
                    continue;
                }
            }
        }
        // string literal
        if c == '"' {
            i += 1;
            while i < b.len() {
                if b[i] == '\\' {
                    i += 2;
                    continue;
                }
                if b[i] == '"' {
                    i += 1;
                    break;
                }
                if b[i] == '\n' {
                    out.push('\n');
                }
                i += 1;
            }
            continue;
        }
        // char literal (not a lifetime: 'a' vs 'a)
        if c == '\'' {
            let is_char = if i + 1 < b.len() && b[i + 1] == '\\' {
                true
            } else {
                i + 2 < b.len() && b[i + 2] == '\''
            };
            if is_char {
                i += 1;
                while i < b.len() {
                    if b[i] == '\\' {
                        i += 2;
                        continue;
                    }
                    if b[i] == '\'' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

fn manifest_crate_type(manifest: &str) -> Option<String> {
    for line in manifest.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("crate-type") {
            if rest.contains("cdylib") {
                return Some("cdylib".to_string());
            }
            if rest.contains("staticlib") {
                return Some("staticlib".to_string());
            }
        }
    }
    None
}

fn declares_panic_abort(manifest: &str) -> bool {
    manifest.lines().any(|l| {
        let t: String = l.chars().filter(|c| !c.is_whitespace()).collect();
        t == "panic=\"abort\"" || t == "panic='abort'"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "rustopt-guard-{tag}-{}-{}",
            std::process::id(),
            crate::fnv::hash_str(tag)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(p.join("src")).unwrap();
        p
    }

    #[test]
    fn catch_unwind_bans_abort() {
        let d = tmpdir("cu");
        std::fs::write(d.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        std::fs::write(
            d.join("src/main.rs"),
            "fn main() {\n    let _ = std::panic::catch_unwind(|| 1);\n}\n",
        )
        .unwrap();
        let f = scan(&d).unwrap();
        let cu = f.iter().find(|f| f.id == "catch-unwind").expect("finding");
        assert_eq!(cu.severity, SEV_BAN);
        assert!(cu.evidence.ends_with("src/main.rs:2"), "{}", cu.evidence);
        let banned = banned_knobs(&f);
        assert_eq!(banned.len(), 1);
        assert_eq!(banned[0].0, "panic");
        assert_eq!(banned[0].1, "\"abort\"");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn preexisting_abort_plus_catch_unwind_is_an_error() {
        let d = tmpdir("conflict");
        std::fs::write(
            d.join("Cargo.toml"),
            "[package]\nname=\"x\"\n\n[profile.release]\npanic = \"abort\"\n",
        )
        .unwrap();
        std::fs::write(
            d.join("src/main.rs"),
            "fn main() { let _ = std::panic::catch_unwind(|| 1); }\n",
        )
        .unwrap();
        let f = scan(&d).unwrap();
        assert!(f
            .iter()
            .any(|f| f.id == "abort-conflict" && f.severity == SEV_ERROR));
        assert!(!f.iter().any(|f| f.id == "catch-unwind"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn clean_package_has_no_ban() {
        let d = tmpdir("clean");
        std::fs::write(d.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        std::fs::write(d.join("Cargo.lock"), "version = 3\n").unwrap();
        std::fs::write(d.join("src/main.rs"), "fn main() {}\n").unwrap();
        let f = scan(&d).unwrap();
        assert!(banned_knobs(&f).is_empty());
        // no Cargo.lock warning must be absent because the lock exists
        assert!(!f.iter().any(|f| f.id == "no-cargo-lock"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn cdylib_bans_abort() {
        let d = tmpdir("cdylib");
        std::fs::write(d.join("Cargo.toml"), "[lib]\ncrate-type = [\"cdylib\"]\n").unwrap();
        std::fs::write(d.join("src/main.rs"), "fn main() {}\n").unwrap();
        let f = scan(&d).unwrap();
        let c = f.iter().find(|f| f.id == "cdylib").expect("cdylib finding");
        assert_eq!(c.severity, SEV_BAN);
        assert_eq!(banned_knobs(&f).len(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn target_dir_is_not_scanned() {
        let d = tmpdir("skip");
        std::fs::write(d.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        std::fs::create_dir_all(d.join("target/debug")).unwrap();
        std::fs::write(d.join("target/debug/gen.rs"), "catch_unwind\n").unwrap();
        std::fs::write(d.join("src/main.rs"), "fn main() {}\n").unwrap();
        let f = scan(&d).unwrap();
        assert!(!f.iter().any(|f| f.id == "catch-unwind"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn comments_and_literals_are_not_evidence() {
        let src = concat!(
            "// catch_unwind in a line comment\n",
            "let a = 1;\n",
            "/* catch_unwind in a block comment\n",
            "   nested /* catch_unwind */ still a comment */\n",
            "let s = \"catch_unwind in a string\";\n",
            "let r = r#\"catch_unwind in a raw string\"#;\n",
            "let c = 'x';\n",
            "std::panic::catch_unwind(f);\n",
        );
        let code = strip_noncode(src);
        let lines: Vec<&str> = code.lines().collect();
        assert_eq!(
            lines.len(),
            src.lines().count(),
            "line numbers must survive"
        );
        for i in [0usize, 2, 3, 4, 5] {
            assert!(
                !lines[i].contains("catch_unwind"),
                "line {i} must be blanked: {:?}",
                lines[i]
            );
        }
        assert!(lines[7].contains("catch_unwind"));
        assert!(lines[6].contains("let c ="));
    }

    #[test]
    fn lifetimes_survive_but_char_literals_do_not() {
        let code = strip_noncode("fn f<'a>(x: &'a str) -> char { 'x' }\n");
        assert!(code.contains("fn f<'a>"), "{code}");
        assert!(code.contains("&'a str"), "{code}");
        assert!(!code.contains("'x'"), "{code}");
    }

    #[test]
    fn a_tree_that_only_mentions_catch_unwind_in_a_comment_is_clean() {
        let d = tmpdir("comment-only");
        std::fs::write(d.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        std::fs::write(d.join("Cargo.lock"), "version = 3\n").unwrap();
        std::fs::write(
            d.join("src/main.rs"),
            "//! This crate deliberately does not use catch_unwind.\nfn main() {}\n",
        )
        .unwrap();
        let f = scan(&d).unwrap();
        assert!(
            !f.iter().any(|f| f.id == "catch-unwind"),
            "a comment is not evidence"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn identifier_substrings_are_not_evidence() {
        let d = tmpdir("substring");
        std::fs::write(d.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        std::fs::write(d.join("Cargo.lock"), "version = 3\n").unwrap();
        std::fs::write(
            d.join("src/main.rs"),
            "fn catch_unwind_like() {}\nfn main() { let catch_unwind_flag = 1; let _ = catch_unwind_flag; }\n",
        )
        .unwrap();
        let f = scan(&d).unwrap();
        assert!(
            !f.iter().any(|f| f.id == "catch-unwind"),
            "an identifier that merely contains the word is not a call site"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn self_bootstrap_points_at_the_real_call_site() {
        // rustopt implements its own G0 rule with catch_unwind in main.rs. Scanning
        // its own tree must report that call, not this module's documentation that
        // merely talks about catch_unwind.
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let f = scan(&root).unwrap();
        let cu = f
            .iter()
            .find(|f| f.id == "catch-unwind")
            .expect("rustopt itself catches panics for the exit-2 contract");
        assert!(
            cu.evidence.starts_with("src/main.rs:"),
            "evidence must be the call site, got {}",
            cu.evidence
        );
    }
}
