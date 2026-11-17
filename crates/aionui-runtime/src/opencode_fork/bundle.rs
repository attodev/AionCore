// fork(opencode): download, verify and place the pinned opencode into the managed-resources bundle — see docs/fork/opencode-overlay.md

use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use tracing::info;

use crate::http_client;
use crate::managed_resources::export_cli_to_root;
use crate::managed_resources_contract::ManagedCliResourceContract;

use super::{OPENCODE_COMMAND, OpencodeAsset, locked_asset, opencode_lock};

pub const OPENCODE_DOWNLOAD_BASE_ENV: &str = "AIONUI_OPENCODE_DOWNLOAD_BASE";
const DEFAULT_DOWNLOAD_BASE: &str = "https://github.com/anomalyco/opencode/releases/download";

// Same budget as the managed Node download (node_runtime/managed.rs).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);
const DOWNLOAD_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BundleOutcome {
    Bundled(ManagedCliResourceContract),
    SkippedUnsupportedPlatform { runtime_key: String },
}

/// `stage` is a stable machine-readable name (`download`, `checksum`,
/// `extract`, `verify`, `export`); `message` never carries a full URL or path.
#[derive(Debug, thiserror::Error)]
#[error("opencode bundle stage={stage}: {message}")]
pub struct OpencodeBundleError {
    pub stage: &'static str,
    pub message: String,
}

fn fail(stage: &'static str, message: impl Into<String>) -> OpencodeBundleError {
    OpencodeBundleError {
        stage,
        message: message.into(),
    }
}

/// `output_root` is the `--bundle-out` directory of `prepare-managed-resources`.
pub async fn bundle_opencode(output_root: &Path) -> Result<BundleOutcome, OpencodeBundleError> {
    let base = std::env::var(OPENCODE_DOWNLOAD_BASE_ENV).unwrap_or_else(|_| DEFAULT_DOWNLOAD_BASE.to_owned());
    let runtime_key = crate::current_runtime_key().unwrap_or("unknown");
    bundle_for_key(&base, runtime_key, output_root).await
}

pub(crate) async fn bundle_for_key(
    base: &str,
    runtime_key: &str,
    output_root: &Path,
) -> Result<BundleOutcome, OpencodeBundleError> {
    let Some(asset) = locked_asset(runtime_key) else {
        return Ok(BundleOutcome::SkippedUnsupportedPlatform {
            runtime_key: runtime_key.to_owned(),
        });
    };
    let version = &opencode_lock().version;
    bundle_opencode_from(base, asset, version, runtime_key, output_root)
        .await
        .map(BundleOutcome::Bundled)
}

pub(crate) async fn bundle_opencode_from(
    base: &str,
    asset: &OpencodeAsset,
    version: &str,
    runtime_key: &str,
    output_root: &Path,
) -> Result<ManagedCliResourceContract, OpencodeBundleError> {
    if !asset.file.ends_with(".tar.gz") {
        return Err(fail(
            "download",
            format!("unsupported archive type for {} (only .tar.gz)", asset.file),
        ));
    }
    let work = TempWorkDir::create(output_root)?;
    let archive_path = work.path().join("archive.download");
    let url = format!("{}/v{version}/{}", base.trim_end_matches('/'), asset.file);

    download_verified(&url, &asset.file, &asset.sha256, &archive_path).await?;

    let extract_dir = work.path().join("extract");
    extract_tar_gz(&archive_path, &extract_dir, &asset.file)?;
    check_executable(&extract_dir, &asset.executable)?;

    let exported = export_cli_to_root(output_root, &extract_dir, OPENCODE_COMMAND, version, runtime_key);
    if let Err(error) = exported {
        remove_cli_output(output_root, version, runtime_key);
        return Err(fail("export", error.to_string()));
    }
    info!(version, runtime_key, "bundled opencode");

    Ok(ManagedCliResourceContract {
        name: OPENCODE_COMMAND.into(),
        version: version.into(),
        root: format!("cli/{OPENCODE_COMMAND}/{version}/{runtime_key}"),
        platform_directory: runtime_key.into(),
        executable: asset.executable.clone(),
        required_files: Vec::new(),
        required_directories: Vec::new(),
    })
}

