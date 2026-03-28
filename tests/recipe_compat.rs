// Recipe public API guard and functional tests.
//
// All tests in this file fail to compile until git_closure::Recipe and
// git_closure::recipe exist. The functional test also fails to run until
// from_file, from_str, and execute are fully implemented.

// ── Accessibility guard ───────────────────────────────────────────────────────

// Fails until Recipe is re-exported at crate root:
#[test]
fn recipe_accessible_via_public_api() {
    let _ = std::mem::size_of::<git_closure::Recipe>();
}

// ── Parsing / defaults ────────────────────────────────────────────────────────

// Fails until from_str exists and RecipeFormat/RecipeProvider are accessible:
#[test]
fn recipe_parses_minimal_toml_and_asserts_defaults() {
    use git_closure::recipe::{self, RecipeFormat, RecipeProvider};

    let text = r#"
        source = "gh:owner/repo"
        output = "snapshot.gcl"
    "#;
    let r = recipe::from_str(text).expect("minimal recipe must parse");
    assert_eq!(r.source, "gh:owner/repo");
    assert_eq!(r.output, "snapshot.gcl");
    assert_eq!(
        r.format,
        RecipeFormat::Gcl,
        "omitted format must default to gcl"
    );
    assert_eq!(
        r.provider,
        RecipeProvider::Auto,
        "omitted provider must default to auto"
    );
}

// Unknown fields must be rejected, not silently ignored.
#[test]
fn recipe_rejects_unknown_fields() {
    use git_closure::recipe;
    let text = r#"
        source   = "."
        output   = "out.gcl"
        provdier = "local"
    "#;
    assert!(
        recipe::from_str(text).is_err(),
        "from_str must return an error when an unknown field is present"
    );
}

// ── Path resolution semantics ─────────────────────────────────────────────────

// Proves that relative paths are resolved against the recipe file's parent
// directory, NOT the caller's CWD. This is the core durability guarantee.
#[test]
fn recipe_paths_resolve_relative_to_recipe_file_not_cwd() {
    use git_closure::{parse_snapshot, recipe};

    let root = tempfile::TempDir::new().unwrap();
    let project = root.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let src_dir = project.join("src");
    std::fs::create_dir(&src_dir).unwrap();
    std::fs::write(src_dir.join("a.txt"), b"hello world\n").unwrap();

    std::fs::write(
        project.join("recipe.toml"),
        b"source = \"src\"\noutput = \"out.gcl\"\n",
    )
    .unwrap();

    // Execute from root/ — CWD is intentionally wrong for a CWD-relative impl.
    let r = recipe::from_file(&project.join("recipe.toml")).unwrap();
    recipe::execute(&r).unwrap();

    // Output must land in project/out.gcl, NOT root/out.gcl.
    assert!(
        project.join("out.gcl").exists(),
        "output must be in project/ (recipe-relative), not in the caller's CWD"
    );
    assert!(
        !root.path().join("out.gcl").exists(),
        "output must NOT land in the caller's CWD"
    );

    let gcl = std::fs::read_to_string(project.join("out.gcl")).unwrap();
    let (_header, files) = parse_snapshot(&gcl).expect("recipe output must be valid .gcl");
    assert!(files.iter().any(|f| f.path == "a.txt"));
}

// Locks that from_file() does not rewrite remote source syntaxes.
#[test]
fn recipe_from_file_preserves_remote_source_unchanged() {
    use git_closure::recipe;

    let dir = tempfile::TempDir::new().unwrap();
    let recipe_path = dir.path().join("recipe.toml");
    std::fs::write(
        &recipe_path,
        b"source = \"gh:owner/repo\"\noutput = \"out.gcl\"\n",
    )
    .unwrap();

    let r = recipe::from_file(&recipe_path).unwrap();

    assert_eq!(
        r.source, "gh:owner/repo",
        "from_file must not rewrite gh: sources"
    );

    let output_path = std::path::Path::new(&r.output);
    assert!(
        output_path.is_absolute(),
        "output must be an absolute path after from_file()"
    );
    assert!(
        r.output.ends_with("out.gcl"),
        "output filename must be preserved"
    );
    assert_eq!(
        output_path.parent().unwrap(),
        dir.path(),
        "output must resolve to the recipe file's directory"
    );
}

