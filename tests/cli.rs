//! End-to-end tests: run the real binary against materialised fixture packages.
//!
//! These are the mechanical gates for the P0 contract:
//!   * exit 0 for a plan, exit 1 for an exceeded budget, exit 2 for tool errors
//!   * the `default` variant really does reproduce a package without any
//!     `[profile]` (the regression test for cargo's own `strip = "debuginfo"`
//!     release default: a baseline built by "not overriding anything" can be off
//!     by whole percent, which makes `strip` look more valuable than it is)
//!   * a package that catches panics never gets `panic = "abort"` recommended
//!
//! Fixtures live in `tests/fixtures/<name>/` with their manifest named
//! `manifest.toml`; `cargo package` refuses to ship any subdirectory containing a
//! `Cargo.toml`, so the tests copy a fixture to a temp dir and rename it there.
//! That keeps the published crate self-testing.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_rustopt"))
}

fn scratch(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("rustopt-it-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let target = to.join(e.file_name());
        if e.path().is_dir() {
            copy_tree(&e.path(), &target);
        } else {
            std::fs::copy(e.path(), &target).unwrap();
        }
    }
}

/// Copy a fixture package into a private temp dir and give it a real manifest.
fn fixture(tag: &str, name: &str) -> PathBuf {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    assert!(
        src.is_dir(),
        "fixture {name} is missing at {}",
        src.display()
    );
    let dest = scratch(&format!("{tag}-{name}")).join(name);
    copy_tree(&src, &dest);
    std::fs::rename(dest.join("manifest.toml"), dest.join("Cargo.toml")).unwrap();
    dest
}

struct Run {
    code: i32,
    stdout: String,
}

fn run(args: &[&str]) -> Run {
    let out = Command::new(bin())
        .args(args)
        .output()
        .expect("failed to spawn rustopt");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
    }
}

fn json(args: &[&str]) -> serde_json::Value {
    let r = run(args);
    assert_eq!(r.code, 0, "expected exit 0, got {}", r.code);
    serde_json::from_str(&r.stdout).unwrap_or_else(|e| panic!("bad json: {e}\n{}", r.stdout))
}

#[test]
fn dry_run_reports_the_variant_matrix_without_building() {
    let dir = fixture("dry", "tiny");
    let work = scratch("dry-work");
    let v = json(&[
        "plan",
        "--manifest",
        dir.to_str().unwrap(),
        "--work-dir",
        work.to_str().unwrap(),
        "--dry-run",
        "--no-log",
        "--emit",
        "json",
    ]);

    assert_eq!(v["verdict"], "dry-run");
    assert_eq!(v["package"], "tiny");
    let variants = v["variants"].as_array().unwrap();
    assert!(variants.len() >= 4, "default set should have >= 4 variants");

    let default = variants
        .iter()
        .find(|x| x["variant"] == "default")
        .expect("default variant");
    let cmd = default["command"].as_str().unwrap();
    // The whole point of the `default` variant: cargo's own release default is
    // strip = "debuginfo", and it must be written out explicitly.
    assert!(
        cmd.contains("profile.release.strip=\"debuginfo\""),
        "default variant must set strip=debuginfo explicitly: {cmd}"
    );
    assert!(cmd.contains("profile.release.opt-level=3"), "{cmd}");
    assert!(cmd.contains("profile.release.codegen-units=16"), "{cmd}");

    let current = variants
        .iter()
        .find(|x| x["variant"] == "current")
        .expect("current variant");
    assert!(
        !current["command"].as_str().unwrap().contains("profile."),
        "the current variant must not override anything"
    );
    // A dry run must not have built anything.
    assert!(default["bytes"].is_null());
}

