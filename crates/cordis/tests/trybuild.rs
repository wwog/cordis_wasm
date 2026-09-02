#[test]
fn component_macro_diagnostics() {
    let tests = trybuild::TestCases::new();
    tests.pass("tests/ui-pass/*.rs");
    tests.compile_fail("tests/ui/*.rs");
}
