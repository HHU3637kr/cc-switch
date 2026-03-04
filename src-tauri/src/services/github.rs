//! GitHub REST API transport layer.
//!
//! Provides low-level HTTP operations against the GitHub Contents API and
//! Git Blobs API, analogous to [`super::webdav`] for WebDAV.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use reqwest::header::{ACCEPT, AUTHORIZATION, USER_AGENT};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::settings::GitHubSyncSettings;

const GITHUB_API_BASE: &str = "https://api.github.com";
const APP_USER_AGENT: &str = "CC-Switch-Sync/1.0";
const MAX_CONTENTS_API_SIZE: u64 = 100 * 1024 * 1024; // 100 MB upload limit

/// Represents a file retrieved from GitHub Contents API.
#[derive(Debug, Clone)]
pub struct GitHubFile {
    pub content: Vec<u8>,
    pub sha: String,
    pub size: u64,
}

/// Metadata returned by the Contents API (without decoded content).
#[derive(Debug, Deserialize)]
struct ContentsResponse {
    sha: String,
    size: u64,
    content: Option<String>,
    encoding: Option<String>,
}

/// Response from PUT /repos/{owner}/{repo}/contents/{path}
#[derive(Debug, Deserialize)]
struct PutContentsResponse {
    content: PutContentInfo,
}

#[derive(Debug, Deserialize)]
struct PutContentInfo {
    sha: String,
}

/// Response from GET /repos/{owner}/{repo}/git/blobs/{sha}
#[derive(Debug, Deserialize)]
struct BlobResponse {
    content: String,
    encoding: String,
    size: u64,
}

/// Request body for PUT /repos/{owner}/{repo}/contents/{path}
#[derive(Debug, Serialize)]
struct PutContentsRequest {
    message: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    sha: Option<String>,
    branch: String,
}

fn localized(key: &'static str, zh: impl Into<String>, en: impl Into<String>) -> AppError {
    AppError::localized(key, zh, en)
}

fn build_client() -> Result<reqwest::Client, AppError> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| {
            localized(
                "github.client_build_failed",
                format!("创建 HTTP 客户端失败: {e}"),
                format!("Failed to build HTTP client: {e}"),
            )
        })
}

fn auth_header(token: &str) -> String {
    format!("Bearer {token}")
}

fn contents_url(repo: &str, path: &str, branch: &str) -> String {
    let path = path.trim_start_matches('/');
    format!("{GITHUB_API_BASE}/repos/{repo}/contents/{path}?ref={branch}")
}

fn blob_url(repo: &str, sha: &str) -> String {
    format!("{GITHUB_API_BASE}/repos/{repo}/git/blobs/{sha}")
}

fn repo_url(repo: &str) -> String {
    format!("{GITHUB_API_BASE}/repos/{repo}")
}

fn branch_url(repo: &str, branch: &str) -> String {
    format!("{GITHUB_API_BASE}/repos/{repo}/branches/{branch}")
}

fn redact_token(token: &str) -> String {
    if token.len() <= 8 {
        "***".to_string()
    } else {
        format!("{}...{}", &token[..4], &token[token.len() - 4..])
    }
}

/// Test GitHub connectivity: verify token, repo, and branch.
pub async fn test_connection(settings: &GitHubSyncSettings) -> Result<(), AppError> {
    settings.validate()?;
    let client = build_client()?;
    let token = &settings.token;

    log::debug!(
        "[GitHub] Testing connection to repo={}, branch={}, token={}",
        settings.repo,
        settings.branch,
        redact_token(token),
    );

    // 1. Verify token + repo access
    let resp = client
        .get(repo_url(&settings.repo))
        .header(AUTHORIZATION, auth_header(token))
        .header(USER_AGENT, APP_USER_AGENT)
        .header(ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| {
            localized(
                "github.connection_failed",
                format!("连接 GitHub 失败: {e}"),
                format!("Failed to connect to GitHub: {e}"),
            )
        })?;

    match resp.status() {
        StatusCode::OK => {}
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            return Err(localized(
                "github.auth_failed",
                "GitHub Token 无效或权限不足，请检查 Token 是否拥有 repo 权限",
                "GitHub Token is invalid or has insufficient permissions. Ensure it has the 'repo' scope.",
            ));
        }
        StatusCode::NOT_FOUND => {
            return Err(localized(
                "github.repo_not_found",
                format!("仓库 {} 不存在或不可访问", settings.repo),
                format!("Repository {} not found or inaccessible", settings.repo),
            ));
        }
        status => {
            let body = resp.text().await.unwrap_or_default();
            return Err(localized(
                "github.repo_check_failed",
                format!("检查仓库失败 (HTTP {status}): {body}"),
                format!("Failed to check repository (HTTP {status}): {body}"),
            ));
        }
    }

    // 2. Verify branch exists
    let resp = client
        .get(branch_url(&settings.repo, &settings.branch))
        .header(AUTHORIZATION, auth_header(token))
        .header(USER_AGENT, APP_USER_AGENT)
        .header(ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| {
            localized(
                "github.branch_check_failed",
                format!("检查分支失败: {e}"),
                format!("Failed to check branch: {e}"),
            )
        })?;

    match resp.status() {
        StatusCode::OK => Ok(()),
        StatusCode::NOT_FOUND => Err(localized(
            "github.branch_not_found",
            format!("分支 {} 不存在", settings.branch),
            format!("Branch {} does not exist", settings.branch),
        )),
        status => {
            let body = resp.text().await.unwrap_or_default();
            Err(localized(
                "github.branch_check_failed",
                format!("检查分支失败 (HTTP {status}): {body}"),
                format!("Failed to check branch (HTTP {status}): {body}"),
            ))
        }
    }
}