#[test]
fn measured_plan_matches_the_reference_and_finds_a_smaller_variant() {
    let dir = fixture("plan", "tiny");
    let work = scratch("plan-work");
    let v = json(&[
        "plan",
        "--manifest",
        dir.to_str().unwrap(),
        "--variants",
        "current,default,lto,tuned",
        "--work-dir",
        work.to_str().unwrap(),
        "--offline",
        "--no-log",
        "--emit",
        "json",
    ]);

    assert_eq!(v["verdict"], "ok");
    let variants = v["variants"].as_array().unwrap();
    let bytes = |name: &str| -> u64 {
        variants
            .iter()
            .find(|x| x["variant"] == name)
            .unwrap_or_else(|| panic!("variant {name} missing"))["bytes"]
            .as_u64()
            .unwrap_or_else(|| panic!("variant {name} has no bytes"))
    };

    // The fixture declares no [profile] at all, so the explicitly-written cargo
    // defaults must land within a hair of the package's own configuration. (They
    // are not byte-identical: forcing the options changes the option hash that
    // gets embedded in the artifact, which is a few dozen bytes.)
    let current = bytes("current");
    let default = bytes("default");
    let drift = (current as f64 - default as f64).abs() / current as f64;
    assert!(
        drift < 0.005,
        "default variant drifted {:.3}% from the no-profile baseline \
         (current={current}, default={default}) - the baseline is not trustworthy",
        drift * 100.0
    );

    let tuned = bytes("tuned");
    assert!(
        tuned < default,
        "tuned ({tuned}) should beat cargo's default profile ({default})"
    );

    let rec = &v["recommendation"];
    assert_eq!(rec["variant"], "tuned");
    assert_eq!(rec["bytes"].as_u64().unwrap(), tuned);
    assert_eq!(
        rec["applied"], false,
        "plan must never claim to have edited files"
    );
    assert!(rec["profile"]["lto"].as_str().unwrap().contains("fat"));

    // Size and build time pull in opposite directions, so the recommendation reports
    // the price of its own advice. The recommended variant was built, so its own time
    // is always there; the ratio needs a usable `default` baseline (a no-op build can
    // legitimately measure 0 ms).
    assert!(
        rec["build_ms"].as_u64().unwrap() > 0,
        "the recommended variant's build time must be reported: {rec}"
    );
    if rec["default_build_ms"].as_u64().unwrap_or(0) > 0 {
        let ratio = rec["price_ratio_vs_default"]
            .as_f64()
            .expect("a measured default build must yield a price ratio");
        assert!(ratio > 0.0, "ratio must be positive, got {ratio}");
    }
}

#[test]
fn budget_gate_returns_one_when_exceeded_and_zero_when_not() {
    let dir = fixture("budget", "tiny");
    let work = scratch("budget-work");
    let over = run(&[
        "check",
        "--manifest",
        dir.to_str().unwrap(),
        "--budget",
        "1",
        "--work-dir",
        work.to_str().unwrap(),
        "--offline",
        "--no-log",
    ]);
    assert_eq!(over.code, 1, "over budget must exit 1");

    let under = run(&[
        "check",
        "--manifest",
        dir.to_str().unwrap(),
        "--budget",
        "100MB",
        "--work-dir",
        work.to_str().unwrap(),
        "--offline",
        "--no-log",
    ]);
    assert_eq!(under.code, 0, "within budget must exit 0");
    assert!(under.stdout.contains("PASS"), "{}", under.stdout);
}

#[test]
fn tool_errors_exit_two_not_one() {
    let dir = fixture("errors", "tiny");
    // A missing directory is a tool error, and must never look like a failed gate.
    let missing = run(&[
        "check",
        "--manifest",
        "/nonexistent-rustopt-path",
        "--budget",
        "1MB",
        "--no-log",
    ]);
    assert_eq!(missing.code, 2);

    // Missing required option.
    let no_budget = run(&["check", "--manifest", dir.to_str().unwrap(), "--no-log"]);
    assert_eq!(no_budget.code, 2);

    // Unknown command / unknown flag / bad enum value / bad size literal.
    assert_eq!(run(&["frobnicate"]).code, 2);
    assert_eq!(run(&["plan", "--nope"]).code, 2);
    assert_eq!(run(&["plan", "--emit", "xml"]).code, 2);
    assert_eq!(run(&["check", "--budget", "12XB"]).code, 2);

    // --help / --version are the only zero-exit non-work invocations.
    assert_eq!(run(&["--help"]).code, 0);
    assert_eq!(run(&["--version"]).code, 0);
}

