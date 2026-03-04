#![allow(non_snake_case)]

use serde_json::{json, Value};
use tauri::State;

use crate::commands::sync_support::{
    attach_warning, post_sync_warning_from_result, run_post_import_sync,
};
use crate::error::AppError;
use crate::services::github_sync as github_sync_service;
use crate::settings::{self, GitHubSyncSettings};
use crate::store::AppState;

fn persist_sync_error(settings: &mut GitHubSyncSettings, error: &AppError, source: &str) {
    settings.status.last_error = Some(error.to_string());
    settings.status.last_error_source = Some(source.to_string());
    let _ = settings::update_github_sync_status(settings.status.clone());
}

fn github_not_configured_error() -> String {
    AppError::localized(
        "github.sync.not_configured",
        "未配置 GitHub 同步",
        "GitHub sync is not configured.",
    )
    .to_string()
}

fn github_sync_disabled_error() -> String {
    AppError::localized(
        "github.sync.disabled",
        "GitHub 同步未启用",
        "GitHub sync is disabled.",
    )
    .to_string()
}

fn require_enabled_github_settings() -> Result<GitHubSyncSettings, String> {
    let settings =
        settings::get_github_sync_settings().ok_or_else(github_not_configured_error)?;
    if !settings.enabled {
        return Err(github_sync_disabled_error());
    }
    Ok(settings)
}

fn resolve_token_for_request(
    mut incoming: GitHubSyncSettings,
    existing: Option<GitHubSyncSettings>,
    preserve_empty_token: bool,
) -> GitHubSyncSettings {
    if let Some(existing_settings) = existing {
        if preserve_empty_token && incoming.token.is_empty() {
            incoming.token = existing_settings.token;
        }
    }
    incoming
}

async fn run_with_github_lock<T, Fut>(operation: Fut) -> Result<T, AppError>
where
    Fut: std::future::Future<Output = Result<T, AppError>>,
{
    github_sync_service::run_with_sync_lock(operation).await
}

fn map_sync_result<T, F>(result: Result<T, AppError>, on_error: F) -> Result<T, String>
where
    F: FnOnce(&AppError),
{
    match result {
        Ok(value) => Ok(value),
        Err(err) => {
            on_error(&err);
            Err(err.to_string())
        }
    }
}

#[tauri::command]
pub async fn github_test_connection(
    settings: GitHubSyncSettings,
    #[allow(non_snake_case)] preserveEmptyToken: Option<bool>,
) -> Result<Value, String> {
    let preserve_empty = preserveEmptyToken.unwrap_or(true);
    let resolved = resolve_token_for_request(
        settings,
        settings::get_github_sync_settings(),
        preserve_empty,
    );
    github_sync_service::check_connection(&resolved)
        .await
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "success": true,
        "message": "GitHub connection ok"
    }))
}

#[tauri::command]
pub async fn github_sync_upload(state: State<'_, AppState>) -> Result<Value, String> {
    let db = state.db.clone();
    let mut settings = require_enabled_github_settings()?;

    let result = run_with_github_lock(github_sync_service::upload(&db, &mut settings)).await;
    map_sync_result(result, |error| {
        persist_sync_error(&mut settings, error, "manual")
    })
}

#[tauri::command]
pub async fn github_sync_download(state: State<'_, AppState>) -> Result<Value, String> {
    let db = state.db.clone();
    let db_for_sync = db.clone();
    let mut settings = require_enabled_github_settings()?;
    let _auto_sync_suppression =
        crate::services::webdav_auto_sync::AutoSyncSuppressionGuard::new();

    let sync_result =
        run_with_github_lock(github_sync_service::download(&db, &mut settings)).await;
    let mut result = map_sync_result(sync_result, |error| {
        persist_sync_error(&mut settings, error, "manual")
    })?;

    // Post-download sync is best-effort: snapshot restore has already succeeded.
    let warning = post_sync_warning_from_result(
        tauri::async_runtime::spawn_blocking(move || run_post_import_sync(db_for_sync))
            .await
            .map_err(|e| e.to_string()),
    );
    if let Some(msg) = warning.as_ref() {
        log::warn!("[GitHub] post-download sync warning: {msg}");
    }
    result = attach_warning(result, warning);

    Ok(result)
}

#[tauri::command]
pub async fn github_sync_save_settings(
    settings: GitHubSyncSettings,
    #[allow(non_snake_case)] tokenTouched: Option<bool>,
) -> Result<Value, String> {
    let token_touched = tokenTouched.unwrap_or(false);
    let existing = settings::get_github_sync_settings();
    let mut sync_settings =
        resolve_token_for_request(settings, existing.clone(), !token_touched);

    // Preserve server-owned fields that the frontend does not manage
    if let Some(existing_settings) = existing {
        sync_settings.status = existing_settings.status;
    }

    // Mutual exclusion: if enabling GitHub sync, disable WebDAV sync
    if sync_settings.enabled {
        if let Some(mut webdav) = settings::get_webdav_sync_settings() {
            if webdav.enabled {
                webdav.enabled = false;
                webdav.auto_sync = false;
                settings::set_webdav_sync_settings(Some(webdav)).map_err(|e| e.to_string())?;
                log::info!("[GitHub] Disabled WebDAV sync due to mutual exclusion");
            }
        }
    }

    sync_settings.normalize();
    sync_settings.validate().map_err(|e| e.to_string())?;
    settings::set_github_sync_settings(Some(sync_settings)).map_err(|e| e.to_string())?;
    Ok(json!({ "success": true }))
}

#[tauri::command]
pub async fn github_sync_fetch_remote_info() -> Result<Value, String> {
    let settings = require_enabled_github_settings()?;
    let info = github_sync_service::fetch_remote_info(&settings)
        .await
        .map_err(|e| e.to_string())?;
    Ok(info.unwrap_or(json!({ "empty": true })))
}
