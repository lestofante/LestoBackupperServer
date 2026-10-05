// The transport-agnostic core: turn one parsed `RequestBody` into one
// `ResponseBody`, or an `ApiError`. Mirrors the shape of the earlier JSON
// `server::api::dispatch` (one enum, one exhaustive match, auth resolved
// once before the match) — same reasoning, different wire format. Neither
// `src/tcp.rs` nor `src/ws.rs` know anything about `server`'s domain
// types; this module is the only place that does.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use lestobackupper_schema::protocol::{
    self as wire, Ack, AlbumMember as WireAlbumMember, AlbumMembersResponse, AlbumSummary as WireAlbumSummary, BeginUploadResponse, DownloadChunkResponse, LibraryFile as WireLibraryFile, LibraryPageResponse, CreateAlbumResponse, CreateUserResponse, DownloadFileResponse, FileSummary as WireFileSummary, ListFilesResponse,
    ListMyAlbumsResponse, ListPendingUsersResponse, FindLibraryFilesByHashResponse, HashMatch as WireHashMatch, ListSessionsResponse, ListTrashResponse, LoginResponse, OtpEnrollmentResponse, PendingUser as WirePendingUser, RequestBody,
    ResponseBody, RoleOfResponse, SessionSummary as WireSessionSummary, TrashedFile as WireTrashedFile, UploadFileResponse,
};
use server::commands::{album, file, session, user};
use server::{AlbumId, AlbumKind, AlbumRole, EventLog, FileId, ObjectId, Server, UserId};

use crate::error::ApiError;
use crate::human::Bytes;

/// Everything a request handler needs: the event log and where blobs live
/// on disk. Bundled together (rather than two separate `Arc`s) because
/// every blocking closure needs both — `upload_file`/`download_file` take
/// `objects_dir` directly, and even the ones that don't still go through
/// the same `blocking` helper.
pub struct AppState {
    pub log: EventLog<Server>,
    pub objects_dir: PathBuf,
}

pub type SharedState = Arc<AppState>;

type Result<T> = std::result::Result<T, ApiError>;

/// `raw_token` is whatever the transport pulled off the wire (a WebSocket
/// header, a raw-TCP handshake field — transport-specific, resolved before
/// this is called) — `None` for a client that hasn't logged in yet, which
/// is only ever valid for `CreateUserRequest`/`LoginRequest`.
pub async fn dispatch(state: &SharedState, raw_token: Option<&str>, body: RequestBody) -> Result<ResponseBody> {
    let op = op_name(&body);
    let started = Instant::now();
    let result = handle(state, raw_token, body).await;
    match &result {
        Ok(_) => tracing::debug!(op, took = ?started.elapsed(), "ok"),
        Err(err) => {
            let (code, message) = err.to_wire();
            tracing::warn!(op, ?code, %message, "request failed");
        }
    }
    result
}