#[test]
fn guards_take_panic_abort_off_the_table() {
    let dir = fixture("guards", "guarded");
    let v = json(&[
        "plan",
        "--manifest",
        dir.to_str().unwrap(),
        "--variants",
        "current,default,tuned,abort",
        "--dry-run",
        "--no-log",
        "--emit",
        "json",
    ]);

    let findings = v["findings"].as_array().unwrap();
    let cu = findings
        .iter()
        .find(|f| f["id"] == "catch-unwind")
        .expect("catch-unwind finding");
    assert_eq!(cu["severity"], "ban");
    assert!(
        cu["evidence"].as_str().unwrap().ends_with("src/main.rs:7"),
        "evidence should point at the call site, not a comment: {}",
        cu["evidence"]
    );

    let abort = v["variants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["variant"] == "abort")
        .expect("abort variant");
    assert_eq!(abort["banned"], true);
    assert!(abort["ban_reason"]
        .as_str()
        .unwrap()
        .contains("catch_unwind"));

    let rejected = v["rejected"].as_array().unwrap();
    assert_eq!(rejected[0]["knob"], "panic");
    assert_eq!(rejected[0]["value"], "\"abort\"");
}

#[test]
fn ledger_records_runs_and_projects_them_back() {
    let dir = fixture("ledger", "tiny");
    let work = scratch("ledger-work");
    let log = work.join("runs.jsonl");

    let r = run(&[
        "check",
        "--manifest",
        dir.to_str().unwrap(),
        "--budget",
        "100MB",
        "--work-dir",
        work.to_str().unwrap(),
        "--offline",
        "--log",
        log.to_str().unwrap(),
    ]);
    assert_eq!(r.code, 0);
    assert!(log.exists(), "ledger must be written at the requested path");

    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains("\"t\":\"run.start\""));
    assert!(text.contains("\"t\":\"run.finish\""));
    // Evidence discipline: no raw compiler output in the ledger.
    assert!(
        !text.contains("Compiling"),
        "raw cargo output leaked into the ledger"
    );

    let v = json(&["ledger", "--log", log.to_str().unwrap(), "--emit", "json"]);
    let runs = v.as_array().unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0]["command"], "check");
    assert_eq!(runs[0]["verdict"], "pass");
    assert!(!runs[0]["interrupted"].as_bool().unwrap());
    assert!(runs[0]["measured_bytes"].as_u64().unwrap() > 0);
}

#[test]
fn ledger_of_a_missing_file_is_empty_not_an_error() {
    let work = scratch("noledger");
    let r = run(&[
        "ledger",
        "--log",
        work.join("absent.jsonl").to_str().unwrap(),
    ]);
    assert_eq!(r.code, 0);
    assert!(r.stdout.contains("(no runs)"), "{}", r.stdout);
}

#[test]
fn ledger_reports_a_dangling_run_as_interrupted() {
    let work = scratch("interrupted");
    let log = work.join("runs.jsonl");
    std::fs::write(
        &log,
        "{\"t\":\"run.start\",\"run_id\":\"run-x\",\"version\":\"0.1.0\",\"ts\":1000,\"command\":\"plan\",\"cwd\":\"/x\",\"manifest\":\"/x\",\"variants\":[\"tuned\"]}\n",
    )
    .unwrap();
    let v = json(&["ledger", "--log", log.to_str().unwrap(), "--emit", "json"]);
    assert_eq!(v[0]["interrupted"], true);
}

#[test]
fn clean_previews_then_applies() {
    let dir = fixture("clean", "tiny");
    let work = scratch("clean-work");
    // Build one variant so there is something to clean.
    let r = run(&[
        "check",
        "--manifest",
        dir.to_str().unwrap(),
        "--budget",
        "100MB",
        "--work-dir",
        work.to_str().unwrap(),
        "--offline",
        "--no-log",
    ]);
    assert_eq!(r.code, 0);

    let preview = run(&["clean", "--work-dir", work.to_str().unwrap()]);
    assert_eq!(preview.code, 0);
    assert!(work.exists(), "preview must not delete anything");
    assert!(preview.stdout.contains("logical"), "{}", preview.stdout);

    let applied = run(&["clean", "--work-dir", work.to_str().unwrap(), "--apply"]);
    assert_eq!(applied.code, 0);
    assert!(!work.exists(), "--apply must remove the work dir");
}

