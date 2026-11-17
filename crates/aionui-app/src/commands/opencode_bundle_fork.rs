// fork(opencode): bundle the pinned opencode during prepare-managed-resources — see docs/fork/opencode-overlay.md

use std::path::Path;

use aionui_runtime::managed_resources_contract::ManagedCliResourceContract;
use aionui_runtime::opencode_fork::bundle::{BundleOutcome, OpencodeBundleError, bundle_opencode};

use super::error::{CliBoundaryCode, CliBoundaryError};

const SUBCOMMAND: &str = "prepare-managed-resources";

/// Returns the `clis` entries opencode contributes to the manifest: one entry
/// when bundled, none when the platform has no locked asset (warning only).
pub(crate) async fn bundle_for_contract(
    output_root: &Path,
) -> Result<Vec<ManagedCliResourceContract>, CliBoundaryError> {
    match bundle_opencode(output_root).await.map_err(bundle_error)? {
        BundleOutcome::Bundled(contract) => {
            println!("  opencode -> {}", output_root.join(&contract.root).display());
            Ok(vec![contract])
        }
        BundleOutcome::SkippedUnsupportedPlatform { runtime_key } => {
            eprintln!("warning: opencode is not bundled: no locked asset for platform {runtime_key}");
            Ok(Vec::new())
        }
    }
}

fn bundle_error(error: OpencodeBundleError) -> CliBoundaryError {
    eprintln!(
        "prepare-managed-resources stage=opencode.{} detail: {}",
        error.stage, error.message
    );
    stage_error(error.stage)
}

fn stage_error(stage: &'static str) -> CliBoundaryError {
    let stage = match stage {
        "download" => "opencode.download",
        "checksum" => "opencode.checksum",
        "extract" => "opencode.extract",
        "verify" => "opencode.verify",
        _ => "opencode.export",
    };
    CliBoundaryError::new(
        CliBoundaryCode::CliPrepareManagedResourcesFailed,
        SUBCOMMAND,
        "failed to prepare managed resources",
    )
    .with_field("stage", stage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_error_maps_to_stable_stage_without_raw_path() {
        for (stage, expected) in [
            ("download", "opencode.download"),
            ("checksum", "opencode.checksum"),
            ("extract", "opencode.extract"),
            ("verify", "opencode.verify"),
            ("export", "opencode.export"),
        ] {
            let err = bundle_error(OpencodeBundleError {
                stage,
                message: "/Users/secret/bundle".into(),
            });
            assert_eq!(err.code(), CliBoundaryCode::CliPrepareManagedResourcesFailed);
            assert!(err.stderr_line().contains(&format!("stage={expected}")), "{stage}");
            assert!(!err.stderr_line().contains("/Users/secret/bundle"));
        }
    }
}
