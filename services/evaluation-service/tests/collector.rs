//! PVC Collector adversarial and determinism tests.

use std::fs;

use contracts::parse_strict_json;
use contracts::submission::SubmissionManifest;
#[cfg(unix)]
use evaluation_service::SnapshotSource;
use evaluation_service::{CollectError, CollectorLimits, PvcSnapshotSource, SnapshotCollector};
use persistence_sqlx::Sha256Digest;
use tempfile::tempdir;

fn manifest(
    max_total_bytes: u64,
    max_files: u32,
) -> Result<SubmissionManifest, Box<dyn std::error::Error>> {
    let json = format!(
        r#"{{
          "apiVersion":"evaluation.labweaver.io/v1",
          "kind":"SubmissionManifest",
          "name":"workspace",
          "source":"workspace",
          "include":[
            {{"kind":"directoryTree","path":"src"}},
            {{"kind":"exactFile","path":"README.md"}}
          ],
          "exclude":[{{"kind":"directoryTree","path":"src/generated"}}],
          "required":[{{"kind":"exactFile","path":"src/main.rs"}}],
          "llmReadable":[{{"kind":"exactFile","path":"src/main.rs"}}],
          "maxTotalBytes":{max_total_bytes},
          "maxFiles":{max_files},
          "followSymlinks":false
        }}"#
    );
    Ok(parse_strict_json(json.as_bytes())?)
}

fn fixture() -> Result<tempfile::TempDir, Box<dyn std::error::Error>> {
    let directory = tempdir()?;
    fs::create_dir_all(directory.path().join("src/generated"))?;
    fs::write(directory.path().join("src/main.rs"), b"fn main() {}\n")?;
    fs::write(directory.path().join("src/empty.txt"), b"")?;
    fs::write(directory.path().join("src/generated/ignored.rs"), b"secret")?;
    fs::write(directory.path().join("README.md"), b"hello\n")?;
    Ok(directory)
}

#[tokio::test]
async fn pvc_preflight_and_freeze_are_deterministic_and_include_empty_files()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = fixture()?;
    let source = PvcSnapshotSource::open(
        directory.path(),
        Sha256Digest::of_bytes(b"pvc:course:environment:revision"),
    )?;
    let manifest = manifest(1_024, 10)?;
    let collector = SnapshotCollector::default();
    let preflight = collector.preflight(&source, &manifest).await?;
    assert_eq!(
        preflight
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        vec!["README.md", "src/empty.txt", "src/main.rs"]
    );
    assert_eq!(preflight.files[1].size_bytes, 0);
    assert!(
        preflight
            .files
            .iter()
            .all(|file| file.path != "src/generated/ignored.rs")
    );
    let frozen = collector.freeze(&source, &manifest, &preflight).await?;
    assert_eq!(frozen.files, preflight.files);
    assert_eq!(frozen.manifest_sha256, preflight.manifest_sha256);
    assert_eq!(Sha256Digest::of_bytes(&frozen.bytes), frozen.sha256);
    assert!(!frozen.bytes.is_empty());
    Ok(())
}

#[tokio::test]
async fn freeze_rejects_source_change_file_and_byte_limits()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = fixture()?;
    let source = PvcSnapshotSource::open(directory.path(), Sha256Digest::of_bytes(b"pvc"))?;
    let collector = SnapshotCollector::default();
    let normal_manifest = manifest(1_024, 10)?;
    let preflight = collector.preflight(&source, &normal_manifest).await?;
    fs::write(directory.path().join("src/main.rs"), b"changed\n")?;
    assert_eq!(
        collector
            .freeze(&source, &normal_manifest, &preflight)
            .await,
        Err(CollectError::SourceChanged)
    );
    assert_eq!(
        collector.preflight(&source, &manifest(4, 10)?).await,
        Err(CollectError::ByteLimitExceeded)
    );
    assert_eq!(
        collector.preflight(&source, &manifest(1_024, 2)?).await,
        Err(CollectError::FileLimitExceeded)
    );
    assert_eq!(
        collector
            .preflight(&source, &manifest(65 * 1024 * 1024, 10)?)
            .await,
        Err(CollectError::ByteLimitExceeded)
    );
    Ok(())
}

#[tokio::test]
async fn freeze_rejects_canonical_output_over_the_service_limit()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = fixture()?;
    fs::write(directory.path().join("README.md"), vec![b'x'; 900])?;
    let source = PvcSnapshotSource::open(directory.path(), Sha256Digest::of_bytes(b"pvc"))?;
    let collector = SnapshotCollector::new(CollectorLimits {
        max_source_bytes: 1_024,
        max_archive_bytes: 1_024,
        max_files: 10,
    })?;
    let manifest = manifest(1_024, 10)?;
    let preflight = collector.preflight(&source, &manifest).await?;
    assert_eq!(
        collector.freeze(&source, &manifest, &preflight).await,
        Err(CollectError::OutputLimitExceeded)
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn pvc_collection_rejects_symlinks_even_when_the_target_stays_under_root()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::symlink;

    let directory = fixture()?;
    symlink("main.rs", directory.path().join("src/link.rs"))?;
    symlink("src", directory.path().join("src-alias"))?;
    let source = PvcSnapshotSource::open(directory.path(), Sha256Digest::of_bytes(b"pvc"))?;
    assert_eq!(
        SnapshotSource::validate_path(&source, "src-alias/main.rs").await,
        Err(CollectError::SymlinkRejected)
    );
    assert_eq!(
        SnapshotCollector::default()
            .preflight(&source, &manifest(1_024, 10)?)
            .await,
        Err(CollectError::SymlinkRejected)
    );
    Ok(())
}
