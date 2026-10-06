//! Fixture: a package whose exit-code contract depends on catching a panic.
//!
//! `panic = "abort"` must therefore never be recommended for it: the guard below
//! could not run, and a panic would abort the process instead of unwinding.

fn main() {
    let caught = std::panic::catch_unwind(|| 1 + 1);
    println!("{}", caught.unwrap_or(0));
}
