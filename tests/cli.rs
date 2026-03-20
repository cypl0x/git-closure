use std::fs;
use std::path::{Path, PathBuf};

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

#[test]
fn cli_fixtures_do_not_use_placeholder_snapshot_hashes() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/cli");
    let mut fixture_files = Vec::new();
    collect_gcl_files(&root, &mut fixture_files);

    for path in fixture_files {
        let text = fs::read_to_string(&path).expect("read .gcl fixture");
        for line in text.lines() {
            if let Some(hash) = line.trim().strip_prefix(";; snapshot-hash:") {
                let hash = hash.trim();
                let all_same = hash
                    .as_bytes()
                    .first()
                    .is_some_and(|first| hash.as_bytes().iter().all(|b| b == first));
                assert!(
                    !(hash.len() == 64 && all_same),
                    "fixture uses placeholder snapshot-hash in {}: {hash}",
                    path.display()
                );
            }
        }
    }
}

fn collect_gcl_files(root: &Path, out: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(root).expect("read fixture directory");
    for entry in entries {
        let entry = entry.expect("read fixture entry");
        let path = entry.path();
        let file_type = entry.file_type().expect("read fixture file type");
        if file_type.is_dir() {
            collect_gcl_files(&path, out);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("gcl") {
            out.push(path);
        }
    }
}
