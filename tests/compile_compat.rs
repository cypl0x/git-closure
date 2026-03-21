// Compile-path public API guard and functional tests.
//
// The accessibility tests fail to compile until the named symbols exist.
// The functional tests fail to compile (and then fail to run) until
// compile_source and CompileFormat are implemented.

// ── Accessibility guards ──────────────────────────────────────────────────────

// Fails until compile_source + CompileFormat exist:
#[test]
fn compile_source_accessible_via_public_api() {
    use git_closure::{compile_source, CompileFormat};
    let _ = compile_source;
    let _ = std::mem::size_of::<CompileFormat>();
}

// Fails until GclBackend is re-exported at crate root:
#[test]
fn gcl_backend_accessible_via_crate_root() {
    use git_closure::GclBackend;
    let _ = std::mem::size_of::<GclBackend>();
}

// ── Functional: GCL compile path ─────────────────────────────────────────────

#[test]
fn compile_local_source_produces_gcl_artifact() {
    use git_closure::providers::ProviderKind;
    use git_closure::{compile_source, parse_snapshot, CompileFormat};

    // Non-git temp dir — no git metadata, no source annotation.
    let src = tempfile::TempDir::new().unwrap();
    std::fs::write(src.path().join("hello.txt"), b"hello world\n").unwrap();

    let out = tempfile::TempDir::new().unwrap();
    let output = out.path().join("out.gcl");

    compile_source(
        src.path().to_str().unwrap(),
        &output,
        CompileFormat::Gcl,
        ProviderKind::Local,
    )
    .unwrap();

    assert!(output.exists());

    // Parse the emitted .gcl and assert the expected file entry is present.
    let text = std::fs::read_to_string(&output).unwrap();
    let (_header, files) = parse_snapshot(&text).expect("compile output must be valid .gcl");
    assert!(
        files.iter().any(|f| f.path == "hello.txt"),
        "expected hello.txt in compile output; got: {:?}",
        files.iter().map(|f| &f.path).collect::<Vec<_>>()
    );
}

// ── Functional: NAR compile path — oracle comparison ─────────────────────────

#[test]
fn compile_local_source_to_nar_matches_build_then_export_oracle() {
    use git_closure::providers::ProviderKind;
    use git_closure::{
        build_snapshot_with_options, compile_source, export_snapshot_as_nar, BuildOptions,
        CompileFormat,
    };

    // Non-git temp dir: both build and compile produce no git provenance,
    // so byte-level equality of the resulting NAR is valid.
    let src = tempfile::TempDir::new().unwrap();
    std::fs::write(src.path().join("hello.txt"), b"hello world\n").unwrap();

    let work = tempfile::TempDir::new().unwrap();
    let gcl_path = work.path().join("oracle.gcl");
    let oracle_nar = work.path().join("oracle.nar");
    let compile_nar = work.path().join("compile.nar");

    // Oracle path: build → export (existing trusted pipeline).
    build_snapshot_with_options(src.path(), &gcl_path, &BuildOptions::default()).unwrap();
    export_snapshot_as_nar(&gcl_path, &oracle_nar).unwrap();

    // New compile path: source → Closure → NarBackend.
    compile_source(
        src.path().to_str().unwrap(),
        &compile_nar,
        CompileFormat::Nar,
        ProviderKind::Local,
    )
    .unwrap();

    assert_eq!(
        std::fs::read(&oracle_nar).unwrap(),
        std::fs::read(&compile_nar).unwrap(),
        "compile --format nar must produce byte-identical NAR to build+export for the same non-git source"
    );
}
