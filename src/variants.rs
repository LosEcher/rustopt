//! The variant catalogue: the single source of truth for what gets built.
//!
//! A *variant* is a named set of `profile.release` overrides. Overrides are
//! applied with `cargo --config profile.release.<key>=<toml literal>`, never with
//! `CARGO_PROFILE_*` environment variables: env vars leak into subprocesses and
//! make the recorded command line less faithful.
//!
//! The `default` variant writes cargo's own release defaults **explicitly**.
//! That matters: since Rust 1.77 cargo's default release profile already sets
//! `strip = "debuginfo"`, so a baseline built by "not overriding anything" (or by
//! overriding only what you think is neutral) can be *larger* than the real
//! default and make `strip` look more valuable than it is.

/// The profile `plan` ablates and `check` measures unless `--build-profile`
/// selects another one.
///
/// `release` is cargo's built-in profile, so it needs no manifest declaration.
/// Any other name (for example `dist`, the profile this crate ships from) must
/// exist in the package's manifest, and cargo rejects the build if it does not —
/// which is what makes a typo behave as a tool error instead of a false pass.
pub const DEFAULT_PROFILE: &str = "release";

/// `--config` key prefix for a named profile, e.g. `profile.release`.
#[must_use]
pub fn profile_prefix(profile: &str) -> String {
    format!("profile.{profile}")
}

/// cargo's release-profile defaults, spelled out (TOML literals, quotes included).
pub const CARGO_DEFAULTS: &[(&str, &str)] = &[
    ("opt-level", "3"),
    ("debug", "false"),
    ("lto", "false"),
    ("codegen-units", "16"),
    ("strip", "\"debuginfo\""),
    ("panic", "\"unwind\""),
];

#[derive(Debug, Clone, Copy)]
pub struct Variant {
    pub name: &'static str,
    /// Overrides relative to [`CARGO_DEFAULTS`].
    pub delta: &'static [(&'static str, &'static str)],
    /// `true` for the variant that passes **no** `--config` at all, i.e. "what this
    /// package ships today". Only `current` uses it.
    pub no_overrides: bool,
    /// Whether `plan` runs this variant when `--variants` is not given.
    pub default_set: bool,
    pub note: &'static str,
}

pub const VARIANTS: &[Variant] = &[
    Variant {
        name: "current",
        delta: &[],
        no_overrides: true,
        default_set: true,
        note: "no overrides: what the package ships today",
    },
    Variant {
        name: "default",
        delta: &[],
        no_overrides: false,
        default_set: true,
        note: "cargo's own release defaults, written out explicitly",
    },
    Variant {
        name: "z",
        delta: &[("opt-level", "\"z\"")],
        no_overrides: false,
        default_set: true,
        note: "single knob; usually reduces nothing on its own",
    },
    Variant {
        name: "strip",
        delta: &[("strip", "\"symbols\"")],
        no_overrides: false,
        default_set: false,
        note: "symbol table removed (loses nm/bloat symbolization)",
    },
    Variant {
        name: "lto",
        delta: &[("lto", "\"fat\""), ("codegen-units", "1")],
        no_overrides: false,
        default_set: true,
        note: "fat LTO + one codegen unit",
    },
    Variant {
        name: "thin",
        delta: &[("lto", "\"thin\""), ("codegen-units", "1")],
        no_overrides: false,
        default_set: false,
        note: "thin LTO: cheaper builds, measure it before assuming",
    },
    Variant {
        name: "tuned",
        delta: &[
            ("opt-level", "\"z\""),
            ("lto", "\"fat\""),
            ("codegen-units", "1"),
            ("strip", "\"symbols\""),
        ],
        no_overrides: false,
        default_set: true,
        note: "the combination this tool recommends when guards allow it",
    },
    Variant {
        name: "abort",
        delta: &[
            ("opt-level", "\"z\""),
            ("lto", "\"fat\""),
            ("codegen-units", "1"),
            ("strip", "\"symbols\""),
            ("panic", "\"abort\""),
        ],
        no_overrides: false,
        default_set: false,
        note: "changes crash semantics: unwinding (101) becomes SIGABRT (134)",
    },
];

#[must_use]
pub fn all() -> &'static [Variant] {
    VARIANTS
}

#[must_use]
pub fn by_name(name: &str) -> Option<&'static Variant> {
    VARIANTS.iter().find(|v| v.name == name)
}

#[must_use]
pub fn names() -> Vec<&'static str> {
    VARIANTS.iter().map(|v| v.name).collect()
}

#[must_use]
pub fn default_set() -> Vec<&'static Variant> {
    VARIANTS.iter().filter(|v| v.default_set).collect()
}

/// Parse `a,b,c`. An empty spec means [`default_set`].
pub fn parse_list(spec: &str) -> Result<Vec<&'static Variant>, String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Ok(default_set());
    }
    let mut out = Vec::new();
    for raw in spec.split(',') {
        let name = raw.trim();
        if name.is_empty() {
            continue;
        }
        let v = by_name(name)
            .ok_or_else(|| format!("unknown variant {name:?}; known: {}", names().join(", ")))?;
        if !out.iter().any(|e: &&Variant| e.name == v.name) {
            out.push(v);
        }
    }
    if out.is_empty() {
        return Err(format!(
            "no variants selected; known: {}",
            names().join(", ")
        ));
    }
    Ok(out)
}

