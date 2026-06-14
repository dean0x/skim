// AC2 (NEGATIVE, type-level): Automated trybuild compile-fail tests.
//
// Proves that the non-waivered `Contract` transform surface cannot express:
// (a) returning an error to the caller in place of passthrough — type-level
//     (the transform return type is `Outcome`, not `Result<Outcome, _>`)
// (b) no dedicated byte-grow API exists on `Outcome` — calling a non-existent
//     `grow_bytes_unchecked` method fails to compile
// (c) no dedicated turn-reorder API exists on `Outcome` — calling a non-existent
//     `reorder_turns` method fails to compile
// (d) no dedicated hot-zone mutation API exists on `Outcome` — calling a
//     non-existent `mutate_hot_zone_at` method fails to compile
//
// # Scope of type-level enforcement
//
// Case (a) is a STRONG type-level proof: the `transform` return type is `Outcome`,
// not `Result<Outcome, _>`. Returning a `Result` instead of an `Outcome` is a
// compile-time error. The type system makes fail-open the ONLY shape.
//
// Cases (b), (c), (d) are WEAKER type-level proofs: they confirm that NO
// DEDICATED API exists on `Outcome` for byte-growth, turn-reorder, or hot-zone
// mutation. These operations are NOT structurally unrepresentable (a caller can
// produce inflated bytes via `Outcome::modified` with a larger-than-input buffer),
// but they require the caller to knowingly bypass conventions rather than calling
// a convenient "grow bytes" shortcut. The runtime never-inflate gate in
// `guarded_transform` is the PRIMARY enforcement for byte-growth; the type system
// provides absence-of-dedicated-API as a secondary signal.
//
// AC2's plan text says the trybuild tests are "the gate." For cases (b)–(d) this
// means: the gate is "no convenience API for violation exists" — not "violation is
// structurally impossible." This is documented here to prevent future confusion.
//
// Reference: DECISIONS-RESOLVED.md Decision 1; rskim-contract plan AC2.

#[test]
fn ac2_case_a_error_return_is_type_error() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/a_error_return.rs");
}

#[test]
fn ac2_case_b_no_dedicated_grow_bytes_api() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/b_grow_bytes.rs");
}

#[test]
fn ac2_case_c_no_dedicated_reorder_turns_api() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/c_reorder_turns.rs");
}

#[test]
fn ac2_case_d_no_dedicated_hot_zone_mutation_api() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/d_hot_zone_mutation.rs");
}