#[test]
fn check_can_gate_a_named_profile_instead_of_release() {
    // The gate has to be able to measure what the package actually publishes: a
    // crate that ships from `--profile dist` otherwise passes a release-sized
    // budget while the artifact it uploads stays ungated.
    let dir = fixture("namedprofile", "tiny");
    let work = scratch("namedprofile-work");
    let v = json(&[
        "check",
        "--manifest",
        dir.to_str().unwrap(),
        "--budget",
        "10MB",
        "--build-profile",
        "dist",
        "--work-dir",
        work.to_str().unwrap(),
        "--offline",
        "--no-log",
        "--emit",
        "json",
    ]);
    let cmd = v["command"].as_str().unwrap();
    assert!(cmd.contains("--profile dist"), "{cmd}");
    assert!(!cmd.contains("--release"), "{cmd}");
    assert_eq!(v["build_profile"], "dist");
    assert_eq!(v["verdict"], "pass");
    assert!(v["measured_bytes"].as_u64().unwrap() > 0);
}

#[test]
fn an_unmeasurable_profile_is_a_tool_error_not_a_pass() {
    // Fail closed: a profile cargo cannot build must exit 2, never 0/1.
    let dir = fixture("badprofile", "tiny");
    let r = run(&[
        "check",
        "--manifest",
        dir.to_str().unwrap(),
        "--budget",
        "10MB",
        "--build-profile",
        "not-a-profile",
        "--offline",
        "--no-log",
    ]);
    assert_eq!(r.code, 2, "stdout was: {}", r.stdout);
    assert!(!r.stdout.contains("PASS"), "{}", r.stdout);
}

#[test]
fn a_profile_name_cannot_smuggle_config_syntax() {
    let dir = fixture("badname", "tiny");
    let r = run(&[
        "check",
        "--manifest",
        dir.to_str().unwrap(),
        "--budget",
        "10MB",
        "--build-profile",
        "dist.opt-level=\"z\"",
        "--no-log",
    ]);
    assert_eq!(r.code, 2, "stdout was: {}", r.stdout);
    assert!(
        r.stdout.is_empty(),
        "a rejected profile name must not reach cargo: {}",
        r.stdout
    );
}

#[test]
fn plan_reports_the_profile_it_ablated() {
    let dir = fixture("plantprofile", "tiny");
    let v = json(&[
        "plan",
        "--manifest",
        dir.to_str().unwrap(),
        "--build-profile",
        "dist",
        "--dry-run",
        "--no-log",
        "--emit",
        "json",
    ]);
    assert_eq!(v["build_profile"], "dist");
    let default = v["variants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["variant"] == "default")
        .expect("default variant");
    let cmd = default["command"].as_str().unwrap();
    assert!(cmd.contains("--profile dist"), "{cmd}");
    assert!(cmd.contains("profile.dist.opt-level=3"), "{cmd}");
    assert!(
        !cmd.contains("profile.release."),
        "a dist plan must not write release overrides: {cmd}"
    );
}

/// A two-package workspace: a virtual root plus one member package. This is the
/// layout where a `[profile.*]` recommendation can be written somewhere inert,
/// because cargo reads profiles only from the workspace root manifest.
fn workspace_fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = scratch(&format!("{tag}-ws"));
    let member = root.join("member");
    std::fs::create_dir_all(member.join("src")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"member\"]\nresolver = \"2\"\n",
    )
    .unwrap();
    // The member declares the very knob a plan would recommend, in the place
    // cargo ignores.
    std::fs::write(
        member.join("Cargo.toml"),
        "[package]\nname = \"ws-member\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [profile.release]\nstrip = \"symbols\"\n",
    )
    .unwrap();
    std::fs::write(
        member.join("src/main.rs"),
        "fn main() { println!(\"hi\"); }\n",
    )
    .unwrap();
    (root, member)
}

