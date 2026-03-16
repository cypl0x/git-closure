use std::fs;
use std::path::Path;

use git_closure::{build_snapshot, render_snapshot, RenderFormat};
use tempfile::TempDir;

#[cfg(unix)]
use std::os::unix::fs::{symlink, PermissionsExt};

#[cfg(unix)]
fn copy_tree_preserving_symlinks_and_modes(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let file_type = entry.file_type()?;

        if file_type.is_dir() {
            copy_tree_preserving_symlinks_and_modes(&src_path, &dst_path)?;
            continue;
        }

        if file_type.is_symlink() {
            let target = fs::read_link(&src_path)?;
            symlink(target, &dst_path)?;
            continue;
        }

        fs::copy(&src_path, &dst_path)?;
        let src_mode = fs::symlink_metadata(&src_path)?.permissions().mode() & 0o777;
        let mut perms = fs::symlink_metadata(&dst_path)?.permissions();
        perms.set_mode(src_mode);
        fs::set_permissions(&dst_path, perms)?;
    }

    Ok(())
}

#[cfg(unix)]
#[test]
fn td07_simple_tree_build_matches_golden_snapshot_bytes() {
    let fixture_src = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/simple"
    ));
    let expected_snapshot = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/simple.gcl"
    ))
    .expect("read golden simple.gcl fixture");

    let tmp = TempDir::new().expect("create temp workspace");
    let source = tmp.path().join("simple-src");
    copy_tree_preserving_symlinks_and_modes(fixture_src, &source)
        .expect("copy fixture tree to temp workspace");

    let output = tmp.path().join("actual.gcl");
    build_snapshot(&source, &output).expect("build snapshot from copied fixture tree");

    let actual_snapshot = fs::read_to_string(&output).expect("read built snapshot");
    assert_eq!(
        actual_snapshot, expected_snapshot,
        "golden snapshot bytes drifted for tests/fixtures/simple"
    );
}

#[test]
fn td07_render_json_matches_golden_fixture() {
    let snapshot = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/simple.gcl"
    ));
    let expected_json = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/simple.render.json"
    ))
    .expect("read golden render json fixture");

    let actual_json =
        render_snapshot(snapshot, RenderFormat::Json).expect("render snapshot as JSON");
    assert_eq!(
        actual_json, expected_json,
        "render --format json output drifted from golden fixture"
    );
}
