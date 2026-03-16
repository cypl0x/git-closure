#[test]
fn cli_contracts() {
    trycmd::TestCases::new()
        .case("tests/cli/*.toml")
        .case("tests/cli/*.trycmd");
}
