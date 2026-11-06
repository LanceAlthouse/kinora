//! Playback routes: decide how to play, direct streams, HLS, subtitles.

use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use kinora_core::hls::{parse_segment_name, vod_playlist, SEGMENT_SECONDS};
use kinora_core::media::{HdrFormat, MediaInfo};
use kinora_core::playback::{decide, ClientCaps, Limits, Method, PlaybackPlan};
use kinora_core::roles::Role;
use kinora_plugin_api::ServerEvent;
use serde::{Deserialize, Serialize};
use tower::ServiceExt;
use tower_http::services::ServeFile;

use crate::auth::AuthUser;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::transcode::{SegmentError, SessionInput};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/playback/{file_id}", post(start))
        .route("/api/stream/{file_id}", get(direct))
        .route("/api/hls/{session}/main.m3u8", get(playlist))
        .route("/api/hls/{session}/{segment}", get(segment))
        .route("/api/hls/{session}", delete(stop))
        .route("/api/subtitles/{file_id}/{index}", get(subtitle))
        .route("/api/sessions", get(active_sessions))
}

struct FileRow {
    item_id: String,
    path: String,
    info: MediaInfo,
}

async fn load_file(state: &AppState, file_id: &str) -> AppResult<FileRow> {
    let (item_id, path, info): (String, String, Option<String>) =
        sqlx::query_as("SELECT item_id, path, info FROM media_files WHERE id = ?")
            .bind(file_id)
            .fetch_one(&state.db)
            .await?;
    let mut info: MediaInfo = info
        .and_then(|i| serde_json::from_str(&i).ok())
        .ok_or_else(|| AppError::BadRequest("file has not been scanned yet".into()))?;
    // ffprobe reports WebM as "matroska"; the extension tells them apart.
    if path.to_lowercase().ends_with(".webm") {
        info.container = "webm".into();
    }
    Ok(FileRow { item_id, path, info })
}

#[derive(Deserialize)]
struct StartRequest {
    caps: ClientCaps,
    #[serde(default)]
    audio_index: Option<u32>,
    #[serde(default)]
    max_bitrate: Option<u64>,
    /// Force a transcode even if direct play would work (for testing / bad files).
    #[serde(default)]
    force_transcode: bool,
}

#[derive(Serialize)]
struct SubtitleTrack {
    index: u32,
    label: String,
    language: Option<String>,
    url: String,
    codec: String,
    is_default: bool,
    is_forced: bool,
    /// Text subtitles can be shown in the browser. Image subtitles (PGS, VobSub)
    /// need to be burned into the video, which isn't built yet, so they're listed but disabled.
    supported: bool,
}

#[derive(Serialize)]
struct StartResponse {
    method: Method,
    /// Relative URL (works behind reverse proxies with a sub-path).
    url: String,
    session_id: Option<String>,
    plan: PlaybackPlan,
    duration_ms: u64,
    subtitles: Vec<SubtitleTrack>,
    hardware: String,
}

async fn start(
    State(state): State<AppState>,
    user: AuthUser,
    Path(file_id): Path<String>,
    Json(req): Json<StartRequest>,
) -> AppResult<Json<StartResponse>> {
    let file = load_file(&state, &file_id).await?;
    let limits = Limits { max_bitrate: req.max_bitrate, audio_index: req.audio_index, ..Default::default() };
    let mut caps = req.caps;
    if req.force_transcode {
        caps.containers.clear();
        caps.video_codecs.retain(|_| false);
    }
    let plan = decide(&file.info, &caps, &limits);

    let subtitles = file
        .info
        .subtitles
        .iter()
        .map(|s| SubtitleTrack {
            index: s.index,
            label: s.title.clone().or_else(|| s.language.clone()).unwrap_or_else(|| format!("Track {}", s.index)),
            language: s.language.clone(),
            url: format!("api/subtitles/{file_id}/{}", s.index),
            codec: s.codec.clone(),
            is_default: s.is_default,
            is_forced: s.is_forced,
            supported: s.is_text,
        })
        .collect();

    let hw = state.hardware.read().await.profile.clone();
    let title: String =
        sqlx::query_scalar("SELECT title FROM items WHERE id = ?").bind(&file.item_id).fetch_one(&state.db).await?;

    state.emit(ServerEvent::PlaybackStarted {
        user_id: user.id.clone(),
        item_id: file.item_id.clone(),
        method: format!("{:?}", plan.method),
    });

    let (url, session_id) = if plan.method == Method::DirectPlay {
        (format!("api/stream/{file_id}"), None)
    } else {
        let v = file.info.video.as_ref();
        let id = state
            .transcodes
            .create(SessionInput {
                user_id: user.id.clone(),
                username: user.username.clone(),
                item_id: file.item_id.clone(),
                title,
                file_path: file.path.clone(),
                duration_ms: file.info.duration_ms,
                video_index: v.map(|v| v.index),
                frame_rate: v.and_then(|v| v.frame_rate),
                source_is_dolby_vision: v.map(|v| v.hdr == HdrFormat::DolbyVision).unwrap_or(false),
                plan: plan.clone(),
                hw: hw.clone(),
            })
            .await?;
        (format!("api/hls/{id}/main.m3u8"), Some(id))
    };

    Ok(Json(StartResponse {
        method: plan.method,
        url,
        session_id,
        duration_ms: file.info.duration_ms,
        plan,
        subtitles,
        hardware: hw.accel.label().to_string(),
    }))
}

