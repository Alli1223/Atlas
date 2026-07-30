//! Admin-only endpoints: system telemetry and self-update management.
//!
//! All three routes require [`RequireAdmin`]; the project-access layer is told
//! [`Scope::Unscoped`] because the instance role check lives in the handler
//! signature, not in the layer.

use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use sysinfo::{Disks, System};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::api::AppState;
use crate::auth::extract::RequireAdmin;
use crate::error::{AppError, AppResult, Problem};

// ── system stats ─────────────────────────────────────────────────────────────

/// Point-in-time host resource usage.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SystemStats {
    /// CPU utilisation across all cores, 0–100.
    pub cpu_usage_percent: f32,
    /// Total physical memory, in bytes.
    pub memory_total_bytes: u64,
    /// Memory currently in use, in bytes.
    pub memory_used_bytes: u64,
    /// Total capacity of the filesystem that holds the data directory, in bytes.
    pub disk_total_bytes: u64,
    /// Used capacity of that filesystem, in bytes.
    pub disk_used_bytes: u64,
}

/// Returns current CPU, memory and disk usage. Admin only.
#[utoipa::path(
    get,
    path = "/admin/system",
    tag = "admin",
    responses(
        (status = 200, description = "Current system statistics", body = SystemStats),
        (status = 401, description = "Not authenticated", body = Problem),
        (status = 403, description = "Forbidden — admins only", body = Problem),
    )
)]
async fn get_system(_: RequireAdmin, State(state): State<AppState>) -> AppResult<Json<SystemStats>> {
    // CPU requires two samples separated by a short interval.
    let mut sys = System::new();
    sys.refresh_cpu_all();
    tokio::time::sleep(Duration::from_millis(250)).await;
    sys.refresh_cpu_all();
    sys.refresh_memory();

    let disks = Disks::new_with_refreshed_list();
    let data = state.config.data_dir.to_string_lossy();

    // Pick the disk whose mount point is the longest prefix of the data dir,
    // so /data beats / when both are present.
    let (disk_total, disk_used) = disks
        .iter()
        .filter(|d| data.starts_with(d.mount_point().to_string_lossy().as_ref()))
        .max_by_key(|d| d.mount_point().to_string_lossy().len())
        .map(|d| (d.total_space(), d.total_space() - d.available_space()))
        .unwrap_or((0, 0));

    Ok(Json(SystemStats {
        cpu_usage_percent: sys.global_cpu_usage(),
        memory_total_bytes: sys.total_memory(),
        memory_used_bytes: sys.used_memory(),
        disk_total_bytes: disk_total,
        disk_used_bytes: disk_used,
    }))
}

// ── update check ─────────────────────────────────────────────────────────────

/// Result of polling GitHub Releases for a newer version.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    /// The version currently running.
    pub current_version: String,
    /// The latest published release tag (without the `v` prefix), if reachable.
    pub latest_version: Option<String>,
    /// Whether `latest_version` is strictly newer than `current_version`.
    pub has_update: bool,
    /// URL of the GitHub release page.
    pub release_url: Option<String>,
    /// Markdown release notes from the GitHub release body.
    pub release_notes: Option<String>,
    /// Set when the check could not be completed (network error, private repo, etc.).
    pub error: Option<String>,
}

/// The subset of fields Atlas reads from a GitHub Releases API response.
#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
    body: Option<String>,
}