/// Scratch directory inside the bundle root, removed on drop so no failure
/// path leaves a partial download or extraction behind.
struct TempWorkDir(PathBuf);

impl TempWorkDir {
    fn create(output_root: &Path) -> Result<Self, OpencodeBundleError> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = output_root.join(format!(".opencode-bundle-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&dir).map_err(|error| fail("download", format!("create work dir: {error}")))?;
        Ok(Self(dir))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempWorkDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Removes `cli/opencode/<version>/<key>` and any now-empty parents up to and
/// including `cli/opencode`.
fn remove_cli_output(output_root: &Path, version: &str, runtime_key: &str) {
    let base = output_root.join("cli").join(OPENCODE_COMMAND);
    let _ = fs::remove_dir_all(base.join(version).join(runtime_key));
    let _ = fs::remove_dir(base.join(version));
    let _ = fs::remove_dir(&base);
    let _ = fs::remove_dir(output_root.join("cli"));
}

async fn download_verified(
    url: &str,
    file: &str,
    expected_sha256: &str,
    archive_path: &Path,
) -> Result<(), OpencodeBundleError> {
    let client =
        http_client::build_http_client(CONNECT_TIMEOUT, DOWNLOAD_TIMEOUT).map_err(|error| fail("download", error))?;
    let mut response = client.get(url).send().await.map_err(|error| {
        fail(
            "download",
            format!("request for {file} failed: {}", error.without_url()),
        )
    })?;
    if !response.status().is_success() {
        return Err(fail(
            "download",
            format!("download of {file} returned HTTP {}", response.status().as_u16()),
        ));
    }

    let mut writer =
        fs::File::create(archive_path).map_err(|error| fail("download", format!("create archive file: {error}")))?;
    let mut hasher = Sha256::new();
    loop {
        let chunk = tokio::time::timeout(DOWNLOAD_IDLE_TIMEOUT, response.chunk())
            .await
            .map_err(|_| fail("download", format!("download of {file} stalled")))?
            .map_err(|error| fail("download", format!("reading {file} failed: {}", error.without_url())))?;
        let Some(chunk) = chunk else { break };
        hasher.update(&chunk);
        writer
            .write_all(&chunk)
            .map_err(|error| fail("download", format!("write archive: {error}")))?;
    }
    writer
        .flush()
        .map_err(|error| fail("download", format!("write archive: {error}")))?;

    let actual = hex::encode(hasher.finalize());
    if !actual.eq_ignore_ascii_case(expected_sha256) {
        return Err(fail(
            "checksum",
            format!("checksum mismatch for {file}: expected {expected_sha256}, got {actual}"),
        ));
    }
    Ok(())
}

/// Unpacks regular files and directories only, and refuses any entry whose
/// path is absolute or climbs out via `..` (tar's own `unpack` would silently
/// skip those, which would hide a tampered archive).
fn extract_tar_gz(archive_path: &Path, dest: &Path, file: &str) -> Result<(), OpencodeBundleError> {
    let extract_err = |detail: String| fail("extract", format!("{file}: {detail}"));
    fs::create_dir_all(dest).map_err(|error| extract_err(error.to_string()))?;
    let archive_file = fs::File::open(archive_path).map_err(|error| extract_err(error.to_string()))?;
    let mut archive = tar::Archive::new(GzDecoder::new(archive_file));
    let entries = archive.entries().map_err(|error| extract_err(error.to_string()))?;
    for entry in entries {
        let mut entry = entry.map_err(|error| extract_err(error.to_string()))?;
        let entry_path = entry
            .path()
            .map_err(|error| extract_err(error.to_string()))?
            .into_owned();
        if entry_path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::RootDir | Component::Prefix(_)))
        {
            return Err(extract_err("archive entry has an absolute or parent path".into()));
        }
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_dir()) {
            return Err(extract_err("archive entry is not a regular file or directory".into()));
        }
        let unpacked = entry.unpack_in(dest).map_err(|error| extract_err(error.to_string()))?;
        if !unpacked {
            return Err(extract_err("archive entry escapes the extraction directory".into()));
        }
    }
    Ok(())
}

