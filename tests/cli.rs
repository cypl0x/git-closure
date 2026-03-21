#[test]
fn cli_contracts() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cli_toml = root.join("tests/cli/*.toml");
    let cli_trycmd = root.join("tests/cli/*.trycmd");
    let readme_toml = root.join("tests/cli/README/*.toml");
    let readme_trycmd = root.join("tests/cli/README/*.trycmd");

    trycmd::TestCases::new()
        .case(cli_toml.to_string_lossy().as_ref())
        .case(cli_trycmd.to_string_lossy().as_ref())
        .case(readme_toml.to_string_lossy().as_ref())
        .case(readme_trycmd.to_string_lossy().as_ref());
}
