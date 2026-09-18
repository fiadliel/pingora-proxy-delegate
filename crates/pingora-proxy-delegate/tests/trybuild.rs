#[test]
fn ui() {
    let t = trybuild::TestCases::new();
    // The dev-dependency always enables `pingora-proxy/upstream_modules`, so
    // without this crate's matching feature every expansion is a mismatch.
    if cfg!(not(feature = "upstream_modules")) {
        t.compile_fail("tests/ui/fail_upstream_modules_mismatch.rs");
        return;
    }
    t.pass("tests/ui/pass_basic.rs");
    t.pass("tests/ui/pass_lint_warn.rs");
    t.pass("tests/ui/pass_trailing_comma.rs");
    t.pass("tests/ui/pass_delegate_expr.rs");
    t.pass("tests/ui/pass_conditional_impl.rs");
    t.compile_fail("tests/ui/fail_conditional_method.rs");
    t.compile_fail("tests/ui/fail_conditional_method_attr.rs");
    t.compile_fail("tests/ui/fail_bad_attr.rs");
    t.compile_fail("tests/ui/fail_not_proxy_http.rs");
    t.compile_fail("tests/ui/fail_unknown_key.rs");
    t.compile_fail("tests/ui/fail_bad_lint_level.rs");
}
