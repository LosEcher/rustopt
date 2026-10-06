# rustopt

**Measure release-profile variants with real builds, get the smallest one that is
safe for your crate, and fail CI when the artifact grows past a budget.**

`rustopt` is not another size *reporter*. `cargo bloat`, `cargo bsize` and friends tell
you where the bytes are. `rustopt` runs the actual `cargo build --release` for a matrix
of `[profile.release]` variants, applies semantic guards to decide what may be
recommended, and then acts as a gate:

```
$ rustopt plan --manifest .
rustopt 0.1.0 - plan (run-1791296836526-0)
package   tiny 0.1.0
manifest  /tmp/rustopt-fixture-tiny.cSRJG4
toolchain rustc 1.97.0 / aarch64-apple-darwin
verdict   OK

VARIANT   STATUS   BYTES                    VS DEFAULT  VS CURRENT  BUILD
current   ok       431.23 KB (431232 bytes) -0.0%       -           0.5s
default   ok       431.30 KB (431296 bytes) -           +0.0%       0.2s
z         ok       431.38 KB (431376 bytes) +0.0%       +0.0%       0.2s
lto       ok       373.07 KB (373072 bytes) -13.5%      -13.5%      2.3s
tuned     ok       286.11 KB (286112 bytes) -33.7%      -33.7%      2.1s

RECOMMENDATION  tuned -> 286.11 KB (286112 bytes)  (-33.7% vs cargo defaults, -33.7% vs current)
  [profile.release]
  codegen-units = 1
  lto = "fat"
  opt-level = "z"
  strip = "symbols"
  rustopt does not edit files: copy the block above yourself

NOTES
  - trap: opt-level="z" on its own is 80 bytes LARGER than cargo's default (+0.0%). Single-knob comparisons mislead; only a measured combination counts.
  - lto="fat" + codegen-units=1 alone: 431.30 KB (431296 bytes) -> 373.07 KB (373072 bytes) (-13.5%)
  - current configuration 431.23 KB (431232 bytes) -> recommended 286.11 KB (286112 bytes) (-33.7%)
```

## Why measure instead of estimate

Two things go wrong with profile advice, and both are measurable:

1. **Single knobs mislead.** `opt-level = "z"` on its own is frequently *larger* than
   cargo's default `opt-level = 3` (less inlining, no unrolling). On the fixture above
   it is larger; on real crates it has been **+9%** and **+14%**. An estimator that adds
   up per-knob savings gets the sign wrong; a measured combination does not.
2. **"Default" is not "nothing".** Since Rust 1.77 cargo's release profile already sets
   `strip = "debuginfo"`. A baseline built by "only overriding what looks neutral" can
   therefore be *larger* than the real default — by ~4% in practice — which makes
   `strip` look far more valuable than it is. `rustopt` writes cargo's defaults out
   explicitly (`--config profile.release.strip="debuginfo"`, `opt-level=3`,
   `codegen-units=16`, …) and a test asserts that this reference lands within 0.5% of a
   package that declares no profile at all.

## Guards: what may be recommended

Size advice is only useful if it is safe for *your* crate. `rustopt` reads the package
before recommending anything:

| finding | severity | consequence |
|---|---|---|
| `catch-unwind` — source calls `std::panic::catch_unwind` | `ban` | `panic = "abort"` is taken off the table |
| `cdylib` — `crate-type = ["cdylib"]` / `["staticlib"]` | `ban` | same: panics must unwind across FFI |
| `abort-conflict` — the manifest already sets `panic = "abort"` **and** the source catches panics | `error` | reported as a pre-existing defect, not as advice |
| `should-panic` — `#[should_panic]` tests | `warn` | panic strategy is observable in tests |
| `backtrace-use` — the source refers to `Backtrace` / `backtrace::` | `warn` | `strip = "symbols"` degrades symbolization |
| `no-cargo-lock` — no `Cargo.lock` | `warn` | a build may write a lockfile into your tree |

Evidence is computed on **code only**: comments, string literals and raw strings are
blanked out before matching, and the needles require a path segment or a call site
(`::catch_unwind`, `catch_unwind(`, `#[should_panic`) rather than a bare word. A finding
that points at a comment, or at an identifier that merely contains the word, is worse than
no finding at all. A test asserts this against rustopt's own source, which catches panics
on purpose.

