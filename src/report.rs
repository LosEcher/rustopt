//! Human-readable rendering of plans and check reports.

use crate::budget::fmt_bytes;
use crate::plan::{CheckReport, Plan, VariantResult};

fn delta_cell(p: Option<f64>) -> String {
    match p {
        Some(v) => format!("{v:+.1}%"),
        None => "-".to_string(),
    }
}

fn bytes_cell(b: Option<u64>) -> String {
    match b {
        Some(b) => fmt_bytes(b),
        None => "-".to_string(),
    }
}

fn variant_line(r: &VariantResult) -> String {
    let ban = if r.banned { "  [banned]" } else { "" };
    format!(
        "{:<9} {:<8} {:<24} {:<11} {:<11} {:.1}s{}\n",
        r.outcome.variant,
        r.outcome.status,
        bytes_cell(r.outcome.bytes),
        delta_cell(r.delta_vs_default_pct),
        delta_cell(r.delta_vs_current_pct),
        r.outcome.duration_ms as f64 / 1000.0,
        ban
    )
}

#[must_use]
pub fn plan_pretty(p: &Plan) -> String {
    let mut s = String::new();
    s.push_str(&format!("{} {} - plan ({})\n", p.tool, p.version, p.run_id));
    s.push_str(&format!("package   {} {}\n", p.package, p.package_version));
    s.push_str(&format!("manifest  {}\n", p.manifest_dir));
    s.push_str(&format!("toolchain rustc {} / {}\n", p.rustc, p.host));
    s.push_str(&format!("verdict   {}\n\n", p.verdict.to_uppercase()));

    s.push_str(&format!(
        "{:<9} {:<8} {:<24} {:<11} {:<11} {}\n",
        "VARIANT", "STATUS", "BYTES", "VS DEFAULT", "VS CURRENT", "BUILD"
    ));
    for r in &p.variants {
        s.push_str(&variant_line(r));
    }
    s.push('\n');

    match &p.recommendation {
        Some(rec) => {
            s.push_str(&format!(
                "RECOMMENDATION  {} -> {}  ({} vs cargo defaults, {} vs current)\n",
                rec.variant,
                fmt_bytes(rec.bytes),
                delta_cell(rec.vs_default_pct),
                delta_cell(rec.vs_current_pct)
            ));
            if rec.profile.is_empty() {
                s.push_str("  (the current configuration already is the recommendation)\n");
            } else {
                s.push_str("  [profile.release]\n");
                for (k, v) in &rec.profile {
                    s.push_str(&format!("  {k} = {v}\n"));
                }
                s.push_str("  rustopt does not edit files: copy the block above yourself\n");
            }
        }
        None => s.push_str("RECOMMENDATION  none (no variant produced a measurement)\n"),
    }
    s.push('\n');

    if !p.findings.is_empty() {
        s.push_str("GUARDS\n");
        for f in &p.findings {
            s.push_str(&format!(
                "  {:<6} {:<16} {:<28} {}\n",
                f.severity, f.id, f.evidence, f.message
            ));
        }
        s.push('\n');
    }

    if !p.rejected.is_empty() {
        s.push_str("NOT RECOMMENDED\n");
        for r in &p.rejected {
            s.push_str(&format!(
                "  {knob} = {val} is off the table: {reason} ({ev})\n",
                knob = r.knob,
                val = r.value,
                reason = r.reason,
                ev = r.evidence
            ));
        }
        s.push('\n');
    }

    if !p.notes.is_empty() {
        s.push_str("NOTES\n");
        for n in &p.notes {
            s.push_str(&format!("  - {n}\n"));
        }
    }
    s
}

#[must_use]
pub fn check_pretty(c: &CheckReport) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "{} {} - check ({})\n",
        c.tool, c.version, c.run_id
    ));
    s.push_str(&format!("package   {} {}\n", c.package, c.package_version));
    s.push_str(&format!("manifest  {}\n", c.manifest_dir));
    s.push_str(&format!("toolchain rustc {} / {}\n", c.rustc, c.host));
    s.push_str(&format!(
        "variant   {} (what the package ships today)\n",
        c.variant
    ));
    s.push_str(&format!("command   {}\n", c.command));
    s.push_str(&format!(
        "measured  {}  across {} target(s)\n",
        fmt_bytes(c.measured_bytes),
        c.targets.len()
    ));
    for t in &c.targets {
        s.push_str(&format!("  - {} {}\n", t.target, fmt_bytes(t.bytes)));
    }
    s.push_str(&format!("budget    {}\n", fmt_bytes(c.budget_bytes)));
    let head = if c.headroom_bytes >= 0 {
        format!(
            "within budget by {}",
            fmt_bytes(c.headroom_bytes.unsigned_abs())
        )
    } else {
        format!(
            "over budget by {}",
            fmt_bytes(c.headroom_bytes.unsigned_abs())
        )
    };
    s.push_str(&format!(
        "verdict   {}  ({})\n",
        c.verdict.to_uppercase(),
        head
    ));
    for n in &c.notes {
        s.push_str(&format!("  - {n}\n"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delta_and_bytes_cells_handle_missing_values() {
        assert_eq!(delta_cell(None), "-");
        assert_eq!(delta_cell(Some(-51.83)), "-51.8%");
        assert_eq!(bytes_cell(None), "-");
        assert_eq!(bytes_cell(Some(2548384)), "2.55 MB (2548384 bytes)");
    }
}