/// Get file content + blob SHA from GitHub.
///
/// Returns `None` if the file does not exist (HTTP 404).
/// For files > 1 MB, automatically falls back to the Blobs API.
pub async fn get_file(
    settings: &GitHubSyncSettings,
    path: &str,
) -> Result<Option<GitHubFile>, AppError> {
    let client = build_client()?;
    let url = contents_url(&settings.repo, path, &settings.branch);

    let resp = client
        .get(&url)
        .header(AUTHORIZATION, auth_header(&settings.token))
        .header(USER_AGENT, APP_USER_AGENT)
        .header(ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| {
            localized(
                "github.get_file_failed",
                format!("获取文件失败: {e}"),
                format!("Failed to get file: {e}"),
            )
        })?;

    match resp.status() {
        StatusCode::NOT_FOUND => return Ok(None),
        StatusCode::OK => {}
        status => {
            let body = resp.text().await.unwrap_or_default();
            return Err(localized(
                "github.get_file_failed",
                format!("获取文件失败 (HTTP {status}): {body}"),
                format!("Failed to get file (HTTP {status}): {body}"),
            ));
        }
    }

    let meta: ContentsResponse = resp.json().await.map_err(|e| {
        localized(
            "github.parse_response_failed",
            format!("解析 GitHub 响应失败: {e}"),
            format!("Failed to parse GitHub response: {e}"),
        )
    })?;

    // Small file: decode base64 content from Contents API
    if meta.size <= 1_000_000 {
        if let Some(ref encoded) = meta.content {
            let cleaned: String = encoded.chars().filter(|c| !c.is_whitespace()).collect();
            let bytes = BASE64.decode(&cleaned).map_err(|e| {
                localized(
                    "github.base64_decode_failed",
                    format!("Base64 解码失败: {e}"),
                    format!("Base64 decode failed: {e}"),
                )
            })?;
            return Ok(Some(GitHubFile {
                content: bytes,
                sha: meta.sha,
                size: meta.size,
            }));
        }
    }

    // Large file (> 1 MB): use Blobs API
    let bytes = get_blob(&client, settings, &meta.sha).await?;
    Ok(Some(GitHubFile {
        size: bytes.len() as u64,
        content: bytes,
        sha: meta.sha,
    }))
}

/// Create or update a file via the Contents API.
///
/// - `blob_sha`: must be provided when updating an existing file.
/// - Returns the new blob SHA after the commit.
pub async fn put_file(
    settings: &GitHubSyncSettings,
    path: &str,
    content: &[u8],
    blob_sha: Option<&str>,
    commit_message: &str,
) -> Result<String, AppError> {
    if content.len() as u64 > MAX_CONTENTS_API_SIZE {
        let max_mb = MAX_CONTENTS_API_SIZE / 1024 / 1024;
        return Err(localized(
            "github.file_too_large",
            format!("文件大小超过 GitHub 上限（{max_mb} MB）"),
            format!("File size exceeds GitHub limit ({max_mb} MB)"),
        ));
    }

    let client = build_client()?;
    // PUT URL does not need ?ref= query param; branch is specified in the body
    let path_trimmed = path.trim_start_matches('/');
    let url = format!(
        "{GITHUB_API_BASE}/repos/{}/contents/{path_trimmed}",
        settings.repo
    );

    let body = PutContentsRequest {
        message: commit_message.to_string(),
        content: BASE64.encode(content),
        sha: blob_sha.map(|s| s.to_string()),
        branch: settings.branch.clone(),
    };

    let resp = client
        .put(&url)
        .header(AUTHORIZATION, auth_header(&settings.token))
        .header(USER_AGENT, APP_USER_AGENT)
        .header(ACCEPT, "application/vnd.github+json")
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            localized(
                "github.put_file_failed",
                format!("上传文件失败: {e}"),
                format!("Failed to upload file: {e}"),
            )
        })?;

    match resp.status() {
        StatusCode::OK | StatusCode::CREATED => {}
        StatusCode::CONFLICT => {
            return Err(localized(
                "github.sha_conflict",
                "文件 SHA 冲突：远端文件已被修改，请先执行下载同步",
                "File SHA conflict: remote file has been modified. Please download first.",
            ));
        }
        StatusCode::UNPROCESSABLE_ENTITY => {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(localized(
                "github.put_file_failed",
                format!("上传文件失败 (422): {body_text}"),
                format!("Failed to upload file (422): {body_text}"),
            ));
        }
        status => {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(localized(
                "github.put_file_failed",
                format!("上传文件失败 (HTTP {status}): {body_text}"),
                format!("Failed to upload file (HTTP {status}): {body_text}"),
            ));
        }
    }

    let put_resp: PutContentsResponse = resp.json().await.map_err(|e| {
        localized(
            "github.parse_response_failed",
            format!("解析上传响应失败: {e}"),
            format!("Failed to parse upload response: {e}"),
        )
    })?;

    Ok(put_resp.content.sha)
}