#[test]
fn a_workspace_member_is_told_where_the_profile_must_be_written() {
    let (root, member) = workspace_fixture("profile-site");
    let work = scratch("profile-site-work");

    let v = json(&[
        "plan",
        "--manifest",
        member.to_str().unwrap(),
        "--work-dir",
        work.to_str().unwrap(),
        "--dry-run",
        "--no-log",
        "--emit",
        "json",
    ]);

    assert_eq!(
        v["profile_site"]["is_workspace_root"], false,
        "a member package is not the workspace root: {v}"
    );
    let effective = v["profile_site"]["effective_manifest"].as_str().unwrap();
    assert_eq!(
        std::fs::canonicalize(effective).unwrap(),
        std::fs::canonicalize(root.join("Cargo.toml")).unwrap(),
        "the recommendation must name the workspace root manifest, not the member"
    );

    let finding = v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["id"] == "profile-site")
        .unwrap_or_else(|| panic!("a member package must carry a profile-site warning: {v}"));
    assert_eq!(finding["severity"], "warn");
    let message = finding["message"].as_str().unwrap();
    assert!(
        message.contains("workspace root manifest"),
        "the warning must say why the site matters: {message}"
    );
}

#[test]
fn a_standalone_package_gets_no_profile_site_warning() {
    // Negative control for the test above: the warning is about the layout, so a
    // package that *is* its own workspace root must stay quiet. Without this arm
    // an always-warn implementation would pass.
    let dir = fixture("profile-site-solo", "tiny");
    let work = scratch("profile-site-solo-work");

    let v = json(&[
        "plan",
        "--manifest",
        dir.to_str().unwrap(),
        "--work-dir",
        work.to_str().unwrap(),
        "--dry-run",
        "--no-log",
        "--emit",
        "json",
    ]);

    assert_eq!(v["profile_site"]["is_workspace_root"], true, "{v}");
    assert!(
        v["profile_site"]["warning"].is_null(),
        "a standalone package needs no site warning: {v}"
    );
    assert!(
        !v["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["id"] == "profile-site"),
        "a standalone package must not report profile-site: {v}"
    );
}

/// Pins the cargo rule the `profile-site` finding rests on, from cargo's own
/// output rather than from documentation. If a future cargo starts honouring a
/// member's `[profile.*]`, this goes red and the finding must be reworded — not
/// silently kept.
#[test]
fn cargo_ignores_a_profile_declared_in_a_workspace_member() {
    let (root, _member) = workspace_fixture("cargo-rule");
    let manifest = root.join("Cargo.toml");
    let build = |extra: &[&str]| -> String {
        let mut cmd = Command::new("cargo");
        cmd.arg("build")
            .arg("--release")
            .arg("-v")
            .arg("--offline")
            .arg("--manifest-path")
            .arg(&manifest);
        for e in extra {
            cmd.arg(e);
        }
        let out = cmd.output().expect("failed to spawn cargo");
        // Verbose cargo writes the rustc invocation to stderr.
        String::from_utf8_lossy(&out.stderr).to_string()
    };

    let member_declared = build(&[]);
    assert!(
        member_declared.contains("profiles for the non root package will be ignored"),
        "cargo's own warning is the observable this finding is about:\n{member_declared}"
    );
    assert!(
        !member_declared.contains("-C strip=symbols"),
        "cargo ignored the member's profile (as this finding claims), so the flag must be \
         absent; if cargo honours it now, the finding is wrong:\n{member_declared}"
    );

    // The form rustopt itself uses must take effect, otherwise a measurement
    // would be of the wrong configuration.
    let via_config = build(&["--config", "profile.release.strip=\"symbols\""]);
    assert!(
        via_config.contains("-C strip=symbols"),
        "`--config` must apply the override that a member manifest cannot:\n{via_config}"
    );
}
