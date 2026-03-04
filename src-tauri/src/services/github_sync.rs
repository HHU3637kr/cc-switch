//! GitHub v2 sync protocol layer.
//!
//! Implements manifest-based synchronization on top of the HTTP transport
//! primitives in [`super::github`]. Shares the same artifact set and manifest
//! format as [`super::webdav_sync`]: `db.sql` + `skills.zip`.

use std::collections::BTreeMap;
use std::fs;
use std::future::Future;
use std::process::Command;
use std::sync::OnceLock;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::tempdir;

use crate::error::AppError;
use crate::services::github::{get_file, head_file_sha, put_file, test_connection};
use crate::settings::{update_github_sync_status, GitHubSyncSettings, GitHubSyncStatus};

// Reuse the archive module from webdav_sync (skills zip/unzip)
use crate::services::webdav_sync::archive::{
    backup_current_skills, restore_skills_from_backup, restore_skills_zip, zip_skills_ssot,
};

// ─── Protocol constants ──────────────────────────────────────

const PROTOCOL_FORMAT: &str = "cc-switch-webdav-sync";
const PROTOCOL_VERSION: u32 = 2;
const REMOTE_DB_SQL: &str = "db.sql";
const REMOTE_SKILLS_ZIP: &str = "skills.zip";
const REMOTE_MANIFEST: &str = "manifest.json";
const MAX_DEVICE_NAME_LEN: usize = 64;
pub(super) const MAX_SYNC_ARTIFACT_BYTES: u64 = 100 * 1024 * 1024; // 100 MB (GitHub limit)

pub fn sync_mutex() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

pub async fn run_with_sync_lock<T, Fut>(operation: Fut) -> Result<T, AppError>
where
    Fut: Future<Output = Result<T, AppError>>,
{
    let _guard = sync_mutex().lock().await;
    operation.await
}

fn localized(key: &'static str, zh: impl Into<String>, en: impl Into<String>) -> AppError {
    AppError::localized(key, zh, en)
}

fn io_context_localized(
    _key: &'static str,
    zh: impl Into<String>,
    en: impl Into<String>,
    source: std::io::Error,
) -> AppError {
    let zh_msg = zh.into();
    let en_msg = en.into();
    AppError::IoContext {
        context: format!("{zh_msg} ({en_msg})"),
        source,
    }
}

// ─── Types ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SyncManifest {
    format: String,
    version: u32,
    device_name: String,
    created_at: String,
    artifacts: BTreeMap<String, ArtifactMeta>,
    snapshot_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ArtifactMeta {
    sha256: String,
    size: u64,
}

struct LocalSnapshot {
    db_sql: Vec<u8>,
    skills_zip: Vec<u8>,
    manifest_bytes: Vec<u8>,
    manifest_hash: String,
}

/// Tracks blob SHAs for remote files (needed to update existing files).
struct RemoteShas {
    db_sql: Option<String>,
    skills_zip: Option<String>,
    manifest: Option<String>,
}

// ─── Public API ──────────────────────────────────────────────

/// Check GitHub connectivity and verify repo/branch access.
pub async fn check_connection(settings: &GitHubSyncSettings) -> Result<(), AppError> {
    settings.validate()?;
    test_connection(settings).await
}

/// Upload local snapshot (db + skills) to GitHub.
pub async fn upload(
    db: &crate::database::Database,
    settings: &mut GitHubSyncSettings,
) -> Result<Value, AppError> {
    settings.validate()?;

    let snapshot = build_local_snapshot(db)?;

    // Fetch existing blob SHAs for update operations
    let remote_shas = fetch_remote_shas(settings).await?;

    // Upload order: artifacts first, manifest last (best-effort consistency)
    let db_path = remote_file_path(settings, REMOTE_DB_SQL);
    let _db_sha = put_file(
        settings,
        &db_path,
        &snapshot.db_sql,
        remote_shas.db_sql.as_deref(),
        "sync: update db.sql",
    )
    .await?;

    let skills_path = remote_file_path(settings, REMOTE_SKILLS_ZIP);
    let _skills_sha = put_file(
        settings,
        &skills_path,
        &snapshot.skills_zip,
        remote_shas.skills_zip.as_deref(),
        "sync: update skills.zip",
    )
    .await?;

    let manifest_path = remote_file_path(settings, REMOTE_MANIFEST);
    let manifest_blob_sha = put_file(
        settings,
        &manifest_path,
        &snapshot.manifest_bytes,
        remote_shas.manifest.as_deref(),
        "sync: update manifest.json",
    )
    .await?;

    let _persisted = persist_sync_success_best_effort(
        settings,
        snapshot.manifest_hash,
        Some(manifest_blob_sha),
    );
    Ok(serde_json::json!({ "status": "uploaded" }))
}

