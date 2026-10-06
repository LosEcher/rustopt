# Test fixtures

Each fixture is a complete cargo package, but its manifest is named
`manifest.toml` instead of `Cargo.toml` on purpose:

> `cargo package` refuses to include any subdirectory that contains a
> `Cargo.toml`, so a fixture stored as `Cargo.toml` would be missing from the
> published crate and `cargo test` would fail for anyone who downloaded it.

`tests/cli.rs` materialises a fixture into a temporary directory (copy, then
rename `manifest.toml` to `Cargo.toml`) before running `rustopt` on it. The same
thing can be done by hand:

```sh
dir=$(bash tests/fixtures/materialize.sh tiny)
cargo run -- plan --manifest "$dir"
```

| fixture | what it is for |
|---|---|
| `tiny` | a package with no `[profile]` at all: the `default` variant must land within a hair of it |
| `guarded` | a package that implements its exit-code contract with `catch_unwind`: `panic = "abort"` must be reported as banned, with the call site as evidence |