// path: ref with a relative payload is resolved recipe-file-relative.
#[test]
fn recipe_from_file_resolves_path_prefix_source() {
    use git_closure::recipe;

    let dir = tempfile::TempDir::new().unwrap();
    let recipe_path = dir.path().join("recipe.toml");
    std::fs::write(
        &recipe_path,
        b"source = \"path:./flake\"\noutput = \"out.gcl\"\n",
    )
    .unwrap();

    let r = recipe::from_file(&recipe_path).unwrap();

    let expected = format!("path:{}", dir.path().join("flake").display());
    assert_eq!(
        r.source, expected,
        "path: source must be resolved recipe-file-relative"
    );

    assert!(
        std::path::Path::new(&r.output).is_absolute(),
        "output must be absolute after from_file()"
    );
    assert_eq!(
        std::path::Path::new(&r.output).parent().unwrap(),
        dir.path(),
        "output must resolve to the recipe file's directory"
    );
}

// nix:path: ref with a relative payload is resolved recipe-file-relative.
#[test]
fn recipe_from_file_resolves_nix_path_source() {
    use git_closure::recipe;

    let dir = tempfile::TempDir::new().unwrap();
    let recipe_path = dir.path().join("recipe.toml");
    std::fs::write(
        &recipe_path,
        b"source = \"nix:path:./flake\"\noutput = \"out.gcl\"\n",
    )
    .unwrap();

    let r = recipe::from_file(&recipe_path).unwrap();

    let expected = format!("nix:path:{}", dir.path().join("flake").display());
    assert_eq!(
        r.source, expected,
        "nix:path: source must be resolved recipe-file-relative"
    );

    assert!(
        std::path::Path::new(&r.output).is_absolute(),
        "output must be absolute after from_file()"
    );
}

// A .git suffix alone does not make a source remote. Local bare-repo paths
// like ./repo.git must be resolved recipe-file-relative.
#[test]
fn recipe_from_file_resolves_local_git_suffix_source() {
    use git_closure::recipe;

    let dir = tempfile::TempDir::new().unwrap();
    let recipe_path = dir.path().join("recipe.toml");
    std::fs::write(
        &recipe_path,
        b"source = \"./repo.git\"\noutput = \"out.gcl\"\n",
    )
    .unwrap();

    let r = recipe::from_file(&recipe_path).unwrap();

    let expected = dir.path().join("repo.git").to_string_lossy().into_owned();
    assert_eq!(
        r.source, expected,
        "./repo.git must be resolved recipe-file-relative"
    );
    assert!(
        std::path::Path::new(&r.source).is_absolute(),
        "resolved source must be an absolute path"
    );
}

// file+ local-path forms are rejected with a clear error in Phase 6.
#[test]
fn recipe_from_file_rejects_file_plus_source() {
    use git_closure::recipe;

    let dir = tempfile::TempDir::new().unwrap();
    let recipe_path = dir.path().join("recipe.toml");
    std::fs::write(
        &recipe_path,
        b"source = \"file+./local\"\noutput = \"out.gcl\"\n",
    )
    .unwrap();

    let err =
        recipe::from_file(&recipe_path).expect_err("file+ local-path source must be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("file+"),
        "error must mention file+ syntax: {msg}"
    );
    assert!(
        msg.contains("Phase 6") || msg.contains("not supported"),
        "error must indicate unsupported status: {msg}"
    );
}

// ── Phase 7: mode field ───────────────────────────────────────────────────────

// Fails until RecipeMode is pub in git_closure::recipe and Recipe has a `mode` field.
#[test]
fn recipe_mode_defaults_to_compile() {
    use git_closure::recipe::{self, RecipeMode};

    let text = r#"
        source = "gh:owner/repo"
        output = "snapshot.gcl"
    "#;
    let r = recipe::from_str(text).expect("minimal recipe must parse");
    assert_eq!(
        r.mode,
        RecipeMode::Compile,
        "omitted mode must default to compile"
    );
}

// Fails until RecipeMode::Build is a recognised serde variant.
#[test]
fn recipe_mode_parses_build_variant() {
    use git_closure::recipe::{self, RecipeMode};

    let text = r#"
        source = "gh:owner/repo"
        output = "snapshot.gcl"
        mode   = "build"
    "#;
    let r = recipe::from_str(text).expect("mode=build must parse");
    assert_eq!(r.mode, RecipeMode::Build);
}

// Fails until the serde layer rejects unknown enum variants.
#[test]
fn recipe_rejects_unknown_mode_value() {
    use git_closure::recipe;

    let text = r#"
        source = "."
        output = "out.gcl"
        mode   = "fast"
    "#;
    assert!(
        recipe::from_str(text).is_err(),
        "unknown mode variant must be rejected"
    );
}