/// Download remote snapshot and apply to local database + skills.
pub async fn download(
    db: &crate::database::Database,
    settings: &mut GitHubSyncSettings,
) -> Result<Value, AppError> {
    settings.validate()?;

    let manifest_path = remote_file_path(settings, REMOTE_MANIFEST);
    let manifest_file = get_file(settings, &manifest_path)
        .await?
        .ok_or_else(|| {
            localized(
                "github.sync.remote_empty",
                "远端没有可下载的同步数据",
                "No downloadable sync data found on GitHub.",
            )
        })?;

    let manifest: SyncManifest =
        serde_json::from_slice(&manifest_file.content).map_err(|e| AppError::Json {
            path: REMOTE_MANIFEST.to_string(),
            source: e,
        })?;

    validate_manifest_compat(&manifest)?;

    // Download and verify artifacts
    let db_sql = download_and_verify(settings, REMOTE_DB_SQL, &manifest.artifacts).await?;
    let skills_zip =
        download_and_verify(settings, REMOTE_SKILLS_ZIP, &manifest.artifacts).await?;

    // Apply snapshot
    apply_snapshot(db, &db_sql, &skills_zip)?;

    let manifest_hash = sha256_hex(&manifest_file.content);
    let _persisted = persist_sync_success_best_effort(
        settings,
        manifest_hash,
        Some(manifest_file.sha),
    );
    Ok(serde_json::json!({ "status": "downloaded" }))
}

/// Fetch remote manifest info without downloading artifacts.
pub async fn fetch_remote_info(settings: &GitHubSyncSettings) -> Result<Option<Value>, AppError> {
    settings.validate()?;
    let manifest_path = remote_file_path(settings, REMOTE_MANIFEST);

    let Some(file) = get_file(settings, &manifest_path).await? else {
        return Ok(None);
    };

    let manifest: SyncManifest =
        serde_json::from_slice(&file.content).map_err(|e| AppError::Json {
            path: REMOTE_MANIFEST.to_string(),
            source: e,
        })?;

    let compatible = validate_manifest_compat(&manifest).is_ok();

    let payload = serde_json::json!({
        "deviceName": manifest.device_name,
        "createdAt": manifest.created_at,
        "snapshotId": manifest.snapshot_id,
        "version": manifest.version,
        "compatible": compatible,
        "artifacts": manifest.artifacts.keys().collect::<Vec<_>>(),
    });

    Ok(Some(payload))
}

// ─── Sync status persistence ─────────────────────────────────

fn persist_sync_success(
    settings: &mut GitHubSyncSettings,
    manifest_hash: String,
    manifest_blob_sha: Option<String>,
) -> Result<(), AppError> {
    let status = GitHubSyncStatus {
        last_sync_at: Some(Utc::now().timestamp()),
        last_error: None,
        last_error_source: None,
        last_local_manifest_hash: Some(manifest_hash.clone()),
        last_remote_manifest_hash: Some(manifest_hash),
        last_manifest_blob_sha: manifest_blob_sha,
    };
    settings.status = status.clone();
    update_github_sync_status(status)
}

fn persist_sync_success_best_effort(
    settings: &mut GitHubSyncSettings,
    manifest_hash: String,
    manifest_blob_sha: Option<String>,
) -> bool {
    match persist_sync_success(settings, manifest_hash, manifest_blob_sha) {
        Ok(()) => true,
        Err(err) => {
            log::warn!("[GitHub] Persist sync status failed, keep operation success: {err}");
            false
        }
    }
}

// ─── Snapshot building ───────────────────────────────────────