The trade-off is recall: a marker that only ever appears *inside* a string (say
`env::var("RUST_BACKTRACE")`) is not detected. Precision was chosen over recall for bans —
a wrong `ban` silently removes a real option.

`panic = "abort"` is never in the default variant set: it changes crash behaviour from
unwinding (exit 101) to SIGABRT (exit 134), and exit codes are part of a CLI's contract.
If you want to measure it anyway, ask for it: `--variants tuned,abort`.

## Install

Not on crates.io yet — install from git:

```sh
cargo install --git https://github.com/LosEcher/rustopt --locked
```

or build it in place:

```sh
cargo build --release --manifest-path path/to/rustopt/Cargo.toml
```

## Usage

```sh
rustopt plan   [--manifest DIR] [--variants a,b,c] [--work-dir DIR] [--ephemeral]
               [--dry-run] [--offline] [--jobs N] [--emit json|pretty]
               [--log PATH | --no-log]

rustopt check  [--manifest DIR] --budget SIZE [--work-dir DIR] [--ephemeral]
               [--offline] [--jobs N] [--emit json|pretty] [--log PATH | --no-log]

rustopt ledger [--tail N] [--run ID] [--log PATH] [--emit json|pretty]

rustopt clean  [--repo DIR] [--work-dir DIR] [--apply] [--emit json|pretty]
```

`plan` never edits your package: it prints the block to copy. `--dry-run` prints the exact
cargo argv without building anything. `check` measures what you ship today and compares it
with `--budget`; sizes accept `1500000`, `1.5MB` (1000-based) or `2MiB` (1024-based).

### Gate a release in CI

```yaml
- run: cargo install --git https://github.com/LosEcher/rustopt --locked
- run: rustopt check --manifest . --budget 2.5MB --no-log
```

`check` exits `1` when the artifact is over budget and `2` when the tool itself could not
measure (missing manifest, no bin target, failed build). A broken build therefore never
looks like a passing gate.

### Variants

| variant | overrides (on top of cargo's release defaults) | in default set |
|---|---|---|
| `current` | *(none — what the package ships today)* | yes |
| `default` | *(cargo's own defaults, written out explicitly)* | yes |
| `z` | `opt-level = "z"` | yes |
| `strip` | `strip = "symbols"` | |
| `lto` | `lto = "fat"`, `codegen-units = 1` | yes |
| `thin` | `lto = "thin"`, `codegen-units = 1` | |
| `tuned` | `opt-level = "z"`, `lto = "fat"`, `codegen-units = 1`, `strip = "symbols"` | yes |
| `abort` | `tuned` + `panic = "abort"` | |

`thin` exists because fat LTO is expensive: measure it before assuming the trade-off.

## Cost and isolation

Each variant is a full `cargo build --release` in its **own** `CARGO_TARGET_DIR`
(`~/.cache/rustopt/work/<package>-<hash>/<variant>`), so `plan` is slow the first time and
near-instant afterwards. Nothing is written into the package: `--locked` is passed whenever
a `Cargo.lock` exists, and the only file written into your working directory is the ledger
(`.rustopt/runs.jsonl`, disable with `--no-log`). `rustopt clean` previews the work dirs and
`rustopt clean --apply` removes them.

Every run appends to the ledger; `rustopt ledger --tail 5` shows past runs, `--run <id>`
shows one, and a run that crashed before finishing is reported as `interrupted` rather than
silently dropped. Compiler output is never stored — only a fingerprint and a byte count.

## What this does not do

* No `what-if` estimation: every number comes from a build, because estimation gets the
  sign wrong (see above).
* No code-level deduplication or shared monomorphization. That needs compiler changes
  (MIR/LLVM IR) and cannot be done safely from outside `rustc`.
* No binary packing/compression. It breaks macOS code signing, and a 5 MB binary is not
  usually the problem.
* No `target/` garbage collection: use `cargo sweep` or your own tooling.
* No dynamic-linking advice: shipping one static binary is a product decision.

## License

MIT
