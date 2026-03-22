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
    assert_eq!(r.format, RecipeFormat::Gcl, "omitted format must default to gcl");
    assert_eq!(r.provider, RecipeProvider::Auto, "omitted provider must default to auto");
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

    assert_eq!(r.source, "gh:owner/repo", "from_file must not rewrite gh: sources");

    let output_path = std::path::Path::new(&r.output);
    assert!(output_path.is_absolute(), "output must be an absolute path after from_file()");
    assert!(r.output.ends_with("out.gcl"), "output filename must be preserved");
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
    assert_eq!(r.source, expected, "path: source must be resolved recipe-file-relative");

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
    assert_eq!(r.source, expected, "nix:path: source must be resolved recipe-file-relative");

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
    assert_eq!(r.source, expected, "./repo.git must be resolved recipe-file-relative");
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

    let err = recipe::from_file(&recipe_path)
        .expect_err("file+ local-path source must be rejected");
    let msg = err.to_string();
    assert!(msg.contains("file+"), "error must mention file+ syntax: {msg}");
    assert!(
        msg.contains("Phase 6") || msg.contains("not supported"),
        "error must indicate unsupported status: {msg}"
    );
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