/// Polls GitHub Releases for a version newer than the one currently running.
#[utoipa::path(
    get,
    path = "/admin/updates",
    tag = "admin",
    responses(
        (status = 200, description = "Update status", body = UpdateStatus),
        (status = 401, description = "Not authenticated", body = Problem),
        (status = 403, description = "Forbidden — admins only", body = Problem),
    )
)]
async fn check_updates(_: RequireAdmin) -> AppResult<Json<UpdateStatus>> {
    let current = env!("CARGO_PKG_VERSION");
    let repo_url = env!("CARGO_PKG_REPOSITORY");

    let Some(path) = repo_url.strip_prefix("https://github.com/") else {
        return Ok(Json(UpdateStatus {
            current_version: current.to_owned(),
            latest_version: None,
            has_update: false,
            release_url: None,
            release_notes: None,
            error: Some("Repository is not on GitHub; cannot check for updates.".to_owned()),
        }));
    };

    let api_url = format!("https://api.github.com/repos/{path}/releases/latest");

    let client = reqwest::Client::builder()
        .user_agent(concat!("atlas/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| AppError::internal(anyhow::anyhow!("failed to build HTTP client: {e}")))?;

    let resp = match client
        .get(&api_url)
        .header("Accept", "application/vnd.github.v3+json")
        .send()
        .await
    {
        Err(e) => {
            return Ok(Json(UpdateStatus {
                current_version: current.to_owned(),
                latest_version: None,
                has_update: false,
                release_url: None,
                release_notes: None,
                error: Some(format!("Could not reach GitHub: {e}")),
            }));
        }
        Ok(r) => r,
    };

    if !resp.status().is_success() {
        let status = resp.status();
        return Ok(Json(UpdateStatus {
            current_version: current.to_owned(),
            latest_version: None,
            has_update: false,
            release_url: None,
            release_notes: None,
            error: Some(format!(
                "GitHub returned {status}. The repository may be private or have no published releases."
            )),
        }));
    }

    let release: GitHubRelease = resp
        .json()
        .await
        .map_err(|e| AppError::internal(anyhow::anyhow!("failed to parse GitHub response: {e}")))?;

    let latest = release.tag_name.trim_start_matches('v').to_owned();
    let has_update = is_newer(&latest, current);

    Ok(Json(UpdateStatus {
        current_version: current.to_owned(),
        latest_version: Some(latest),
        has_update,
        release_url: Some(release.html_url),
        release_notes: release.body,
        error: None,
    }))
}

/// Returns `true` when `candidate` is a strictly higher semver than `current`.
fn is_newer(candidate: &str, current: &str) -> bool {
    fn parse(s: &str) -> Option<(u32, u32, u32)> {
        let mut it = s.trim_start_matches('v').splitn(3, '.');
        Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?, it.next()?.parse().ok()?))
    }
    matches!((parse(candidate), parse(current)), (Some(c), Some(cur)) if c > cur)
}

// ── apply update ─────────────────────────────────────────────────────────────

/// Confirmation that an update has been queued.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApplyUpdateResponse {
    /// Human-readable message for the admin.
    pub message: String,
}

/// Queues a self-update by writing a trigger file the host-side update service watches for.
///
/// The rebuild and restart are asynchronous. Atlas will be briefly unavailable while the
/// new container starts. Follow progress with: `journalctl -fu atlas-update`
#[utoipa::path(
    post,
    path = "/admin/updates/apply",
    tag = "admin",
    responses(
        (status = 202, description = "Update queued", body = ApplyUpdateResponse),
        (status = 401, description = "Not authenticated", body = Problem),
        (status = 403, description = "Forbidden — admins only", body = Problem),
        (status = 500, description = "Could not write the trigger file", body = Problem),
    )
)]
async fn apply_update(
    _: RequireAdmin,
    State(state): State<AppState>,
) -> AppResult<(StatusCode, Json<ApplyUpdateResponse>)> {
    let trigger = state.config.data_dir.join("update-requested");

    tokio::fs::write(&trigger, b"")
        .await
        .map_err(|e| AppError::internal(anyhow::anyhow!("failed to write update trigger at {}: {e}", trigger.display())))?;

    tracing::info!(trigger = %trigger.display(), "update trigger written — host service will rebuild and restart Atlas");

    Ok((
        StatusCode::ACCEPTED,
        Json(ApplyUpdateResponse {
            message: "Update queued. Atlas will pull the latest code, rebuild, and restart. \
                      This takes a few minutes. Follow progress with: journalctl -fu atlas-update"
                .to_owned(),
        }),
    ))
}

// ── router ────────────────────────────────────────────────────────────────────

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_system))
        .routes(routes!(check_updates))
        .routes(routes!(apply_update))
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_newer_compares_semver_numerically_not_lexically() {
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
    }

    #[test]
    fn is_newer_strips_the_v_prefix() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("0.2.0", "v0.1.0"));
    }

    #[test]
    fn is_newer_handles_bad_input_gracefully() {
        assert!(!is_newer("not-a-version", "0.1.0"));
        assert!(!is_newer("0.1.0", "not-a-version"));
    }
}