async fn handle(state: &SharedState, raw_token: Option<&str>, body: RequestBody) -> Result<ResponseBody> {
    // Handled before any token is required; everything else falls through
    // to the exhaustive match below, which resolves `actor` first.
    match &body {
        RequestBody::CreateUserRequest(req) => {
            let (username, password) = (req.username.clone(), req.password.clone());
            let user_id = blocking(state, move |state| user::create_user(&state.log, &username, &password)).await?.actor;
            return Ok(ResponseBody::CreateUserResponse(Box::new(CreateUserResponse { user_id: user_id.to_string() })));
        }
        RequestBody::LoginRequest(req) => {
            let (username, password, otp_code, device_label) = (req.username.clone(), req.password.clone(), req.otp_code.clone(), req.device_label.clone());
            let (user, device) = (username.clone(), device_label.clone());
            let result = blocking(state, move |state| session::login(&state.log, &username, &password, otp_code.as_deref(), &device_label)).await?;
            tracing::info!(%user, %device, "logged in");
            return Ok(ResponseBody::LoginResponse(Box::new(LoginResponse { token: result.token, is_admin: result.is_admin })));
        }
        _ => {}
    }

    let raw_token = raw_token.ok_or(ApiError::Server(server::ServerError::InvalidToken))?.to_string();
    let actor = blocking(state, move |state| session::authenticate(&state.log, &raw_token)).await?;

    match body {
        RequestBody::CreateUserRequest(_) | RequestBody::LoginRequest(_) => unreachable!("handled above, before a token was required"),

        RequestBody::UploadFileRequest(req) => {
            let previous_version = req.previous_version.as_deref().map(parse_file_id).transpose()?;
            let (data, mime, name) = (req.data, req.mime, req.name);
            let meta = file::FileMetadata { width: req.width, height: req.height, duration: req.duration, previous_version, taken_at: req.taken_at };
            let started = Instant::now();
            let ev = blocking(state, move |state| file::upload_file(&state.log, &state.objects_dir, actor, &data, mime, name, meta)).await?;
            let stored_in = started.elapsed();
            let server::Action::UploadFile { file_id, size, mime, name, .. } = ev.action else { unreachable!() };
            let user = username(state, actor).await;
            tracing::info!(
                %user,
                name = name.as_deref().unwrap_or("-"),
                size = %Bytes(size as u64),
                %mime,
                %file_id,
                ?stored_in,
                "uploaded"
            );
            Ok(ResponseBody::UploadFileResponse(Box::new(UploadFileResponse { file_id: file_id.to_string() })))
        }
        RequestBody::DownloadFileRequest(req) => {
            let file_id = parse_file_id(&req.file_id)?;
            let variant = to_domain_variant(req.variant);
            let (data, mime, name) = blocking(state, move |state| file::download_file(&state.log, &state.objects_dir, actor, file_id, variant)).await?;
            Ok(ResponseBody::DownloadFileResponse(Box::new(DownloadFileResponse { data, mime, name })))
        }
        RequestBody::RenameFileRequest(req) => {
            let (file_id, name) = (parse_file_id(&req.file_id)?, req.name.clone());
            blocking(state, move |state| file::rename_file(&state.log, actor, file_id, &name)).await?;
            Ok(ack())
        }

        RequestBody::CreateAlbumRequest(req) => {
            let (name, kind) = (req.name.clone(), to_domain_kind(req.kind));
            let ev = blocking(state, move |state| album::create_album(&state.log, actor, &name, kind)).await?;
            let album_id = match ev.action {
                server::Action::CreateAlbum { album_id, .. } => album_id,
                _ => unreachable!(),
            };
            Ok(ResponseBody::CreateAlbumResponse(Box::new(CreateAlbumResponse { album_id: album_id.to_string() })))
        }

        RequestBody::RenameAlbumRequest(req) => {
            let (album_id, name) = (parse_album_id(&req.album_id)?, req.name.clone());
            blocking(state, move |state| album::rename_album(&state.log, actor, album_id, &name)).await?;
            Ok(ack())
        }
        RequestBody::HideAlbumRequest(req) => {
            let album_id = parse_album_id(&req.album_id)?;
            blocking(state, move |state| album::hide_album(&state.log, actor, album_id)).await?;
            Ok(ack())
        }
        RequestBody::UnhideAlbumRequest(req) => {
            let album_id = parse_album_id(&req.album_id)?;
            blocking(state, move |state| album::unhide_album(&state.log, actor, album_id)).await?;
            Ok(ack())
        }
        RequestBody::ListHiddenAlbumsRequest(_) => {
            let albums = blocking(state, move |state| server::queries::list_hidden_albums(&state.log, actor)).await?;
            let albums = albums
                .into_iter()
                .map(wire_album_summary)
                .collect();
            Ok(ResponseBody::ListMyAlbumsResponse(Box::new(ListMyAlbumsResponse { albums })))
        }

        RequestBody::AddToAlbumRequest(req) => {
            let (album_id, file_id) = (parse_album_id(&req.album_id)?, parse_file_id(&req.file_id)?);
            blocking(state, move |state| album::add_to_album(&state.log, actor, album_id, file_id)).await?;
            Ok(ack())
        }
        RequestBody::TrashFromAlbumRequest(req) => {
            let (album_id, file_id) = (parse_album_id(&req.album_id)?, parse_file_id(&req.file_id)?);
            blocking(state, move |state| album::trash_from_album(&state.log, actor, album_id, file_id)).await?;
            Ok(ack())
        }
        RequestBody::RestoreFromTrashRequest(req) => {
            let (album_id, file_id) = (parse_album_id(&req.album_id)?, parse_file_id(&req.file_id)?);
            blocking(state, move |state| album::restore_from_trash(&state.log, actor, album_id, file_id)).await?;
            Ok(ack())
        }
        RequestBody::PurgeFromAlbumRequest(req) => {
            let (album_id, file_id) = (parse_album_id(&req.album_id)?, parse_file_id(&req.file_id)?);
            blocking(state, move |state| album::purge_from_album(&state.log, actor, album_id, file_id)).await?;
            Ok(ack())
        }
        RequestBody::ShareAlbumRequest(req) => {
            let (album_id, user_id, role) = (parse_album_id(&req.album_id)?, parse_user_id(&req.user_id)?, to_domain_role(req.role));
            blocking(state, move |state| album::share_album(&state.log, actor, album_id, user_id, role)).await?;
            Ok(ack())
        }
        RequestBody::ShareAlbumWithRequest(req) => {
            let (album_id, username, role) = (parse_album_id(&req.album_id)?, req.username, to_domain_role(req.role));
            blocking(state, move |state| album::share_album_with(&state.log, actor, album_id, &username, role)).await?;
            Ok(ack())
        }
        RequestBody::ListAlbumMembersRequest(req) => {
            let album_id = parse_album_id(&req.album_id)?;
            let members = blocking(state, move |state| server::queries::list_album_members(&state.log, actor, album_id)).await?;
            Ok(ResponseBody::AlbumMembersResponse(Box::new(AlbumMembersResponse {
                members: members
                    .into_iter()
                    .map(|m| WireAlbumMember { user_id: m.user_id.to_string(), username: m.username, role: from_domain_role(m.role) })
                    .collect(),
            })))
        }
        RequestBody::UnshareAlbumRequest(req) => {
            let (album_id, user_id) = (parse_album_id(&req.album_id)?, parse_user_id(&req.user_id)?);
            blocking(state, move |state| album::unshare_album(&state.log, actor, album_id, user_id)).await?;
            Ok(ack())
        }
        RequestBody::RoleOfRequest(req) => {
            let (album_id, user_id) = (parse_album_id(&req.album_id)?, parse_user_id(&req.user_id)?);
            let role = blocking(state, move |state| album::role_of(&state.log, album_id, user_id)).await?;
            Ok(ResponseBody::RoleOfResponse(Box::new(RoleOfResponse { role: role.map(from_domain_role) })))
        }

        RequestBody::ListMyAlbumsRequest(_) => {
            let albums = blocking(state, move |state| server::queries::list_my_albums(&state.log, actor)).await?;
            let albums = albums
                .into_iter()
                .map(wire_album_summary)
                .collect();
            Ok(ResponseBody::ListMyAlbumsResponse(Box::new(ListMyAlbumsResponse { albums })))
        }
        RequestBody::ListAlbumFilesRequest(req) => {
            let album_id = parse_album_id(&req.album_id)?;
            let files = blocking(state, move |state| server::queries::list_album_files(&state.log, actor, album_id)).await?;
            Ok(ResponseBody::ListFilesResponse(Box::new(ListFilesResponse { files: files.into_iter().map(from_domain_file_summary).collect() })))
        }
        RequestBody::ListLibraryRequest(_) => {
            let files = blocking(state, move |state| server::queries::list_library(&state.log, actor)).await?;
            Ok(ResponseBody::ListFilesResponse(Box::new(ListFilesResponse { files: files.into_iter().map(from_domain_file_summary).collect() })))
        }
        RequestBody::FindLibraryFilesByHashRequest(req) => {
            let hashes = req.hashes.iter().map(|h| parse_hash(h)).collect::<Result<Vec<_>>>()?;
            let lookup = blocking(state, move |state| server::queries::find_library_files_by_hash(&state.log, &state.objects_dir, actor, &hashes)).await?;
            let user = username(state, actor).await;
            tracing::info!(%user, asked = req.hashes.len(), in_library = lookup.matches.len(), "hash lookup");
            if !lookup.missing_blobs.is_empty() {
                tracing::warn!(
                    count = lookup.missing_blobs.len(),
                    first = %lookup.missing_blobs[0],
                    "library files whose blob is missing from the objects dir; reporting them absent so clients re-upload"
                );
            }
            let matches = lookup.matches.into_iter().map(|m| WireHashMatch { hash: m.hash.to_string(), file_id: m.file_id.to_string() }).collect();
            Ok(ResponseBody::FindLibraryFilesByHashResponse(Box::new(FindLibraryFilesByHashResponse { matches })))
        }
        RequestBody::BeginUploadRequest(req) => {
            let (hash, size) = (parse_hash(&req.hash)?, req.size);
            let log_hash = hash.clone();
            let offset = blocking(state, move |state| file::begin_upload(&state.objects_dir, actor, &hash, size)).await?;
            let user = username(state, actor).await;
            tracing::info!(%user, hash = %log_hash, size = %Bytes(size), resume_from = %Bytes(offset), "chunked upload started");
            Ok(ResponseBody::BeginUploadResponse(Box::new(BeginUploadResponse { offset })))
        }
        RequestBody::UploadChunkRequest(req) => {
            let (hash, offset, data) = (parse_hash(&req.hash)?, req.offset, req.data);
            let len = data.len();
            blocking(state, move |state| file::upload_chunk(&state.objects_dir, actor, &hash, offset, &data)).await?;
            tracing::debug!(offset, len, "chunk stored");
            Ok(ack())
        }
        RequestBody::CommitUploadRequest(req) => {
            let hash = parse_hash(&req.hash)?;
            let previous_version = req.previous_version.as_deref().map(parse_file_id).transpose()?;
            let (mime, name) = (req.mime, req.name);
            let meta = file::FileMetadata { width: req.width, height: req.height, duration: req.duration, previous_version, taken_at: req.taken_at };
            let started = Instant::now();
            let ev = blocking(state, move |state| file::commit_upload(&state.log, &state.objects_dir, actor, &hash, mime, name, meta)).await?;
            let stored_in = started.elapsed();
            let server::Action::UploadFile { file_id, size, mime, name, .. } = ev.action else { unreachable!() };
            let user = username(state, actor).await;
            tracing::info!(
                %user,
                name = name.as_deref().unwrap_or("-"),
                size = %Bytes(size as u64),
                %mime,
                %file_id,
                ?stored_in,
                "uploaded (chunked)"
            );
            Ok(ResponseBody::UploadFileResponse(Box::new(UploadFileResponse { file_id: file_id.to_string() })))
        }
        RequestBody::ListLibraryPageRequest(req) => {
            let (after, limit) = (req.after.clone(), req.limit);
            let page = blocking(state, move |state| server::queries::list_library_page(&state.log, actor, after.as_deref(), limit)).await?;
            Ok(wire_library_page(page))
        }
        RequestBody::ListAlbumPageRequest(req) => {
            let (album_id, after, limit) = (parse_album_id(&req.album_id)?, req.after.clone(), req.limit);
            let page = blocking(state, move |state| server::queries::list_album_page(&state.log, actor, album_id, after.as_deref(), limit)).await?;
            Ok(wire_library_page(page))
        }
        RequestBody::DeleteAlbumRequest(req) => {
            let album_id = parse_album_id(&req.album_id)?;
            blocking(state, move |state| album::delete_album(&state.log, actor, album_id)).await?;
            let user = username(state, actor).await;
            tracing::info!(%user, %album_id, "album deleted");
            Ok(ack())
        }
        RequestBody::RemoveFromLibraryRequest(req) => {
            let file_id = parse_file_id(&req.file_id)?;
            blocking(state, move |state| file::remove_from_library(&state.log, actor, file_id)).await?;
            let user = username(state, actor).await;
            tracing::info!(%user, %file_id, "removed from library");
            Ok(ack())
        }
        RequestBody::DownloadChunkRequest(req) => {
            let (file_id, offset, length) = (parse_file_id(&req.file_id)?, req.offset, req.length);
            let range = blocking(state, move |state| file::download_range(&state.log, &state.objects_dir, actor, file_id, offset, length)).await?;
            Ok(ResponseBody::DownloadChunkResponse(Box::new(DownloadChunkResponse { data: range.data, total_size: range.total_size, mime: range.mime, name: range.name })))
        }
        RequestBody::ListTrashRequest(req) => {
            let album_id = parse_album_id(&req.album_id)?;
            let files = blocking(state, move |state| server::queries::list_trash(&state.log, actor, album_id)).await?;
            let files = files.into_iter().map(|f| WireTrashedFile { file_id: f.file_id.to_string(), trashed_at: f.trashed_at, trashed_by: f.trashed_by.to_string() }).collect();
            Ok(ResponseBody::ListTrashResponse(Box::new(ListTrashResponse { files })))
        }

        RequestBody::BeginOtpEnrollmentRequest(_) => {
            let enrollment = blocking(state, move |state| user::begin_otp_enrollment(&state.log, actor)).await?;
            Ok(ResponseBody::OtpEnrollmentResponse(Box::new(OtpEnrollmentResponse { secret_base32: enrollment.secret_base32, otpauth_url: enrollment.otpauth_url })))
        }
        RequestBody::ConfirmOtpEnrollmentRequest(req) => {
            let (secret, code) = (req.secret_base32.clone(), req.code.clone());
            blocking(state, move |state| user::confirm_otp_enrollment(&state.log, actor, &secret, &code)).await?;
            Ok(ack())
        }
        RequestBody::DisableOtpRequest(_) => {
            blocking(state, move |state| user::disable_otp(&state.log, actor)).await?;
            Ok(ack())
        }

        RequestBody::ChangePasswordRequest(req) => {
            let (old_password, new_password) = (req.old_password.clone(), req.new_password.clone());
            blocking(state, move |state| user::change_password(&state.log, actor, &old_password, &new_password)).await?;
            Ok(ack())
        }

        RequestBody::RevokeSessionRequest(req) => {
            let session_token = req.session_token.clone();
            blocking(state, move |state| session::revoke(&state.log, actor, &session_token)).await?;
            Ok(ack())
        }
        RequestBody::ListSessionsRequest(_) => {
            let sessions = blocking(state, move |state| session::list_sessions(&state.log, actor)).await?;
            let sessions = sessions.into_iter().map(|s| WireSessionSummary { device_label: s.device_label, issued_at: s.issued_at }).collect();
            Ok(ResponseBody::ListSessionsResponse(Box::new(ListSessionsResponse { sessions })))
        }

        RequestBody::ApproveUserRequest(req) => {
            let target = parse_user_id(&req.user_id)?;
            blocking(state, move |state| user::approve_user(&state.log, actor, target)).await?;
            Ok(ack())
        }
        RequestBody::ListPendingUsersRequest(_) => {
            let pending = blocking(state, move |state| server::queries::list_pending_users(&state.log, actor)).await?;
            let users = pending.into_iter().map(|p| WirePendingUser { user_id: p.user_id.to_string(), username: p.username, created_at: p.created_at }).collect();
            Ok(ResponseBody::ListPendingUsersResponse(Box::new(ListPendingUsersResponse { users })))
        }
    }
}