// Fails until execute() validates the build+nar combination.
// The error must fire before any I/O (source="/tmp" is never accessed).
#[test]
fn recipe_build_mode_with_nar_format_is_validation_error() {
    use git_closure::recipe;

    let text = r#"
        source = "/tmp"
        output = "/tmp/out.nar"
        mode   = "build"
        format = "nar"
    "#;
    let r = recipe::from_str(text).expect("recipe must parse");
    let err = recipe::execute(&r).expect_err("build+nar must be a validation error");
    let msg = err.to_string();
    assert!(
        msg.contains("build mode") || msg.contains("nar"),
        "error message must mention the constraint: {msg}"
    );
}

// Integration: build mode routes to the git-aware build path.
// For a real git repo, git_rev must be present in the output header.
#[test]
fn recipe_build_mode_routes_to_build_path_and_records_git_rev() {
    use git_closure::{parse_snapshot, recipe};
    use std::process::Command;

    let root = tempfile::TempDir::new().unwrap();
    let src = root.path();

    Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(src)
        .output()
        .expect("git init");
    Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(src)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(src)
        .output()
        .unwrap();
    std::fs::write(src.join("hello.txt"), b"hello\n").unwrap();
    Command::new("git")
        .args(["add", "."])
        .current_dir(src)
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(src)
        .output()
        .expect("git commit");

    let out_dir = tempfile::TempDir::new().unwrap();
    let output = out_dir.path().join("out.gcl");
    let text = format!(
        "source = {:?}\noutput = {:?}\nmode = \"build\"\n",
        src.to_str().unwrap(),
        output.to_str().unwrap(),
    );
    let r = recipe::from_str(&text).unwrap();
    recipe::execute(&r).unwrap();

    assert!(output.exists(), "output .gcl must be created");
    let gcl = std::fs::read_to_string(&output).unwrap();
    let (header, _files) = parse_snapshot(&gcl).expect("build-mode output must be valid .gcl");
    assert!(
        header.git_rev.is_some(),
        "build mode must record git-rev in the snapshot header for a real git repo"
    );
}

// ── Phase 8: Manifest accessibility guards ────────────────────────────────────

// Fails until Manifest is re-exported at crate root
#[test]
fn manifest_accessible_via_crate_root() {
    let _ = std::mem::size_of::<git_closure::Manifest>();
}

// Fails until manifest_from_str is pub in git_closure::recipe
#[test]
fn manifest_from_str_accessible_via_recipe_module() {
    use git_closure::recipe;
    // empty string is an invalid manifest — what matters is the function is reachable
    let _ = recipe::manifest_from_str("");
}

// Fails until manifest_from_file is pub in git_closure::recipe
#[test]
fn manifest_from_file_accessible_via_recipe_module() {
    use git_closure::recipe;
    use std::path::Path;
    // nonexistent path → Err; what matters is the function is reachable
    let _ = recipe::manifest_from_file(Path::new("/nonexistent/path/manifest.toml"));
}

// ── Phase 8: Manifest type and named targets ──────────────────────────────────

#[test]
fn manifest_parses_multi_target_toml() {
    use git_closure::recipe;
    let text = r#"
        [targets.dev]
        source = "."
        output = "dev.gcl"

        [targets.release]
        source = "gh:owner/repo"
        output = "release.gcl"
    "#;
    let m = recipe::manifest_from_str(text).expect("multi-target manifest must parse");
    assert!(m.targets.contains_key("dev"), "dev target must be present");
    assert!(
        m.targets.contains_key("release"),
        "release target must be present"
    );
    assert_eq!(m.targets.len(), 2);
    assert_eq!(m.targets["dev"].output, "dev.gcl");
    assert_eq!(m.targets["release"].source, "gh:owner/repo");
}

#[test]
fn manifest_selects_default_target_when_none_specified() {
    use git_closure::recipe;
    let text = r#"
        default_target = "dev"

        [targets.dev]
        source = "."
        output = "dev.gcl"

        [targets.release]
        source = "gh:owner/repo"
        output = "release.gcl"
    "#;
    let m = recipe::manifest_from_str(text).expect("manifest must parse");
    let r = m
        .select(None)
        .expect("select(None) must pick default target");
    assert_eq!(r.output, "dev.gcl");
}

#[test]
fn manifest_selects_named_target() {
    use git_closure::recipe;
    let text = r#"
        [targets.dev]
        source = "."
        output = "dev.gcl"

        [targets.release]
        source = "gh:owner/repo"
        output = "release.gcl"
    "#;
    let m = recipe::manifest_from_str(text).expect("manifest must parse");
    let r = m
        .select(Some("release"))
        .expect("select(Some(\"release\")) must succeed");
    assert_eq!(r.source, "gh:owner/repo");
}