fn build_local_snapshot(db: &crate::database::Database) -> Result<LocalSnapshot, AppError> {
    // Export database to SQL string
    let sql_string = db.export_sql_string()?;
    let db_sql = sql_string.into_bytes();

    // Pack skills into deterministic ZIP
    let tmp = tempdir().map_err(|e| {
        io_context_localized(
            "github.sync.snapshot_tmpdir_failed",
            "创建 GitHub 快照临时目录失败",
            "Failed to create temporary directory for GitHub snapshot",
            e,
        )
    })?;
    let skills_zip_path = tmp.path().join(REMOTE_SKILLS_ZIP);
    zip_skills_ssot(&skills_zip_path)?;
    let skills_zip = fs::read(&skills_zip_path).map_err(|e| AppError::io(&skills_zip_path, e))?;

    // Validate sizes against GitHub limits
    validate_artifact_size_limit(REMOTE_DB_SQL, db_sql.len() as u64)?;
    validate_artifact_size_limit(REMOTE_SKILLS_ZIP, skills_zip.len() as u64)?;

    // Build artifact map and compute hashes
    let mut artifacts = BTreeMap::new();
    artifacts.insert(
        REMOTE_DB_SQL.to_string(),
        ArtifactMeta {
            sha256: sha256_hex(&db_sql),
            size: db_sql.len() as u64,
        },
    );
    artifacts.insert(
        REMOTE_SKILLS_ZIP.to_string(),
        ArtifactMeta {
            sha256: sha256_hex(&skills_zip),
            size: skills_zip.len() as u64,
        },
    );

    let snapshot_id = compute_snapshot_id(&artifacts);
    let manifest = SyncManifest {
        format: PROTOCOL_FORMAT.to_string(),
        version: PROTOCOL_VERSION,
        device_name: detect_system_device_name().unwrap_or_else(|| "Unknown Device".to_string()),
        created_at: Utc::now().to_rfc3339(),
        artifacts,
        snapshot_id,
    };
    let manifest_bytes =
        serde_json::to_vec_pretty(&manifest).map_err(|e| AppError::JsonSerialize { source: e })?;
    let manifest_hash = sha256_hex(&manifest_bytes);

    Ok(LocalSnapshot {
        db_sql,
        skills_zip,
        manifest_bytes,
        manifest_hash,
    })
}

