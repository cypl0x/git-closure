//! GitHub API provider.

use std::fs;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};

use flate2::read::GzDecoder;
use tempfile::TempDir;

use crate::error::GitClosureError;
use crate::source::SourceSpec;
use crate::utils::{ensure_no_symlink_ancestors, lexical_normalize, reject_if_symlink};

use super::{FetchedSource, Provider, Result};

const GITHUB_API_BASE: &str = "https://api.github.com/repos";
const GITHUB_TOKEN_ENV: &str = "GCL_GITHUB_TOKEN";
const GITHUB_TARBALL_MAX_BYTES_ENV: &str = "GCL_GITHUB_TARBALL_MAX_BYTES";
/// Default maximum tarball size accepted from github-api downloads.
///
/// 512 MiB is large enough for substantial repositories while preventing
/// unbounded memory growth when reading response bodies from untrusted
/// network endpoints.
const DEFAULT_GITHUB_TARBALL_MAX_BYTES: u64 = 512 * 1024 * 1024;

pub struct GithubApiProvider;

impl Provider for GithubApiProvider {
    fn fetch(&self, source: &str) -> Result<FetchedSource> {
        let parsed = parse_github_api_source(source)?;
        let max_bytes = github_tarball_max_bytes()?;
        let tarball = download_github_tarball(&parsed, max_bytes)?;

        let tempdir = TempDir::new()?;
        let checkout = tempdir.path().join("repo");
        fs::create_dir_all(&checkout)?;

        extract_github_tarball(&tarball, &checkout)?;
        Ok(FetchedSource::temporary(checkout, tempdir))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ParsedGithubApiSource {
    pub(super) owner: String,
    pub(super) repo: String,
    pub(super) reference: Option<String>,
}

impl ParsedGithubApiSource {
    fn archive_url(&self) -> String {
        let reference = self.reference.as_deref().unwrap_or("HEAD");
        format!(
            "{GITHUB_API_BASE}/{}/{}/tarball/{reference}",
            self.owner, self.repo
        )
    }

    fn display_name(&self) -> String {
        let reference = self.reference.as_deref().unwrap_or("HEAD");
        format!("{}/{}@{reference}", self.owner, self.repo)
    }
}

pub(super) fn parse_github_api_source(source: &str) -> Result<ParsedGithubApiSource> {
    match SourceSpec::parse(source)? {
        SourceSpec::GitHubRepo {
            owner,
            repo,
            reference,
        } => Ok(ParsedGithubApiSource {
            owner,
            repo,
            reference,
        }),
        _ => Err(GitClosureError::Parse(format!(
            "github-api provider requires a GitHub source (gh:owner/repo[@ref] or https://github.com/owner/repo[@ref]); got: {source}"
        ))),
    }
}

fn download_github_tarball(source: &ParsedGithubApiSource, max_bytes: u64) -> Result<Vec<u8>> {
    let url = source.archive_url();
    let token = std::env::var(GITHUB_TOKEN_ENV)
        .ok()
        .filter(|v| !v.is_empty());
    download_tarball_url(&url, &source.display_name(), token.as_deref(), max_bytes)
}

fn download_tarball_url(
    url: &str,
    source_name: &str,
    token: Option<&str>,
    max_bytes: u64,
) -> Result<Vec<u8>> {
    let agent = ureq::builder().build();
    let mut request = agent
        .get(url)
        .set("Accept", "application/vnd.github+json")
        .set("User-Agent", "git-closure");
    if let Some(token) = token {
        request = request.set("Authorization", &format!("Bearer {token}"));
    }

    match request.call() {
        Ok(response) => {
            if let Some(content_length_header) = response.header("Content-Length") {
                if let Ok(content_length) = content_length_header.trim().parse::<u64>() {
                    if content_length > max_bytes {
                        return Err(GitClosureError::Parse(format!(
                            "github-api: tarball download for {source_name} exceeds limit {max_bytes} bytes (Content-Length: {content_length})",
                        )));
                    }
                }
            }

            let mut body = Vec::new();
            let mut reader = response.into_reader();
            let mut total_read = 0u64;
            let mut chunk = [0u8; 8192];
            loop {
                let read = reader.read(&mut chunk).map_err(|err| {
                    GitClosureError::Parse(format!(
                        "github-api: failed to read tarball response for {source_name}: {err}",
                    ))
                })?;
                if read == 0 {
                    break;
                }
                total_read = total_read.saturating_add(read as u64);
                if total_read > max_bytes {
                    return Err(GitClosureError::Parse(format!(
                        "github-api: tarball download for {source_name} exceeded limit {max_bytes} bytes (read {total_read} bytes)",
                    )));
                }
                body.extend_from_slice(&chunk[..read]);
            }
            Ok(body)
        }
        Err(ureq::Error::Status(status, response)) => {
            let rate_remaining = response.header("X-RateLimit-Remaining").map(str::to_string);
            let body = response.into_string().unwrap_or_default();
            Err(github_api_status_error(
                status,
                rate_remaining.as_deref(),
                source_name,
                &body,
            ))
        }
        Err(ureq::Error::Transport(err)) => Err(GitClosureError::Parse(format!(
            "github-api: request failed for {source_name}: {err}",
        ))),
    }
}

fn github_tarball_max_bytes() -> Result<u64> {
    match std::env::var(GITHUB_TARBALL_MAX_BYTES_ENV) {
        Ok(raw) => {
            let raw = raw.trim();
            if raw.is_empty() {
                return Ok(DEFAULT_GITHUB_TARBALL_MAX_BYTES);
            }
            let parsed = raw.parse::<u64>().map_err(|err| {
                GitClosureError::Parse(format!(
                    "github-api: invalid {GITHUB_TARBALL_MAX_BYTES_ENV} value {raw:?}: {err}"
                ))
            })?;
            if parsed == 0 {
                return Err(GitClosureError::Parse(format!(
                    "github-api: invalid {GITHUB_TARBALL_MAX_BYTES_ENV} value {raw:?}: must be > 0"
                )));
            }
            Ok(parsed)
        }
        Err(std::env::VarError::NotPresent) => Ok(DEFAULT_GITHUB_TARBALL_MAX_BYTES),
        Err(std::env::VarError::NotUnicode(_)) => Err(GitClosureError::Parse(format!(
            "github-api: invalid {GITHUB_TARBALL_MAX_BYTES_ENV}: value is not valid UTF-8"
        ))),
    }
}

pub(super) fn github_api_status_error(
    status: u16,
    rate_remaining: Option<&str>,
    source_name: &str,
    body: &str,
) -> GitClosureError {
    let body_summary = body.trim();
    let suffix = if body_summary.is_empty() {
        String::new()
    } else {
        format!(": {body_summary}")
    };

    match status {
        401 => GitClosureError::Parse(format!(
            "github-api: authentication failed for {source_name} (HTTP 401). Set {GITHUB_TOKEN_ENV}."
        )),
        403 if rate_remaining == Some("0") => GitClosureError::Parse(format!(
            "github-api: rate limit exceeded while downloading {source_name}. Set {GITHUB_TOKEN_ENV} for higher limits."
        )),
        404 => GitClosureError::Parse(format!(
            "github-api: repository or reference not found: {source_name}"
        )),
        _ => GitClosureError::Parse(format!(
            "github-api: request failed for {source_name} with HTTP {status}{suffix}"
        )),
    }
}

pub(super) fn extract_github_tarball(bytes: &[u8], destination: &Path) -> Result<()> {
    let decoder = GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);
    let mut top_level: Option<std::ffi::OsString> = None;

    for entry_result in archive.entries().map_err(|err| {
        GitClosureError::Parse(format!("github-api: failed to read tar entries: {err}"))
    })? {
        let mut entry = entry_result.map_err(|err| {
            GitClosureError::Parse(format!("github-api: invalid tar entry: {err}"))
        })?;
        let entry_path = entry.path().map_err(|err| {
            GitClosureError::Parse(format!("github-api: invalid tar path entry: {err}"))
        })?;
        let relative = strip_github_archive_prefix(entry_path.as_ref(), &mut top_level)?;
        let Some(relative) = relative else {
            continue;
        };

        let output_path = destination.join(&relative);
        if let Some(parent) = output_path.parent() {
            ensure_no_symlink_ancestors(destination, parent)?;
            fs::create_dir_all(parent)?;
        }

        let entry_type = entry.header().entry_type();
        if entry_type.is_dir() {
            reject_if_symlink(&output_path)?;
            fs::create_dir_all(&output_path)?;
            continue;
        }

        if entry_type.is_file() {
            reject_if_symlink(&output_path)?;
            if output_path.exists() {
                return Err(GitClosureError::Parse(format!(
                    "github-api: duplicate file entry path in archive: {}",
                    relative.display()
                )));
            }
            entry.unpack(&output_path).map_err(|err| {
                GitClosureError::Parse(format!(
                    "github-api: failed to unpack file {}: {err}",
                    output_path.display()
                ))
            })?;
            continue;
        }

        if entry_type.is_symlink() {
            reject_if_symlink(&output_path)?;
            if output_path.exists() {
                return Err(GitClosureError::Parse(format!(
                    "github-api: duplicate symlink entry path in archive: {}",
                    relative.display()
                )));
            }
            let target = entry.link_name().map_err(|err| {
                GitClosureError::Parse(format!("github-api: invalid symlink entry target: {err}"))
            })?;
            let target = target.ok_or_else(|| {
                GitClosureError::Parse("github-api: symlink entry missing target".to_string())
            })?;
            let target_path = target.as_ref();
            let effective_target = if target_path.is_absolute() {
                target_path.to_path_buf()
            } else {
                output_path
                    .parent()
                    .unwrap_or(destination)
                    .join(target_path)
            };
            let normalized = lexical_normalize(&effective_target)?;
            if !normalized.starts_with(destination) {
                return Err(GitClosureError::UnsafePath(format!(
                    "github-api: symlink target escapes destination: {}",
                    relative.display()
                )));
            }
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(target_path, &output_path)?;
            }
            #[cfg(not(unix))]
            {
                let _ = target_path;
                return Err(GitClosureError::Parse(
                    "github-api: symlink extraction is unsupported on this platform".to_string(),
                ));
            }
            continue;
        }

        return Err(GitClosureError::Parse(format!(
            "github-api: unsupported tar entry type for {}",
            relative.display()
        )));
    }