#[test]
fn manifest_error_multiple_targets_no_default_requires_flag() {
    use git_closure::recipe;
    let text = r#"
        [targets.dev]
        source = "."
        output = "dev.gcl"

        [targets.release]
        source = "gh:owner/repo"
        output = "release.gcl"
    "#;
    let m = recipe::manifest_from_str(text).expect("manifest must parse");
    let err = m
        .select(None)
        .expect_err("select(None) must fail when no default and multiple targets");
    let msg = err.to_string();
    assert!(
        msg.contains("--target"),
        "error must mention --target: {msg}"
    );
}

#[test]
fn manifest_error_unknown_target_name_lists_available_in_sorted_order() {
    use git_closure::recipe;
    let text = r#"
        [targets.release]
        source = "gh:owner/repo"
        output = "release.gcl"

        [targets.dev]
        source = "."
        output = "dev.gcl"

        [targets.test]
        source = "./tests"
        output = "test.gcl"
    "#;
    let m = recipe::manifest_from_str(text).expect("manifest must parse");
    let err = m
        .select(Some("missing"))
        .expect_err("unknown target must be an error");
    let msg = err.to_string();
    // BTreeMap guarantees sorted order: dev, release, test
    assert!(
        msg.contains("dev, release, test"),
        "error must list available targets in sorted order: {msg}"
    );
}

#[test]
fn manifest_error_invalid_default_target() {
    use git_closure::recipe;
    let text = r#"
        default_target = "missing"

        [targets.dev]
        source = "."
        output = "dev.gcl"
    "#;
    let m = recipe::manifest_from_str(text)
        .expect("manifest_from_str must succeed even with invalid default_target");
    let err = m
        .select(None)
        .expect_err("select(None) must fail when default_target does not exist");
    let msg = err.to_string();
    assert!(
        msg.contains("missing") && (msg.contains("not defined") || msg.contains("targets")),
        "error must mention the missing default_target name: {msg}"
    );
}

#[test]
fn manifest_rejects_unknown_top_level_field() {
    use git_closure::recipe;
    // "default_targte" is a misspelling of "default_target"
    let text = r#"
        default_targte = "dev"

        [targets.dev]
        source = "."
        output = "out.gcl"
    "#;
    assert!(
        recipe::manifest_from_str(text).is_err(),
        "unknown top-level manifest field must be rejected"
    );
}

#[test]
fn manifest_rejects_mixed_format() {
    use git_closure::recipe;
    let text = r#"
        source = "."
        output = "out.gcl"

        [targets.dev]
        source = "."
        output = "dev.gcl"
    "#;
    assert!(
        recipe::manifest_from_str(text).is_err(),
        "mixing top-level source/output with [targets.*] must be rejected"
    );
}

#[test]
fn manifest_single_target_auto_selected_without_default() {
    use git_closure::recipe;
    let text = r#"
        [targets.dev]
        source = "."
        output = "dev.gcl"
    "#;
    let m = recipe::manifest_from_str(text).expect("single-target manifest must parse");
    let r = m
        .select(None)
        .expect("single target must be auto-selected without default_target");
    assert_eq!(r.output, "dev.gcl");
}

#[test]
fn manifest_legacy_flat_file_is_backward_compatible() {
    use git_closure::{parse_snapshot, recipe};
    let src = tempfile::TempDir::new().unwrap();
    std::fs::write(src.path().join("hello.txt"), b"hello\n").unwrap();
    let out = tempfile::TempDir::new().unwrap();
    let output = out.path().join("out.gcl");
    // Legacy flat format (Phase 6/7 style)
    let text = format!(
        "source = {:?}\noutput = {:?}\n",
        src.path().to_str().unwrap(),
        output.to_str().unwrap(),
    );
    let m = recipe::manifest_from_str(&text).expect("legacy flat file must parse as manifest");
    assert_eq!(
        m.targets.len(),
        1,
        "legacy file must produce exactly one target"
    );
    let r = m
        .select(None)
        .expect("legacy single-target manifest must auto-select");
    recipe::execute(r).expect("legacy manifest must execute");
    assert!(output.exists(), "output must be created");
    let gcl = std::fs::read_to_string(&output).unwrap();
    let (_header, files) = parse_snapshot(&gcl).expect("output must be valid .gcl");
    assert!(files.iter().any(|f| f.path == "hello.txt"));
}