/// For log lines only; falls back to the id if the lookup fails.
async fn username(state: &SharedState, user_id: UserId) -> String {
    blocking(state, move |state| user::username_of(&state.log, user_id)).await.unwrap_or_else(|_| user_id.to_string())
}

fn op_name(body: &RequestBody) -> &'static str {
    match body {
        RequestBody::CreateUserRequest(_) => "CreateUser",
        RequestBody::LoginRequest(_) => "Login",
        RequestBody::ApproveUserRequest(_) => "ApproveUser",
        RequestBody::ChangePasswordRequest(_) => "ChangePassword",
        RequestBody::ListPendingUsersRequest(_) => "ListPendingUsers",
        RequestBody::UploadFileRequest(_) => "UploadFile",
        RequestBody::DownloadFileRequest(_) => "DownloadFile",
        RequestBody::RenameFileRequest(_) => "RenameFile",
        RequestBody::CreateAlbumRequest(_) => "CreateAlbum",
        RequestBody::RenameAlbumRequest(_) => "RenameAlbum",
        RequestBody::HideAlbumRequest(_) => "HideAlbum",
        RequestBody::UnhideAlbumRequest(_) => "UnhideAlbum",
        RequestBody::ListHiddenAlbumsRequest(_) => "ListHiddenAlbums",
        RequestBody::AddToAlbumRequest(_) => "AddToAlbum",
        RequestBody::TrashFromAlbumRequest(_) => "TrashFromAlbum",
        RequestBody::RestoreFromTrashRequest(_) => "RestoreFromTrash",
        RequestBody::PurgeFromAlbumRequest(_) => "PurgeFromAlbum",
        RequestBody::ShareAlbumRequest(_) => "ShareAlbum",
        RequestBody::UnshareAlbumRequest(_) => "UnshareAlbum",
        RequestBody::ShareAlbumWithRequest(_) => "ShareAlbumWith",
        RequestBody::ListAlbumMembersRequest(_) => "ListAlbumMembers",
        RequestBody::RoleOfRequest(_) => "RoleOf",
        RequestBody::ListMyAlbumsRequest(_) => "ListMyAlbums",
        RequestBody::ListAlbumFilesRequest(_) => "ListAlbumFiles",
        RequestBody::ListLibraryRequest(_) => "ListLibrary",
        RequestBody::ListTrashRequest(_) => "ListTrash",
        RequestBody::BeginOtpEnrollmentRequest(_) => "BeginOtpEnrollment",
        RequestBody::ConfirmOtpEnrollmentRequest(_) => "ConfirmOtpEnrollment",
        RequestBody::DisableOtpRequest(_) => "DisableOtp",
        RequestBody::RevokeSessionRequest(_) => "RevokeSession",
        RequestBody::ListSessionsRequest(_) => "ListSessions",
        RequestBody::FindLibraryFilesByHashRequest(_) => "FindLibraryFilesByHash",
        RequestBody::BeginUploadRequest(_) => "BeginUpload",
        RequestBody::UploadChunkRequest(_) => "UploadChunk",
        RequestBody::CommitUploadRequest(_) => "CommitUpload",
        RequestBody::ListLibraryPageRequest(_) => "ListLibraryPage",
        RequestBody::DownloadChunkRequest(_) => "DownloadChunk",
        RequestBody::ListAlbumPageRequest(_) => "ListAlbumPage",
        RequestBody::DeleteAlbumRequest(_) => "DeleteAlbum",
        RequestBody::RemoveFromLibraryRequest(_) => "RemoveFromLibrary",
    }
}

