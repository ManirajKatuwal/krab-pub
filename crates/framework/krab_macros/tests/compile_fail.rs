/// Compile-error tests for `view!` and `#[island]` macros using `trybuild`.
///
/// Each `.rs` file in `tests/compile_fail/` should fail to compile with the
/// expected diagnostic.  Run with `cargo test --test compile_fail`.
#[test]
fn compile_fail_cases() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/*.rs");
}
