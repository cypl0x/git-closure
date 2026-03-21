// Public-path compatibility guard for the providers layer.
//
// Test 1 is the only true failing gate — it fails to compile before Commit 1
// because git_closure::source::SourceSpec does not exist yet.
//
// Test 2 uses paths that already exist before Commit 2; it compiles and passes
// throughout. After Commit 2 it serves as a public-path regression guard,
// ensuring no re-export is accidentally dropped during the submodule split.

// ── Commit 1: SourceSpec reachable from both public paths ─────────────────────

#[test]
fn source_spec_accessible_from_both_public_paths() {
    use git_closure::providers::SourceSpec as SourceSpecViaProviders;
    use git_closure::source::SourceSpec as SourceSpecViaSource;

    let via_source = SourceSpecViaSource::parse("gh:owner/repo").expect("parse via source");
    let via_providers =
        SourceSpecViaProviders::parse("gh:owner/repo").expect("parse via providers");

    // SourceSpec derives PartialEq; same type, same parse result.
    assert_eq!(via_source, via_providers);
}

// ── Post-Phase-4: Provider trait reachable via providers:: ────────────────────

#[test]
fn provider_trait_reachable_via_providers_path() {
    use git_closure::providers::{LocalProvider, Provider};

    // Monomorphisation probe: the compiler must resolve Provider as a trait and
    // LocalProvider as a conforming type at their public paths.  If either
    // re-export is dropped, this fails to compile.
    fn assert_provider_impl<T: Provider>() {}
    assert_provider_impl::<LocalProvider>();
}

// ── Commit 2: concrete provider structs still reachable via providers:: ────────

#[test]
fn provider_symbols_reachable_via_providers_path() {
    use git_closure::providers::{
        FetchedSource, GitCloneProvider, GithubApiProvider, LocalProvider, NixProvider,
        ProviderKind,
    };

    // size_of probes: each line forces full type resolution of the named symbol.
    // If any re-export is dropped from providers/mod.rs, the import above fails
    // to compile and the test fails — intent is unmistakable.
    let _ = std::mem::size_of::<FetchedSource>();
    let _ = std::mem::size_of::<GithubApiProvider>();
    let _ = std::mem::size_of::<GitCloneProvider>();
    let _ = std::mem::size_of::<LocalProvider>();
    let _ = std::mem::size_of::<NixProvider>();
    let _ = std::mem::size_of::<ProviderKind>();

    // Function item binding: proves fetch_source resolves as a named callable
    // at the expected public path without invoking it.
    let _ = git_closure::providers::fetch_source;
}