#[test]
fn manifest_from_file_resolves_paths_per_target() {
    use git_closure::recipe;
    let dir = tempfile::TempDir::new().unwrap();
    let manifest_path = dir.path().join("manifest.toml");
    std::fs::write(
        &manifest_path,
        b"[targets.dev]\nsource = \"src\"\noutput = \"out/dev.gcl\"\n\
          [targets.release]\nsource = \"gh:owner/repo\"\noutput = \"out/release.gcl\"\n",
    )
    .unwrap();
    let m = recipe::manifest_from_file(&manifest_path).expect("manifest_from_file must succeed");
    let dev = m
        .select(Some("dev"))
        .expect("dev target must be selectable");
    assert!(
        std::path::Path::new(&dev.output).is_absolute(),
        "dev output must be absolute after manifest_from_file()"
    );
    assert!(
        dev.output.ends_with("dev.gcl"),
        "dev output filename must be preserved"
    );
    assert!(
        std::path::Path::new(&dev.source).is_absolute(),
        "dev source must be resolved to absolute path"
    );
    // Remote source must be preserved
    let release = m
        .select(Some("release"))
        .expect("release target must be selectable");
    assert_eq!(
        release.source, "gh:owner/repo",
        "remote source must not be rewritten"
    );
}

#[test]
fn manifest_executes_selected_target_end_to_end() {
    use git_closure::{parse_snapshot, recipe};
    let src = tempfile::TempDir::new().unwrap();
    std::fs::write(src.path().join("data.txt"), b"phase8\n").unwrap();
    let out = tempfile::TempDir::new().unwrap();
    let output = out.path().join("out.gcl");
    let text = format!(
        "[targets.snap]\nsource = {:?}\noutput = {:?}\n",
        src.path().to_str().unwrap(),
        output.to_str().unwrap(),
    );
    let m = recipe::manifest_from_str(&text).expect("manifest must parse");
    let r = m
        .select(Some("snap"))
        .expect("snap target must be selectable");
    recipe::execute(r).expect("execute must succeed");
    assert!(output.exists(), "output must be created");
    let gcl = std::fs::read_to_string(&output).unwrap();
    let (_header, files) = parse_snapshot(&gcl).expect("output must be valid .gcl");
    assert!(files.iter().any(|f| f.path == "data.txt"));
}

// ── Functional ────────────────────────────────────────────────────────────────

// Functional: recipe executes compile path end-to-end; checks artifact content.
#[test]
fn recipe_executes_compile_to_gcl() {
    use git_closure::{parse_snapshot, recipe};

    let src = tempfile::TempDir::new().unwrap();
    std::fs::write(src.path().join("hello.txt"), b"hello world\n").unwrap();

    let out = tempfile::TempDir::new().unwrap();
    let output = out.path().join("out.gcl");

    // Use absolute paths (from_str, no resolution needed).
    let text = format!(
        "source = {:?}\noutput = {:?}\n",
        src.path().to_str().unwrap(),
        output.to_str().unwrap(),
    );
    let r = recipe::from_str(&text).unwrap();
    recipe::execute(&r).unwrap();

    assert!(output.exists());
    let gcl = std::fs::read_to_string(&output).unwrap();
    let (_header, files) = parse_snapshot(&gcl).expect("recipe output must be valid .gcl");
    let entry = files
        .iter()
        .find(|f| f.path == "hello.txt")
        .expect("hello.txt must be present in recipe output");
    assert_eq!(entry.content, b"hello world\n");
}

// ── Phase 9: targets command data surface ─────────────────────────────────────

#[test]
fn manifest_targets_iter_is_sorted_and_exposes_mode_and_format() {
    use git_closure::recipe::{self, RecipeFormat, RecipeMode};
    let text = r#"
        default_target = "dev"
        [targets.release]
        source = "gh:owner/repo"
        output = "release.gcl"
        mode   = "build"
        [targets.dev]
        source = "."
        output = "dev.gcl"
        [targets.bundle]
        source = "."
        output = "bundle.nar"
        format = "nar"
    "#;
    let m = recipe::manifest_from_str(text).unwrap();
    let names: Vec<&str> = m.targets.keys().map(|s| s.as_str()).collect();
    assert_eq!(
        names,
        vec!["bundle", "dev", "release"],
        "targets must be sorted (BTreeMap)"
    );
    assert_eq!(m.targets["release"].mode, RecipeMode::Build);
    assert_eq!(m.targets["bundle"].format, RecipeFormat::Nar);
    assert_eq!(m.default_target.as_deref(), Some("dev"));
}