    if top_level.is_none() {
        return Err(GitClosureError::Parse(
            "github-api: archive contained no entries".to_string(),
        ));
    }

    Ok(())
}

pub(super) fn strip_github_archive_prefix(
    path: &Path,
    top_level: &mut Option<std::ffi::OsString>,
) -> Result<Option<PathBuf>> {
    let mut components = path.components();
    let first = match components.next() {
        Some(Component::Normal(name)) => name.to_os_string(),
        _ => {
            return Err(GitClosureError::UnsafePath(path.display().to_string()));
        }
    };

    match top_level {
        Some(existing) if existing != &first => {
            return Err(GitClosureError::Parse(format!(
                "github-api: archive has multiple top-level directories: {} and {}",
                existing.to_string_lossy(),
                first.to_string_lossy(),
            )));
        }
        Some(_) => {}
        None => {
            *top_level = Some(first);
        }
    }

    let mut relative = PathBuf::new();
    for component in components {
        match component {
            Component::Normal(part) => relative.push(part),
            _ => {
                return Err(GitClosureError::UnsafePath(path.display().to_string()));
            }
        }
    }

    if relative.as_os_str().is_empty() {
        return Ok(None);
    }

    Ok(Some(relative))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::GitClosureError;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Mutex;
    use std::time::Duration;

    // Env-var isolation guards for GCL_GITHUB_TARBALL_MAX_BYTES.
    //
    // NOTE: The limit-testing tests (rejects_content_length_over_limit,
    // rejects_stream_over_limit) no longer use env mutation - they pass
    // max_bytes directly via the explicit parameter added in RP-24. These
    // guards exist to provide unwind-safe cleanup in the two RAII regression
    // tests below, and to guard any future code that reintroduces env-var-based
    // limit reading.
    //
    // If GCL_GITHUB_TARBALL_MAX_BYTES is removed from the codebase entirely,
    // these guards and their tests can also be removed.
    static TARBALL_LIMIT_ENV_LOCK: Mutex<()> = Mutex::new(());
    const TARBALL_LIMIT_ENV: &str = "GCL_GITHUB_TARBALL_MAX_BYTES";

    fn lock_tarball_limit_env() -> std::sync::MutexGuard<'static, ()> {
        match TARBALL_LIMIT_ENV_LOCK.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Holds `TARBALL_LIMIT_ENV_LOCK` for its full lifetime while overriding
    /// `GCL_GITHUB_TARBALL_MAX_BYTES`, then restores the previous value on drop.
    ///
    /// Use this guard when a test needs exclusive env-var access throughout its
    /// body.
    struct TarballLimitEnvOverride {
        _lock: std::sync::MutexGuard<'static, ()>,
        previous: Option<String>,
    }

    impl TarballLimitEnvOverride {
        fn set_for_test(value: &str) -> Self {
            let lock = lock_tarball_limit_env();
            let previous = std::env::var(TARBALL_LIMIT_ENV).ok();
            std::env::set_var(TARBALL_LIMIT_ENV, value);
            Self {
                _lock: lock,
                previous,
            }
        }
    }

    /// Saves the current value of `GCL_GITHUB_TARBALL_MAX_BYTES` and restores
    /// it on drop.
    ///
    /// Does NOT hold `TARBALL_LIMIT_ENV_LOCK` for its full lifetime.
    /// It is only a cleanup fallback, not a full mutex guard.
    ///
    /// Use `TarballLimitEnvOverride` if you need exclusive access for the duration
    /// of a test body.
    struct TarballLimitEnvRestore {
        previous: Option<String>,
    }

    impl TarballLimitEnvRestore {
        fn capture() -> Self {
            let _lock = lock_tarball_limit_env();
            let previous = std::env::var(TARBALL_LIMIT_ENV).ok();
            Self { previous }
        }
    }

    impl Drop for TarballLimitEnvRestore {
        fn drop(&mut self) {
            let _lock = lock_tarball_limit_env();
            if let Some(previous) = &self.previous {
                std::env::set_var(TARBALL_LIMIT_ENV, previous);
            } else {
                std::env::remove_var(TARBALL_LIMIT_ENV);
            }
        }
    }

    impl Drop for TarballLimitEnvOverride {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                std::env::set_var(TARBALL_LIMIT_ENV, previous);
            } else {
                std::env::remove_var(TARBALL_LIMIT_ENV);
            }
        }
    }

    fn make_gzipped_tar(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut gz = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut builder = tar::Builder::new(&mut gz);
            for (path, bytes) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_size(bytes.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder
                    .append_data(&mut header, *path, *bytes)
                    .expect("append tar file entry");
            }
            builder.finish().expect("finish tar builder");
        }
        gz.finish().expect("finish gzip stream")
    }

    #[test]
    fn tarball_limit_env_override_restores_previous_value_on_drop() {
        // Regression test: verifies TarballLimitEnvOverride cleans up on normal drop,
        // even though production tests now pass explicit max_bytes parameters.
        let _restore = TarballLimitEnvRestore::capture();
        let _env_guard = lock_tarball_limit_env();
        std::env::remove_var(TARBALL_LIMIT_ENV);
        drop(_env_guard);

        {
            let _env = TarballLimitEnvOverride::set_for_test("8");
            assert_eq!(std::env::var(TARBALL_LIMIT_ENV).as_deref(), Ok("8"));
        }

        let _env_guard = lock_tarball_limit_env();
        assert!(
            std::env::var(TARBALL_LIMIT_ENV).is_err(),
            "override guard must remove env var when no previous value exists"
        );
    }

    #[test]
    fn tarball_limit_env_override_restores_previous_value_after_panic() {
        // Regression test: verifies TarballLimitEnvOverride cleans up on unwind,
        // even though production tests now pass explicit max_bytes parameters.
        let _restore = TarballLimitEnvRestore::capture();
        let _env_guard = lock_tarball_limit_env();
        std::env::remove_var(TARBALL_LIMIT_ENV);
        drop(_env_guard);

        let panic = std::panic::catch_unwind(|| {
            let _env = TarballLimitEnvOverride::set_for_test("16");
            panic!("simulated panic while env override is active");
        });
        assert!(panic.is_err(), "test must panic to validate unwind cleanup");

        let _env_guard = lock_tarball_limit_env();
        assert!(
            std::env::var(TARBALL_LIMIT_ENV).is_err(),
            "override guard must remove env var after unwind when no previous value exists"
        );
    }

    #[test]
    fn parse_github_api_source_accepts_gh_and_https_syntax() {
        let gh = parse_github_api_source("gh:owner/repo@main").expect("parse gh syntax");
        assert_eq!(
            gh,
            ParsedGithubApiSource {
                owner: "owner".to_string(),
                repo: "repo".to_string(),
                reference: Some("main".to_string())
            }
        );

        let https =
            parse_github_api_source("https://github.com/owner/repo").expect("parse https syntax");
        assert_eq!(
            https,
            ParsedGithubApiSource {
                owner: "owner".to_string(),
                repo: "repo".to_string(),
                reference: None
            }
        );
    }

    #[test]
    fn parse_github_api_source_rejects_non_github_inputs() {
        let err = parse_github_api_source("gl:group/repo").expect_err("gl source must fail");
        assert!(
            matches!(err, GitClosureError::Parse(_)),
            "expected parse error for non-github source"
        );
    }

    #[test]
    fn github_api_download_follows_redirects() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test listener");
        let addr = listener.local_addr().expect("listener addr");
        let payload = b"redirect-ok".to_vec();
        let payload_for_server = payload.clone();

        let server = std::thread::spawn(move || {
            let mut seen_redirect = false;
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept connection");
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("set read timeout");
                let mut req_buf = [0u8; 2048];
                let n = stream.read(&mut req_buf).expect("read request");
                let request = String::from_utf8_lossy(&req_buf[..n]);

                if request.starts_with("GET /redirect ") {
                    let response = format!(
                        "HTTP/1.1 302 Found\r\nLocation: http://{addr}/tarball\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    stream
                        .write_all(response.as_bytes())
                        .expect("write redirect response");
                    seen_redirect = true;
                } else if request.starts_with("GET /tarball ") {
                    let headers = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        payload_for_server.len()
                    );
                    stream
                        .write_all(headers.as_bytes())
                        .expect("write ok headers");
                    stream
                        .write_all(&payload_for_server)
                        .expect("write payload");
                    return seen_redirect;
                }
            }
            false
        });

        let bytes = download_tarball_url(
            &format!("http://{addr}/redirect"),
            "owner/repo@HEAD",
            None,
            32,
        )
        .expect("redirected download should succeed");

        assert_eq!(bytes, payload);
        assert!(
            server.join().expect("join test server"),
            "server should observe redirect then tarball request"
        );
    }

    #[test]
    fn github_api_download_rejects_content_length_over_limit() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test listener");
        let addr = listener.local_addr().expect("listener addr");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept connection");
            let response =
                "HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\n123456789";
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        });

        let result = download_tarball_url(
            &format!("http://{addr}/tarball"),
            "owner/repo@HEAD",
            None,
            8,
        );
        server.join().expect("join test server");

        let err = result.expect_err("content-length over configured limit must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("owner/repo@HEAD") && msg.contains("8") && msg.contains("Content-Length"),
            "error should mention source and limit, got: {msg}"
        );
    }

    #[test]
    fn github_api_download_rejects_stream_over_limit() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test listener");
        let addr = listener.local_addr().expect("listener addr");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept connection");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("set read timeout");
            let mut req_buf = [0u8; 2048];
            let _ = stream.read(&mut req_buf).expect("read request");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n")
                .expect("write headers");
            for _ in 0..4 {
                if stream.write_all(b"12345678").is_err() {
                    break;
                }
            }
        });

        let result = download_tarball_url(
            &format!("http://{addr}/stream"),
            "owner/repo@HEAD",
            None,
            16,
        );
        server.join().expect("join test server");

        let err = result.expect_err("stream over configured limit must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("owner/repo@HEAD") && msg.contains("16") && msg.contains("read"),
            "error should mention source and limit, got: {msg}"
        );
    }

    #[test]
    fn github_api_status_error_maps_auth_and_rate_limit_cases() {
        let auth = github_api_status_error(401, None, "owner/repo@HEAD", "");
        assert!(
            auth.to_string().contains("authentication failed")
                && auth.to_string().contains("GCL_GITHUB_TOKEN"),
            "401 must mention authentication and token env var"
        );

        let rate = github_api_status_error(403, Some("0"), "owner/repo@HEAD", "rate limited");
        assert!(
            rate.to_string().contains("rate limit")
                && rate.to_string().contains("GCL_GITHUB_TOKEN"),
            "rate-limit errors must be actionable"
        );

        let missing = github_api_status_error(404, None, "owner/repo@badref", "");
        assert!(
            missing.to_string().contains("not found")
                && missing.to_string().contains("owner/repo@badref"),
            "404 must mention missing repo/ref"
        );
    }

    #[test]
    fn strip_github_archive_prefix_rejects_parent_traversal() {
        let mut top = None;
        let err = strip_github_archive_prefix(Path::new("repo-abc/../../evil.txt"), &mut top)
            .expect_err("path traversal in archive must be rejected");
        assert!(matches!(err, GitClosureError::UnsafePath(_)));
    }

    #[test]
    fn split_github_archive_prefix_strips_top_level_directory() {
        let mut top = None;
        let rel = strip_github_archive_prefix(Path::new("repo-abc/src/lib.rs"), &mut top)
            .expect("valid github archive entry path")
            .expect("non-root entry must remain after stripping");
        assert_eq!(rel, std::path::PathBuf::from("src/lib.rs"));
    }

    #[test]
    fn github_archive_extraction_strips_prefix_and_writes_files() {
        let tarball = make_gzipped_tar(&[
            ("repo-abc/README.md", b"hello\n"),
            ("repo-abc/src/lib.rs", b"pub fn x() {}\n"),
        ]);
        let tmp = tempfile::TempDir::new().expect("create tempdir");
        let dest = tmp.path().join("out");
        std::fs::create_dir_all(&dest).expect("create destination dir");

        extract_github_tarball(&tarball, &dest).expect("extract archive");
        let readme = std::fs::read_to_string(dest.join("README.md")).expect("read README");
        let lib = std::fs::read_to_string(dest.join("src/lib.rs")).expect("read src/lib.rs");
        assert_eq!(readme, "hello\n");
        assert_eq!(lib, "pub fn x() {}\n");
    }

    #[cfg(unix)]
    #[test]
    fn github_archive_extraction_preserves_symlink_entries() {
        let mut gz = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut builder = tar::Builder::new(&mut gz);

            let mut file_header = tar::Header::new_gnu();
            let file_bytes = b"target\n";
            file_header.set_size(file_bytes.len() as u64);
            file_header.set_mode(0o644);
            file_header.set_cksum();
            builder
                .append_data(&mut file_header, "repo-abc/target.txt", &file_bytes[..])
                .expect("append regular file");

            let mut link_header = tar::Header::new_gnu();
            link_header.set_entry_type(tar::EntryType::Symlink);
            link_header.set_size(0);
            link_header.set_mode(0o777);
            link_header
                .set_link_name("target.txt")
                .expect("set symlink target");
            link_header.set_cksum();
            builder
                .append_data(&mut link_header, "repo-abc/link", std::io::empty())
                .expect("append symlink entry");

            builder.finish().expect("finish tar builder");
        }
        let tarball = gz.finish().expect("finish gzip stream");

        let tmp = tempfile::TempDir::new().expect("create tempdir");
        let dest = tmp.path().join("out");
        std::fs::create_dir_all(&dest).expect("create destination dir");

        extract_github_tarball(&tarball, &dest).expect("extract archive");
        let target = std::fs::read_link(dest.join("link")).expect("read extracted symlink");
        assert_eq!(target, std::path::PathBuf::from("target.txt"));
    }

    #[cfg(unix)]
    #[test]
    fn github_archive_extraction_rejects_absolute_symlink_target_escape() {
        let mut gz = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut builder = tar::Builder::new(&mut gz);

            let mut link_header = tar::Header::new_gnu();
            link_header.set_entry_type(tar::EntryType::Symlink);
            link_header.set_size(0);
            link_header.set_mode(0o777);
            link_header
                .set_link_name("/etc")
                .expect("set absolute target");
            link_header.set_cksum();
            builder
                .append_data(&mut link_header, "repo-abc/link", std::io::empty())
                .expect("append symlink entry");

            builder.finish().expect("finish tar builder");
        }
        let tarball = gz.finish().expect("finish gzip stream");

        let tmp = tempfile::TempDir::new().expect("create tempdir");
        let dest = tmp.path().join("out");
        std::fs::create_dir_all(&dest).expect("create destination dir");

        let err = extract_github_tarball(&tarball, &dest)
            .expect_err("absolute symlink target must be rejected");
        assert!(matches!(err, GitClosureError::UnsafePath(_)));
    }

    #[cfg(unix)]
    #[test]
    fn github_archive_extraction_rejects_relative_symlink_target_escape() {
        let mut gz = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut builder = tar::Builder::new(&mut gz);

            let mut link_header = tar::Header::new_gnu();
            link_header.set_entry_type(tar::EntryType::Symlink);
            link_header.set_size(0);
            link_header.set_mode(0o777);
            link_header
                .set_link_name("../../escape")
                .expect("set traversal target");
            link_header.set_cksum();
            builder
                .append_data(&mut link_header, "repo-abc/sub/link", std::io::empty())
                .expect("append symlink entry");

            builder.finish().expect("finish tar builder");
        }
        let tarball = gz.finish().expect("finish gzip stream");

        let tmp = tempfile::TempDir::new().expect("create tempdir");
        let dest = tmp.path().join("out");
        std::fs::create_dir_all(&dest).expect("create destination dir");

        let err = extract_github_tarball(&tarball, &dest)
            .expect_err("relative symlink escape target must be rejected");
        assert!(matches!(err, GitClosureError::UnsafePath(_)));
    }

    #[cfg(unix)]
    #[test]
    fn github_archive_extraction_allows_safe_relative_symlink_target() {
        let mut gz = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut builder = tar::Builder::new(&mut gz);

            let mut file_header = tar::Header::new_gnu();
            let file_bytes = b"ok\n";
            file_header.set_size(file_bytes.len() as u64);
            file_header.set_mode(0o644);
            file_header.set_cksum();
            builder
                .append_data(&mut file_header, "repo-abc/sub/sibling", &file_bytes[..])
                .expect("append sibling file");

            let mut link_header = tar::Header::new_gnu();
            link_header.set_entry_type(tar::EntryType::Symlink);
            link_header.set_size(0);
            link_header.set_mode(0o777);
            link_header
                .set_link_name("./sibling")
                .expect("set safe target");
            link_header.set_cksum();
            builder
                .append_data(&mut link_header, "repo-abc/sub/link", std::io::empty())
                .expect("append symlink entry");

            builder.finish().expect("finish tar builder");
        }
        let tarball = gz.finish().expect("finish gzip stream");

        let tmp = tempfile::TempDir::new().expect("create tempdir");
        let dest = tmp.path().join("out");
        std::fs::create_dir_all(&dest).expect("create destination dir");

        extract_github_tarball(&tarball, &dest).expect("safe symlink should extract");
        let target = std::fs::read_link(dest.join("sub/link")).expect("read extracted symlink");
        assert_eq!(target, std::path::PathBuf::from("./sibling"));
    }

    #[cfg(unix)]
    #[test]
    fn github_archive_extraction_rejects_symlink_parent_escape() {
        let mut gz = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut builder = tar::Builder::new(&mut gz);

            let mut dir_link_header = tar::Header::new_gnu();
            dir_link_header.set_entry_type(tar::EntryType::Symlink);
            dir_link_header.set_size(0);
            dir_link_header.set_mode(0o777);
            dir_link_header
                .set_link_name("../escape")
                .expect("set symlink target");
            dir_link_header.set_cksum();
            builder
                .append_data(&mut dir_link_header, "repo-abc/dir", std::io::empty())
                .expect("append symlinked directory entry");

            let mut file_header = tar::Header::new_gnu();
            let payload = b"owned\n";
            file_header.set_size(payload.len() as u64);
            file_header.set_mode(0o644);
            file_header.set_cksum();
            builder
                .append_data(&mut file_header, "repo-abc/dir/payload.txt", &payload[..])
                .expect("append nested file");

            builder.finish().expect("finish tar builder");
        }
        let tarball = gz.finish().expect("finish gzip stream");

        let tmp = tempfile::TempDir::new().expect("create tempdir");
        let dest = tmp.path().join("out");
        std::fs::create_dir_all(&dest).expect("create destination dir");
        let escape = tmp.path().join("escape");
        std::fs::create_dir_all(&escape).expect("create would-be escape dir");

        let err = extract_github_tarball(&tarball, &dest)
            .expect_err("archive writing through symlink parent must be rejected");
        assert!(
            matches!(err, GitClosureError::UnsafePath(_)),
            "expected UnsafePath, got {err:?}"
        );
        assert!(
            !escape.join("payload.txt").exists(),
            "extraction must not write outside destination root"
        );
    }

    #[test]
    fn github_archive_extraction_rejects_duplicate_file_entries() {
        let tarball = make_gzipped_tar(&[
            ("repo-abc/dup.txt", b"first\n"),
            ("repo-abc/dup.txt", b"second\n"),
        ]);
        let tmp = tempfile::TempDir::new().expect("create tempdir");
        let dest = tmp.path().join("out");
        std::fs::create_dir_all(&dest).expect("create destination dir");

        let err = extract_github_tarball(&tarball, &dest)
            .expect_err("duplicate file entries must be rejected");
        assert!(
            matches!(err, GitClosureError::Parse(_)),
            "expected Parse error, got {err:?}"
        );
        assert!(
            err.to_string().contains("duplicate file entry path"),
            "error must mention duplicate file entry path: {err}"
        );
    }

    #[test]
    fn github_api_provider_rejects_non_github_source() {
        let provider = GithubApiProvider;
        let err = match provider.fetch("gl:group/repo") {
            Ok(_) => panic!("github-api provider must reject non-github source syntax"),
            Err(e) => e,
        };
        assert!(
            matches!(err, GitClosureError::Parse(_)),
            "expected Parse error, got {err:?}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("github-api") || msg.contains("GitHub"),
            "error message must mention github-api source requirement, got: {msg:?}"
        );
    }
}
