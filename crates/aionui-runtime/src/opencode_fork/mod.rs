// fork(opencode): version lock and executable resolution for the bundled opencode — see docs/fork/opencode-overlay.md

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Deserialize;

use crate::managed_resources::cli_sources;

pub const OPENCODE_COMMAND: &str = "opencode";
pub const OPENCODE_BIN_ENV: &str = "AIONUI_OPENCODE_BIN";

pub mod bundle;

const LOCK_JSON: &str = include_str!("../../resources/opencode-lock.json");

#[derive(Debug, Clone, Deserialize)]
pub struct OpencodeLock {
    pub version: String,
    /// Keyed by runtime key (see `current_runtime_key`).
    pub assets: BTreeMap<String, OpencodeAsset>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OpencodeAsset {
    pub file: String,
    pub sha256: String,
    pub executable: String,
}

pub fn opencode_lock() -> &'static OpencodeLock {
    static LOCK: OnceLock<OpencodeLock> = OnceLock::new();
    LOCK.get_or_init(|| serde_json::from_str(LOCK_JSON).expect("opencode-lock.json is valid"))
}

pub fn locked_asset(runtime_key: &str) -> Option<&'static OpencodeAsset> {
    opencode_lock().assets.get(runtime_key)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpencodeSource {
    EnvOverride,
    Bundled,
}

/// Returns a path only when `cmd` is exactly `opencode` and the env override
/// or the bundled binary exists; otherwise the caller falls back to PATH.
pub fn override_command_path(cmd: &str) -> Option<PathBuf> {
    override_source(cmd).map(|(path, _)| path)
}

/// Like [`override_command_path`] but also says where the path came from.
pub fn override_source(cmd: &str) -> Option<(PathBuf, OpencodeSource)> {
    if cmd != OPENCODE_COMMAND {
        return None;
    }
    let candidates = bundled_candidates();
    let env_value = std::env::var_os(OPENCODE_BIN_ENV);
    resolve_override(cmd, env_value.as_deref(), &candidates)
}

/// One-line hook for `resolve_command_path`: rustfmt would expand an inline
/// `if let ... { return ... }` into several lines in the pre-existing file.
macro_rules! return_if_overridden {
    ($cmd:expr) => {
        if let Some(path) = $crate::opencode_fork::override_command_path($cmd) {
            return Some(path);
        }
    };
}
pub(crate) use return_if_overridden;

fn bundled_candidates() -> Vec<PathBuf> {
    let Some(asset) = crate::current_runtime_key().and_then(|key| locked_asset(key).map(|a| (key, a))) else {
        return Vec::new();
    };
    let (key, asset) = asset;
    cli_sources(OPENCODE_COMMAND, &opencode_lock().version, key)
        .into_iter()
        .map(|source| source.root.join(&asset.executable))
        .collect()
}

/// Pure resolution: env override (absolute regular file) first, then the first
/// existing bundled candidate. Never matches a command other than `opencode`.
pub fn resolve_override(
    cmd: &str,
    env_value: Option<&OsStr>,
    bundled_candidates: &[PathBuf],
) -> Option<(PathBuf, OpencodeSource)> {
    if cmd != OPENCODE_COMMAND {
        return None;
    }
    if let Some(value) = env_value {
        let path = Path::new(value);
        if path.is_absolute() && path.is_file() {
            return Some((path.to_path_buf(), OpencodeSource::EnvOverride));
        }
    }
    bundled_candidates
        .iter()
        .find(|path| path.is_file())
        .map(|path| (path.clone(), OpencodeSource::Bundled))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::path::PathBuf;

    fn file_in(dir: &tempfile::TempDir, name: &str) -> PathBuf {
        let p = dir.path().join(name);
        std::fs::write(&p, b"x").unwrap();
        p
    }

    #[test]
    fn resolve_command_path_unchanged_for_other_commands() {
        // A command that surely does not exist, and one that exists only if the host has `cargo`/`sh`.
        assert_eq!(
            crate::resolver::resolve_command_path("definitely-not-a-real-cmd-xyz"),
            None
        );
        for cmd in ["sh", "cargo", "cmd"] {
            let expected = which::which(cmd).ok();
            if expected.is_some() {
                assert_eq!(crate::resolver::resolve_command_path(cmd), expected);
            }
        }
    }

    #[test]
    fn lock_parses_and_pins_2_3_0() {
        let lock = opencode_lock();
        assert_eq!(lock.version, "2.3.0");
        let asset = locked_asset("linux-x64").expect("linux asset");
        assert_eq!(asset.file, "opencode-linux-x64.tar.gz");
        assert_eq!(asset.sha256.len(), 64);
        assert!(asset.sha256.chars().all(|c| c.is_ascii_hexdigit()));
        let mac = locked_asset("darwin-arm64").expect("macOS arm64 asset");
        assert_eq!(mac.file, "opencode-darwin-arm64.zip");
        assert_eq!(mac.sha256.len(), 64);
        assert!(locked_asset("nope").is_none());
    }

    #[test]
    fn other_commands_are_never_overridden() {
        let dir = tempfile::tempdir().unwrap();
        let f = file_in(&dir, "opencode");
        for cmd in ["node", "opencode2", "OPENCODE", "/usr/bin/opencode"] {
            assert_eq!(
                resolve_override(cmd, Some(f.as_os_str()), std::slice::from_ref(&f)),
                None,
                "{cmd}"
            );
        }
    }

    #[test]
    fn env_override_wins_over_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let env = file_in(&dir, "env-bin");
        let bundled = file_in(&dir, "bundled-bin");
        assert_eq!(
            resolve_override("opencode", Some(env.as_os_str()), &[bundled]),
            Some((env, OpencodeSource::EnvOverride))
        );
    }

    #[test]
    fn env_override_ignored_when_missing_dir_or_relative() {
        let dir = tempfile::tempdir().unwrap();
        let bundled = file_in(&dir, "bundled-bin");
        let missing = dir.path().join("missing");
        for bad in [missing.as_os_str(), dir.path().as_os_str(), OsStr::new("bin/opencode")] {
            assert_eq!(
                resolve_override("opencode", Some(bad), std::slice::from_ref(&bundled)),
                Some((bundled.clone(), OpencodeSource::Bundled)),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn bundle_used_when_no_env() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        let bundled = file_in(&dir, "bundled-bin");
        assert_eq!(
            resolve_override("opencode", None, &[missing, bundled.clone()]),
            Some((bundled, OpencodeSource::Bundled))
        );
    }

    #[test]
    fn none_when_nothing_exists() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(resolve_override("opencode", None, &[dir.path().join("missing")]), None);
        assert_eq!(resolve_override("opencode", None, &[]), None);
    }
}
