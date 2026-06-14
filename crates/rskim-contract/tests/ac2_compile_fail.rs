// AC2 (NEGATIVE, type-level): Automated trybuild compile-fail tests.
//
// Proves that the non-waivered `Contract` transform surface cannot express:
// (a) returning an error to the caller in place of passthrough
// (b) growing output bytes via the core trait
// (c) deleting/inserting/reordering turns via the core trait
// (d) mutating bytes outside the live zone via the core trait
//
// Each test case is a Rust file in `tests/compile_fail/` that must fail to
// compile. The trybuild harness verifies that the compilation fails with the
// expected error. Manual trait-surface review is supporting evidence only;
// these automated tests are the gate (per AC2).
//
// Reference: DECISIONS-RESOLVED.md Decision 1; rskim-contract plan AC2.

#[test]
fn ac2_case_a_error_return_is_type_error() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/a_error_return.rs");
}

#[test]
fn ac2_case_b_grow_bytes_method_does_not_exist() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/b_grow_bytes.rs");
}

#[test]
fn ac2_case_c_reorder_turns_method_does_not_exist() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/c_reorder_turns.rs");
}

#[test]
fn ac2_case_d_hot_zone_mutation_method_does_not_exist() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/d_hot_zone_mutation.rs");
}
