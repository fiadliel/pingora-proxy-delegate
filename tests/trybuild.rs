#[test]
fn ui() {
    let t = trybuild::TestCases::new();
    t.pass("tests/ui/pass_basic.rs");
    t.pass("tests/ui/pass_lint_warn.rs");
    t.pass("tests/ui/pass_trailing_comma.rs");
    t.pass("tests/ui/pass_ctx_delegate.rs");
    t.compile_fail("tests/ui/fail_bad_attr.rs");
    t.compile_fail("tests/ui/fail_not_self.rs");
    t.compile_fail("tests/ui/fail_not_proxy_http.rs");
    t.compile_fail("tests/ui/fail_unknown_key.rs");
    t.compile_fail("tests/ui/fail_bad_lint_level.rs");
}