/// Get only the blob SHA of a file (lightweight, no content download).
///
/// Returns `None` if the file does not exist.
pub async fn head_file_sha(
    settings: &GitHubSyncSettings,
    path: &str,
) -> Result<Option<String>, AppError> {
    let client = build_client()?;
    let url = contents_url(&settings.repo, path, &settings.branch);

    let resp = client
        .get(&url)
        .header(AUTHORIZATION, auth_header(&settings.token))
        .header(USER_AGENT, APP_USER_AGENT)
        .header(ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| {
            localized(
                "github.head_file_failed",
                format!("获取文件信息失败: {e}"),
                format!("Failed to get file info: {e}"),
            )
        })?;

    match resp.status() {
        StatusCode::NOT_FOUND => Ok(None),
        StatusCode::OK => {
            let meta: ContentsResponse = resp.json().await.map_err(|e| {
                localized(
                    "github.parse_response_failed",
                    format!("解析响应失败: {e}"),
                    format!("Failed to parse response: {e}"),
                )
            })?;
            Ok(Some(meta.sha))
        }
        status => {
            let body = resp.text().await.unwrap_or_default();
            Err(localized(
                "github.head_file_failed",
                format!("获取文件信息失败 (HTTP {status}): {body}"),
                format!("Failed to get file info (HTTP {status}): {body}"),
            ))
        }
    }
}

// ─── Internal helpers ────────────────────────────────────────

/// Download a blob via the Git Blobs API (for files > 1 MB).
async fn get_blob(
    client: &reqwest::Client,
    settings: &GitHubSyncSettings,
    sha: &str,
) -> Result<Vec<u8>, AppError> {
    let url = blob_url(&settings.repo, sha);

    let resp = client
        .get(&url)
        .header(AUTHORIZATION, auth_header(&settings.token))
        .header(USER_AGENT, APP_USER_AGENT)
        .header(ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| {
            localized(
                "github.blob_download_failed",
                format!("下载大文件失败: {e}"),
                format!("Failed to download blob: {e}"),
            )
        })?;

    if resp.status() != StatusCode::OK {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(localized(
            "github.blob_download_failed",
            format!("下载大文件失败 (HTTP {status}): {body}"),
            format!("Failed to download blob (HTTP {status}): {body}"),
        ));
    }

    let blob: BlobResponse = resp.json().await.map_err(|e| {
        localized(
            "github.parse_response_failed",
            format!("解析 Blob 响应失败: {e}"),
            format!("Failed to parse blob response: {e}"),
        )
    })?;

    if blob.encoding != "base64" {
        return Err(localized(
            "github.unsupported_encoding",
            format!("不支持的 Blob 编码: {}", blob.encoding),
            format!("Unsupported blob encoding: {}", blob.encoding),
        ));
    }

    let cleaned: String = blob.content.chars().filter(|c| !c.is_whitespace()).collect();
    BASE64.decode(&cleaned).map_err(|e| {
        localized(
            "github.base64_decode_failed",
            format!("Base64 解码失败: {e}"),
            format!("Base64 decode failed: {e}"),
        )
    })
}

// ─── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contents_url_builds_correctly() {
        let url = contents_url("user/repo", "path/to/file.json", "main");
        assert_eq!(
            url,
            "https://api.github.com/repos/user/repo/contents/path/to/file.json?ref=main"
        );
    }

    #[test]
    fn contents_url_strips_leading_slash() {
        let url = contents_url("user/repo", "/path/file.json", "main");
        assert_eq!(
            url,
            "https://api.github.com/repos/user/repo/contents/path/file.json?ref=main"
        );
    }

    #[test]
    fn blob_url_builds_correctly() {
        let url = blob_url("user/repo", "abc123");
        assert_eq!(
            url,
            "https://api.github.com/repos/user/repo/git/blobs/abc123"
        );
    }

    #[test]
    fn redact_token_hides_middle() {
        assert_eq!(redact_token("ghp_1234567890abcdef"), "ghp_...cdef");
    }

    #[test]
    fn redact_token_short_input() {
        assert_eq!(redact_token("short"), "***");
    }

    #[test]
    fn auth_header_format() {
        assert_eq!(auth_header("my-token"), "Bearer my-token");
    }
}