fn check_executable(extract_dir: &Path, executable: &str) -> Result<(), OpencodeBundleError> {
    let path = extract_dir.join(executable);
    let metadata = fs::metadata(&path).map_err(|_| fail("verify", format!("{executable} missing from archive")))?;
    if !metadata.is_file() {
        return Err(fail("verify", format!("{executable} is not a regular file")));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(fail("verify", format!("{executable} is not executable")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managed_resources_contract::{
        MANAGED_RESOURCES_CONTRACT_SCHEMA_VERSION, ManagedNodeResourceContract, ManagedResourcesContract,
        validate_contract,
    };
    use sha2::{Digest, Sha256};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const KEY: &str = "linux-x64";
    const FILE: &str = "opencode-linux-x64.tar.gz";

    /// Builds a tar.gz; `mode` 0o755 for executables. Raw header bytes are used
    /// so that `../` entry names (which the safe builder API refuses) can be tested.
    fn tar_gz(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, data, mode) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(*mode);
            header.set_entry_type(tar::EntryType::Regular);
            let bytes = name.as_bytes();
            header.as_old_mut().name[..bytes.len()].copy_from_slice(bytes);
            header.set_cksum();
            builder.append(&header, *data).unwrap();
        }
        let tar_bytes = builder.into_inner().unwrap();
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut enc, &tar_bytes).unwrap();
        enc.finish().unwrap()
    }

    fn asset_for(bytes: &[u8]) -> OpencodeAsset {
        OpencodeAsset {
            file: FILE.into(),
            sha256: hex::encode(Sha256::digest(bytes)),
            executable: "opencode".into(),
        }
    }

    async fn serve(bytes: Vec<u8>) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/v2.3.0/{FILE}")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes))
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn bundles_binary_and_returns_contract() {
        let archive = tar_gz(&[("opencode", b"#!/bin/sh\necho 2.3.0\n", 0o755)]);
        let asset = asset_for(&archive);
        let server = serve(archive).await;
        let out = tempfile::tempdir().unwrap();

        let contract = bundle_opencode_from(&server.uri(), &asset, "2.3.0", KEY, out.path())
            .await
            .unwrap();

        let bin = out.path().join("cli/opencode/2.3.0").join(KEY).join("opencode");
        assert!(bin.is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(std::fs::metadata(&bin).unwrap().permissions().mode() & 0o111, 0);
        }
        assert_eq!(contract.name, "opencode");
        assert_eq!(contract.version, "2.3.0");
        assert_eq!(contract.root, format!("cli/opencode/2.3.0/{KEY}"));
        assert_eq!(contract.platform_directory, KEY);
        assert_eq!(contract.executable, "opencode");
        assert!(contract.required_files.is_empty() && contract.required_directories.is_empty());

        let manifest = ManagedResourcesContract {
            schema_version: MANAGED_RESOURCES_CONTRACT_SCHEMA_VERSION,
            runtime_key: KEY.into(),
            node: ManagedNodeResourceContract {
                version: "1".into(),
                root: "node/x".into(),
                executable: "node".into(),
            },
            clis: vec![contract],
        };
        std::fs::create_dir_all(out.path().join("node/x")).unwrap();
        std::fs::write(out.path().join("node/x/node"), b"n").unwrap();
        validate_contract(out.path(), &manifest).unwrap();
        assert_leftovers_clean(out.path());
    }

    /// Only `cli/` may exist in the bundle root after a run (no temp dirs).
    fn assert_leftovers_clean(root: &Path) {
        for entry in std::fs::read_dir(root).unwrap() {
            let name = entry.unwrap().file_name();
            assert!(["cli", "node"].contains(&name.to_str().unwrap()), "leftover {name:?}");
        }
    }

    #[tokio::test]
    async fn checksum_mismatch_fails_and_leaves_nothing() {
        let archive = tar_gz(&[("opencode", b"x", 0o755)]);
        let mut asset = asset_for(&archive);
        asset.sha256 = "0".repeat(64);
        let server = serve(archive).await;
        let out = tempfile::tempdir().unwrap();

        let err = bundle_opencode_from(&server.uri(), &asset, "2.3.0", KEY, out.path())
            .await
            .unwrap_err();
        assert_eq!(err.stage, "checksum");
        assert!(!out.path().join("cli/opencode").exists());
        assert_leftovers_clean(out.path());
    }

    #[tokio::test]
    async fn archive_without_executable_fails() {
        let archive = tar_gz(&[("README", b"x", 0o644)]);
        let asset = asset_for(&archive);
        let server = serve(archive).await;
        let out = tempfile::tempdir().unwrap();

        let err = bundle_opencode_from(&server.uri(), &asset, "2.3.0", KEY, out.path())
            .await
            .unwrap_err();
        assert_eq!(err.stage, "verify");
        assert!(!out.path().join("cli/opencode").exists());
        assert_leftovers_clean(out.path());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn non_executable_binary_fails() {
        let archive = tar_gz(&[("opencode", b"x", 0o644)]);
        let asset = asset_for(&archive);
        let server = serve(archive).await;
        let out = tempfile::tempdir().unwrap();

        let err = bundle_opencode_from(&server.uri(), &asset, "2.3.0", KEY, out.path())
            .await
            .unwrap_err();
        assert_eq!(err.stage, "verify");
        assert!(!out.path().join("cli/opencode").exists());
    }

    #[tokio::test]
    async fn archive_with_parent_path_fails() {
        let archive = tar_gz(&[("opencode", b"x", 0o755), ("../evil", b"pwn", 0o644)]);
        let asset = asset_for(&archive);
        let server = serve(archive).await;
        let parent = tempfile::tempdir().unwrap();
        let out = parent.path().join("bundle");
        std::fs::create_dir_all(&out).unwrap();

        let err = bundle_opencode_from(&server.uri(), &asset, "2.3.0", KEY, &out)
            .await
            .unwrap_err();
        assert_eq!(err.stage, "extract");
        assert!(!out.join("cli/opencode").exists());
        assert!(!parent.path().join("evil").exists());
        assert!(!out.join("evil").exists());
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn archive_with_absolute_path_fails() {
        let archive = tar_gz(&[("/tmp/opencode-abs-evil", b"pwn", 0o644)]);
        let asset = asset_for(&archive);
        let server = serve(archive).await;
        let out = tempfile::tempdir().unwrap();

        let err = bundle_opencode_from(&server.uri(), &asset, "2.3.0", KEY, out.path())
            .await
            .unwrap_err();
        assert_eq!(err.stage, "extract");
        assert!(!Path::new("/tmp/opencode-abs-evil").exists());
    }

    #[tokio::test]
    async fn http_404_fails_with_stage() {
        let server = MockServer::start().await;
        let archive = tar_gz(&[("opencode", b"x", 0o755)]);
        let asset = asset_for(&archive);
        let out = tempfile::tempdir().unwrap();

        let err = bundle_opencode_from(&server.uri(), &asset, "2.3.0", KEY, out.path())
            .await
            .unwrap_err();
        assert_eq!(err.stage, "download");
        let text = err.to_string();
        assert!(text.contains("download"), "{text}");
        assert!(text.contains(FILE), "{text}");
        assert!(!text.contains(&server.uri()), "{text}");
        assert!(!out.path().join("cli/opencode").exists());
        assert_leftovers_clean(out.path());
    }

    #[tokio::test]
    async fn unsupported_platform_is_skipped() {
        let out = tempfile::tempdir().unwrap();
        let outcome = bundle_for_key("http://127.0.0.1:1", "plan9-mips", out.path())
            .await
            .unwrap();
        assert_eq!(
            outcome,
            BundleOutcome::SkippedUnsupportedPlatform {
                runtime_key: "plan9-mips".into()
            }
        );
        assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn rebundle_is_idempotent() {
        let archive = tar_gz(&[("opencode", b"bin", 0o755)]);
        let asset = asset_for(&archive);
        let server = serve(archive).await;
        let out = tempfile::tempdir().unwrap();

        let first = bundle_opencode_from(&server.uri(), &asset, "2.3.0", KEY, out.path())
            .await
            .unwrap();
        let second = bundle_opencode_from(&server.uri(), &asset, "2.3.0", KEY, out.path())
            .await
            .unwrap();
        assert_eq!(first, second);
        let bin = out.path().join("cli/opencode/2.3.0").join(KEY).join("opencode");
        assert_eq!(std::fs::read(bin).unwrap(), b"bin");
    }
}