fn wire_album_summary(a: server::queries::AlbumSummary) -> WireAlbumSummary {
    WireAlbumSummary {
        album_id: a.album_id.to_string(),
        name: a.name,
        kind: from_domain_kind(a.kind),
        role: from_domain_role(a.role),
        file_count: u32::try_from(a.file_count).unwrap_or(u32::MAX),
        cover_file_id: a.cover_file_id.map(|id| id.to_string()),
    }
}

fn wire_library_page(page: server::queries::LibraryPage) -> ResponseBody {
    let files = page
        .files
        .into_iter()
        .map(|f| WireLibraryFile {
            file_id: f.file_id,
            hash: f.hash,
            name: f.name,
            mime: f.mime,
            size: f.size,
            width: f.width,
            height: f.height,
            duration: f.duration,
            taken_at: f.taken_at,
            added_at: f.added_at,
        })
        .collect();
    ResponseBody::LibraryPageResponse(Box::new(LibraryPageResponse { files, next: page.next }))
}

fn ack() -> ResponseBody {
    ResponseBody::Ack(Box::new(Ack {}))
}

fn parse_user_id(s: &str) -> Result<UserId> {
    s.parse().map_err(|e| ApiError::invalid("user_id", e))
}
fn parse_album_id(s: &str) -> Result<AlbumId> {
    s.parse().map_err(|e| ApiError::invalid("album_id", e))
}
fn parse_file_id(s: &str) -> Result<FileId> {
    s.parse().map_err(|e| ApiError::invalid("file_id", e))
}
fn parse_hash(s: &str) -> Result<ObjectId> {
    s.parse().map_err(|e| ApiError::invalid("hashes", e))
}