/// Serve the original file with HTTP range support (seeking).
async fn direct(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(file_id): Path<String>,
    request: Request,
) -> AppResult<Response> {
    let path: String =
        sqlx::query_scalar("SELECT path FROM media_files WHERE id = ?").bind(&file_id).fetch_one(&state.db).await?;
    let resp = ServeFile::new(&path).oneshot(request).await.map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
    Ok(resp.map(Body::new).into_response())
}

async fn check_session(state: &AppState, user: &AuthUser, session: &str) -> AppResult<()> {
    match state.transcodes.owner(session).await {
        Some(owner) if owner == user.id || user.role.at_least(Role::Admin) => Ok(()),
        Some(_) => Err(AppError::Forbidden),
        None => Err(AppError::NotFound),
    }
}

async fn playlist(State(state): State<AppState>, user: AuthUser, Path(session): Path<String>) -> AppResult<Response> {
    check_session(&state, &user, &session).await?;
    let duration = state.transcodes.duration_ms(&session).await.ok_or(AppError::NotFound)?;
    Ok((
        [(header::CONTENT_TYPE, "application/vnd.apple.mpegurl"), (header::CACHE_CONTROL, "no-cache")],
        vod_playlist(duration, SEGMENT_SECONDS),
    )
        .into_response())
}

async fn segment(
    State(state): State<AppState>,
    user: AuthUser,
    Path((session, segment)): Path<(String, String)>,
) -> AppResult<Response> {
    check_session(&state, &user, &session).await?;
    let index = parse_segment_name(&segment).ok_or(AppError::NotFound)?;
    // Only a missing session or segment is a 404. A transcoder failure is a
    // 500, so the player reports a server problem instead of a missing file.
    let path = state.transcodes.segment(&session, index).await.map_err(|e| {
        tracing::warn!(session, index, error = %e, "segment failed");
        match e {
            SegmentError::NoSession | SegmentError::OutOfRange => AppError::NotFound,
            SegmentError::Failed(e) => AppError::Internal(e),
        }
    })?;
    let bytes = tokio::fs::read(&path).await?;
    Ok(([(header::CONTENT_TYPE, "video/mp2t")], bytes).into_response())
}

#[derive(Deserialize, Default)]
struct StopBody {
    #[serde(default)]
    position_ms: Option<u64>,
    #[serde(default)]
    item_id: Option<String>,
}

async fn stop(
    State(state): State<AppState>,
    user: AuthUser,
    Path(session): Path<String>,
    body: Option<Json<StopBody>>,
) -> AppResult<StatusCode> {
    check_session(&state, &user, &session).await?;
    state.transcodes.stop(&session).await;
    if let Some(Json(b)) = body {
        if let (Some(item_id), Some(position_ms)) = (b.item_id, b.position_ms) {
            state.emit(ServerEvent::PlaybackStopped { user_id: user.id, item_id, position_ms });
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Convert an embedded text subtitle to WebVTT for the browser.
async fn subtitle(
    State(state): State<AppState>,
    _user: AuthUser,
    Path((file_id, index)): Path<(String, String)>,
) -> AppResult<Response> {
    let index: u32 = index.trim_end_matches(".vtt").parse().map_err(|_| AppError::NotFound)?;
    let file = load_file(&state, &file_id).await?;
    if !file.info.subtitles.iter().any(|s| s.index == index && s.is_text) {
        return Err(AppError::NotFound);
    }
    let args = kinora_core::ffmpeg::subtitle_vtt_args(&file.path, index);
    let out = tokio::process::Command::new(&state.config.ffmpeg).args(&args).kill_on_drop(true).output().await?;
    if !out.status.success() {
        return Err(AppError::Internal(anyhow::anyhow!(
            "subtitle extraction failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(([(header::CONTENT_TYPE, "text/vtt; charset=utf-8")], out.stdout).into_response())
}

async fn active_sessions(
    State(state): State<AppState>,
    user: AuthUser,
) -> AppResult<Json<Vec<crate::transcode::SessionInfo>>> {
    user.require(Role::Admin)?;
    Ok(Json(state.transcodes.list().await))
}