fn compute_snapshot_id(artifacts: &BTreeMap<String, ArtifactMeta>) -> String {
    let parts: Vec<String> = artifacts
        .iter()
        .map(|(name, meta)| format!("{}:{}", name, meta.sha256))
        .collect();
    sha256_hex(parts.join("|").as_bytes())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn detect_system_device_name() -> Option<String> {
    let env_name = ["CC_SWITCH_DEVICE_NAME", "COMPUTERNAME", "HOSTNAME"]
        .iter()
        .filter_map(|key| std::env::var(key).ok())
        .find_map(|value| normalize_device_name(&value));

    if env_name.is_some() {
        return env_name;
    }

    let output = Command::new("hostname").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let hostname = String::from_utf8(output.stdout).ok()?;
    normalize_device_name(&hostname)
}

fn normalize_device_name(raw: &str) -> Option<String> {
    let compact = raw
        .chars()
        .fold(String::with_capacity(raw.len()), |mut acc, ch| {
            if ch.is_whitespace() {
                acc.push(' ');
            } else if !ch.is_control() {
                acc.push(ch);
            }
            acc
        });
    let normalized = compact.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = normalized.trim();
    if trimmed.is_empty() {
        return None;
    }

    let limited = trimmed
        .chars()
        .take(MAX_DEVICE_NAME_LEN)
        .collect::<String>();
    if limited.is_empty() {
        None
    } else {
        Some(limited)
    }
}

fn validate_manifest_compat(manifest: &SyncManifest) -> Result<(), AppError> {
    if manifest.format != PROTOCOL_FORMAT {
        return Err(localized(
            "github.sync.manifest_format_incompatible",
            format!("远端 manifest 格式不兼容: {}", manifest.format),
            format!(
                "Remote manifest format is incompatible: {}",
                manifest.format
            ),
        ));
    }
    if manifest.version != PROTOCOL_VERSION {
        return Err(localized(
            "github.sync.manifest_version_incompatible",
            format!(
                "远端 manifest 协议版本不兼容: v{} (本地 v{PROTOCOL_VERSION})",
                manifest.version
            ),
            format!(
                "Remote manifest protocol version is incompatible: v{} (local v{PROTOCOL_VERSION})",
                manifest.version
            ),
        ));
    }
    Ok(())
}

// ─── Download & verify ───────────────────────────────────────

async fn download_and_verify(
    settings: &GitHubSyncSettings,
    artifact_name: &str,
    artifacts: &BTreeMap<String, ArtifactMeta>,
) -> Result<Vec<u8>, AppError> {
    let meta = artifacts.get(artifact_name).ok_or_else(|| {
        localized(
            "github.sync.manifest_missing_artifact",
            format!("manifest 中缺少 artifact: {artifact_name}"),
            format!("Manifest missing artifact: {artifact_name}"),
        )
    })?;
    validate_artifact_size_limit(artifact_name, meta.size)?;

    let path = remote_file_path(settings, artifact_name);
    let file = get_file(settings, &path).await?.ok_or_else(|| {
        localized(
            "github.sync.remote_missing_artifact",
            format!("远端缺少 artifact 文件: {artifact_name}"),
            format!("Remote artifact file missing: {artifact_name}"),
        )
    })?;

    // Quick size check before expensive hash
    if file.content.len() as u64 != meta.size {
        return Err(localized(
            "github.sync.artifact_size_mismatch",
            format!(
                "artifact {artifact_name} 大小不匹配 (expected: {}, got: {})",
                meta.size,
                file.content.len(),
            ),
            format!(
                "Artifact {artifact_name} size mismatch (expected: {}, got: {})",
                meta.size,
                file.content.len(),
            ),
        ));
    }

    let actual_hash = sha256_hex(&file.content);
    if actual_hash != meta.sha256 {
        return Err(localized(
            "github.sync.artifact_hash_mismatch",
            format!(
                "artifact {artifact_name} SHA256 校验失败 (expected: {}..., got: {}...)",
                meta.sha256.get(..8).unwrap_or(&meta.sha256),
                actual_hash.get(..8).unwrap_or(&actual_hash),
            ),
            format!(
                "Artifact {artifact_name} SHA256 verification failed (expected: {}..., got: {}...)",
                meta.sha256.get(..8).unwrap_or(&meta.sha256),
                actual_hash.get(..8).unwrap_or(&actual_hash),
            ),
        ));
    }
    Ok(file.content)
}

fn apply_snapshot(
    db: &crate::database::Database,
    db_sql: &[u8],
    skills_zip: &[u8],
) -> Result<(), AppError> {
    let sql_str = std::str::from_utf8(db_sql).map_err(|e| {
        localized(
            "github.sync.sql_not_utf8",
            format!("SQL 非 UTF-8: {e}"),
            format!("SQL is not valid UTF-8: {e}"),
        )
    })?;
    let skills_backup = backup_current_skills()?;

    // 先替换 skills，再导入数据库；若导入失败则回滚 skills，避免"半恢复"。
    restore_skills_zip(skills_zip)?;

    if let Err(db_err) = db.import_sql_string(sql_str) {
        if let Err(rollback_err) = restore_skills_from_backup(&skills_backup) {
            return Err(localized(
                "github.sync.db_import_and_rollback_failed",
                format!("导入数据库失败: {db_err}; 同时回滚 Skills 失败: {rollback_err}"),
                format!(
                    "Database import failed: {db_err}; skills rollback also failed: {rollback_err}"
                ),
            ));
        }
        return Err(db_err);
    }

    Ok(())
}

// ─── Remote path helpers ─────────────────────────────────────

fn remote_file_path(settings: &GitHubSyncSettings, file_name: &str) -> String {
    format!(
        "{}/v{PROTOCOL_VERSION}/{}/{}",
        settings.remote_root.trim_matches('/'),
        settings.profile.trim_matches('/'),
        file_name,
    )
}

/// Fetch existing blob SHAs for all artifacts (needed for PUT updates).
async fn fetch_remote_shas(settings: &GitHubSyncSettings) -> Result<RemoteShas, AppError> {
    let db_path = remote_file_path(settings, REMOTE_DB_SQL);
    let skills_path = remote_file_path(settings, REMOTE_SKILLS_ZIP);
    let manifest_path = remote_file_path(settings, REMOTE_MANIFEST);

    // Execute all three HEAD requests concurrently
    let (db_sha, skills_sha, manifest_sha) = tokio::try_join!(
        head_file_sha(settings, &db_path),
        head_file_sha(settings, &skills_path),
        head_file_sha(settings, &manifest_path),
    )?;

    Ok(RemoteShas {
        db_sql: db_sha,
        skills_zip: skills_sha,
        manifest: manifest_sha,
    })
}

fn validate_artifact_size_limit(artifact_name: &str, size: u64) -> Result<(), AppError> {
    if size > MAX_SYNC_ARTIFACT_BYTES {
        let max_mb = MAX_SYNC_ARTIFACT_BYTES / 1024 / 1024;
        return Err(localized(
            "github.sync.artifact_too_large",
            format!(
                "artifact {artifact_name} 超过 GitHub 上限（{} MB）",
                max_mb
            ),
            format!(
                "Artifact {artifact_name} exceeds GitHub limit ({} MB)",
                max_mb
            ),
        ));
    }
    Ok(())
}

// ─── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(sha256: &str, size: u64) -> ArtifactMeta {
        ArtifactMeta {
            sha256: sha256.to_string(),
            size,
        }
    }

    #[test]
    fn snapshot_id_is_stable() {
        let mut artifacts = BTreeMap::new();
        artifacts.insert("db.sql".to_string(), artifact("abc123", 100));
        artifacts.insert("skills.zip".to_string(), artifact("def456", 200));

        let id1 = compute_snapshot_id(&artifacts);
        let id2 = compute_snapshot_id(&artifacts);
        assert_eq!(id1, id2);
    }

    #[test]
    fn snapshot_id_changes_with_artifacts() {
        let mut a1 = BTreeMap::new();
        a1.insert("db.sql".to_string(), artifact("hash-a", 1));

        let mut a2 = BTreeMap::new();
        a2.insert("db.sql".to_string(), artifact("hash-b", 1));

        assert_ne!(compute_snapshot_id(&a1), compute_snapshot_id(&a2));
    }

    #[test]
    fn remote_file_path_builds_correctly() {
        let settings = GitHubSyncSettings {
            remote_root: "cc-switch-sync".to_string(),
            profile: "default".to_string(),
            ..GitHubSyncSettings::default()
        };
        let path = remote_file_path(&settings, "manifest.json");
        assert_eq!(path, "cc-switch-sync/v2/default/manifest.json");
    }

    #[test]
    fn sha256_hex_is_correct() {
        let hash = sha256_hex(b"hello");
        assert_eq!(
            hash,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    fn manifest_with(format: &str, version: u32) -> SyncManifest {
        let mut artifacts = BTreeMap::new();
        artifacts.insert("db.sql".to_string(), artifact("abc", 1));
        artifacts.insert("skills.zip".to_string(), artifact("def", 2));
        SyncManifest {
            format: format.to_string(),
            version,
            device_name: "My PC".to_string(),
            created_at: "2026-03-04T00:00:00Z".to_string(),
            artifacts,
            snapshot_id: "snap-1".to_string(),
        }
    }

    #[test]
    fn validate_manifest_compat_accepts_supported_manifest() {
        let manifest = manifest_with(PROTOCOL_FORMAT, PROTOCOL_VERSION);
        assert!(validate_manifest_compat(&manifest).is_ok());
    }

    #[test]
    fn validate_manifest_compat_rejects_wrong_format() {
        let manifest = manifest_with("other-format", PROTOCOL_VERSION);
        assert!(validate_manifest_compat(&manifest).is_err());
    }

    #[test]
    fn validate_manifest_compat_rejects_wrong_version() {
        let manifest = manifest_with(PROTOCOL_FORMAT, PROTOCOL_VERSION + 1);
        assert!(validate_manifest_compat(&manifest).is_err());
    }

    #[test]
    fn validate_artifact_size_limit_rejects_oversized_artifacts() {
        let err = validate_artifact_size_limit("skills.zip", MAX_SYNC_ARTIFACT_BYTES + 1)
            .expect_err("artifact larger than limit should be rejected");
        assert!(
            err.to_string().contains("too large")
                || err.to_string().contains("超过")
                || err.to_string().contains("exceeds"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_artifact_size_limit_accepts_limit_boundary() {
        assert!(validate_artifact_size_limit("skills.zip", MAX_SYNC_ARTIFACT_BYTES).is_ok());
    }
}