fn to_domain_kind(k: wire::AlbumKind) -> AlbumKind {
    match k {
        wire::AlbumKind::User => AlbumKind::User,
        wire::AlbumKind::System => AlbumKind::System,
    }
}
fn from_domain_kind(k: AlbumKind) -> wire::AlbumKind {
    match k {
        AlbumKind::User => wire::AlbumKind::User,
        AlbumKind::System => wire::AlbumKind::System,
    }
}
fn to_domain_role(r: wire::AlbumRole) -> AlbumRole {
    match r {
        wire::AlbumRole::Reader => AlbumRole::Reader,
        wire::AlbumRole::Writer => AlbumRole::Writer,
        wire::AlbumRole::Admin => AlbumRole::Admin,
        wire::AlbumRole::Owner => AlbumRole::Owner,
    }
}
fn to_domain_variant(v: wire::ThumbnailVariant) -> file::FileVariant {
    match v {
        wire::ThumbnailVariant::Original => file::FileVariant::Original,
        wire::ThumbnailVariant::Small => file::FileVariant::Small,
        wire::ThumbnailVariant::Medium => file::FileVariant::Medium,
    }
}
fn from_domain_file_summary(f: server::queries::FileSummary) -> WireFileSummary {
    WireFileSummary { file_id: f.file_id.to_string(), name: f.name }
}
fn from_domain_role(r: AlbumRole) -> wire::AlbumRole {
    match r {
        AlbumRole::Reader => wire::AlbumRole::Reader,
        AlbumRole::Writer => wire::AlbumRole::Writer,
        AlbumRole::Admin => wire::AlbumRole::Admin,
        AlbumRole::Owner => wire::AlbumRole::Owner,
    }
}

/// Runs one blocking `server` call (diesel/r2d2, Argon2, ...) on tokio's
/// blocking thread pool. `server` is deliberately synchronous throughout
/// (see its own README) — this is the one seam where that meets an async
/// server, so every request gets its own thread instead of blocking a
/// worker that other requests need.
async fn blocking<T, F>(state: &SharedState, f: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce(&AppState) -> server::Result<T> + Send + 'static,
{
    let state = Arc::clone(state);
    Ok(tokio::task::spawn_blocking(move || f(&state)).await.expect("blocking task panicked")?)
}
