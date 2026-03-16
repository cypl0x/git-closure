#[test]
fn cli_contracts() {
    trycmd::TestCases::new()
        .case("tests/cli/*.toml")
        .case("tests/cli/*.trycmd")
        .case("tests/cli/README/*.toml")
        .case("tests/cli/README/*.trycmd");
}
