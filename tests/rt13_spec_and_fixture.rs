use std::fs;
use std::process::Command;

use tempfile::TempDir;

#[test]
fn spec_hash_section_matches_big_endian_reference() {
    let spec =
        fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/SPEC.md")).expect("read SPEC.md");

    assert!(
        spec.contains("64-bit big-endian") || spec.contains("64-bit **big-endian**"),
        "SPEC must describe length prefix as big-endian"
    );
    assert!(
        spec.contains("to_be_bytes()"),
        "SPEC reference implementation must use to_be_bytes"
    );
    assert!(
        !spec.contains("to_le_bytes()"),
        "SPEC must not claim little-endian hashing"
    );
}

#[test]
fn rt13_golden_fixture_hash_matches_expected_value() {
    let fixture_root = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/rt13/minimal-src"
    );
    let expected_hash_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/rt13/expected-snapshot-hash.txt"
    );

    let expected = fs::read_to_string(expected_hash_path)
        .expect("read expected hash")
        .trim()
        .to_string();
    let work = TempDir::new().expect("create tempdir");
    let source = work.path().join("src");
    fs::create_dir_all(&source).expect("create source dir");
    fs::copy(
        format!("{fixture_root}/alpha.txt"),
        source.join("alpha.txt"),
    )
    .expect("copy fixture file");
    let output = work.path().join("snapshot.gcl");

    let status = Command::new(env!("CARGO_BIN_EXE_git-closure"))
        .arg("build")
        .arg(&source)
        .arg("--output")
        .arg(&output)
        .status()
        .expect("run git-closure build");
    assert!(status.success(), "git-closure build must succeed");

    let text = fs::read_to_string(&output).expect("read generated snapshot");
    let got = text
        .lines()
        .find_map(|line| line.strip_prefix(";; snapshot-hash:"))
        .map(str::trim)
        .expect("snapshot-hash header present")
        .to_string();
    assert_eq!(got, expected, "golden fixture snapshot hash drifted");
}