/// Effective `profile.release` keys for a variant: cargo's defaults with the
/// variant's delta applied. Order is stable (defaults first, then new keys).
#[must_use]
pub fn composed_knobs(v: &Variant) -> Vec<(&'static str, &'static str)> {
    if v.no_overrides {
        return Vec::new();
    }
    let mut out: Vec<(&'static str, &'static str)> = CARGO_DEFAULTS.to_vec();
    for (k, val) in v.delta {
        if let Some(slot) = out.iter_mut().find(|(dk, _)| dk == k) {
            slot.1 = val;
        } else {
            out.push((k, val));
        }
    }
    out
}

/// `--config` argv fragment for a variant (empty for `current`), applied to the
/// named profile.
#[must_use]
pub fn config_args(v: &Variant, profile: &str) -> Vec<String> {
    let prefix = profile_prefix(profile);
    let mut out = Vec::new();
    for (k, val) in composed_knobs(v) {
        out.push("--config".to_string());
        out.push(format!("{prefix}.{k}={val}"));
    }
    out
}

/// The keys a user would actually write into `Cargo.toml` for this variant.
#[must_use]
pub fn manifest_knobs(v: &Variant) -> &'static [(&'static str, &'static str)] {
    v.delta
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_variant_spells_out_cargo_defaults() {
        let v = by_name("default").unwrap();
        let knobs = composed_knobs(v);
        assert!(knobs.contains(&("strip", "\"debuginfo\"")));
        assert!(knobs.contains(&("opt-level", "3")));
        assert!(knobs.contains(&("lto", "false")));
        assert!(knobs.contains(&("codegen-units", "16")));
        assert!(knobs.contains(&("panic", "\"unwind\"")));
        assert_eq!(knobs.len(), CARGO_DEFAULTS.len());
    }

    #[test]
    fn current_passes_no_overrides() {
        let v = by_name("current").unwrap();
        assert!(v.no_overrides);
        assert!(config_args(v, DEFAULT_PROFILE).is_empty());
        assert!(composed_knobs(v).is_empty());
    }

    #[test]
    fn tuned_overrides_four_knobs() {
        let v = by_name("tuned").unwrap();
        let knobs = composed_knobs(v);
        assert_eq!(knobs.len(), CARGO_DEFAULTS.len());
        assert!(knobs.contains(&("opt-level", "\"z\"")));
        assert!(knobs.contains(&("lto", "\"fat\"")));
        assert!(knobs.contains(&("codegen-units", "1")));
        assert!(knobs.contains(&("strip", "\"symbols\"")));
        assert!(knobs.contains(&("panic", "\"unwind\"")));
    }

    #[test]
    fn config_args_are_pairs_of_flag_and_literal() {
        let v = by_name("lto").unwrap();
        let args = config_args(v, DEFAULT_PROFILE);
        assert_eq!(args.len() % 2, 0);
        assert!(args.iter().any(|a| a == "profile.release.lto=\"fat\""));
        assert!(args.iter().any(|a| a == "profile.release.codegen-units=1"));
    }

    #[test]
    fn config_args_follow_the_selected_profile() {
        let v = by_name("tuned").unwrap();
        let args = config_args(v, "dist");
        assert!(
            args.iter().any(|a| a == "profile.dist.opt-level=\"z\""),
            "{args:?}"
        );
        assert!(
            args.iter().any(|a| a == "profile.dist.strip=\"symbols\""),
            "{args:?}"
        );
        assert!(
            !args.iter().any(|a| a.starts_with("profile.release.")),
            "a named profile must not leak the release prefix: {args:?}"
        );
    }

    #[test]
    fn abort_variant_adds_panic_abort() {
        let v = by_name("abort").unwrap();
        assert!(composed_knobs(v).contains(&("panic", "\"abort\"")));
        assert!(!v.default_set, "abort must never be in the default set");
    }

    #[test]
    fn parse_list_defaults_and_validates() {
        assert_eq!(parse_list("").unwrap().len(), default_set().len());
        assert_eq!(parse_list("tuned").unwrap()[0].name, "tuned");
        assert_eq!(parse_list(" tuned , strip ").unwrap().len(), 2);
        assert_eq!(parse_list("tuned,tuned").unwrap().len(), 1);
        let err = parse_list("nope").unwrap_err();
        assert!(err.contains("unknown variant"));
        assert!(parse_list("tuned,,strip").unwrap().len() == 2);
    }

    #[test]
    fn default_set_contains_the_trap_detector_and_recommendation() {
        let names: Vec<&str> = default_set().iter().map(|v| v.name).collect();
        assert!(names.contains(&"current"));
        assert!(names.contains(&"default"));
        assert!(names.contains(&"z"));
        assert!(names.contains(&"tuned"));
    }
}
