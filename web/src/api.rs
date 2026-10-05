// Typed wrappers around one raw `call()` primitive — each function here
// builds exactly the `RequestBody` variant it needs, and unwraps exactly
// the `ResponseBody` variant it expects. A wire-level `Error` response
// short-circuits to `Err` uniformly in `call()`, so none of the wrappers
// below need to think about it.

use lestobackupper_schema::protocol::{
    AlbumKind, ApproveUserRequest, BeginOtpEnrollmentRequest, ChangePasswordRequest, ConfirmOtpEnrollmentRequest, CreateAlbumRequest, CreateUserRequest, DisableOtpRequest,
    DownloadFileRequest, ErrorCode, HideAlbumRequest, ListAlbumFilesRequest, ListHiddenAlbumsRequest, ListLibraryRequest, ListMyAlbumsRequest, ListPendingUsersRequest,
    LoginRequest, RenameAlbumRequest, RenameFileRequest, Request, RequestBody, Response, ResponseBody, ResponseRef, ThumbnailVariant, TrashFromAlbumRequest, UnhideAlbumRequest,
    UploadFileRequest,
};
use planus::ReadAsRoot;

use crate::ws_client::WsClient;

#[derive(Clone, PartialEq)]
pub struct AlbumSummary {
    pub album_id: String,
    pub name: String,
}

#[derive(Clone, PartialEq)]
pub struct FileInfo {
    pub file_id: String,
    /// Absent for a file nobody's ever named — no filename came with the
    /// upload, and nobody's renamed it since.
    pub name: Option<String>,
}

#[derive(Clone, PartialEq)]
pub struct PendingUser {
    pub user_id: String,
    pub username: String,
}

async fn call(client: &WsClient, token: Option<&str>, body: RequestBody) -> Result<ResponseBody, String> {
    let request_id = client.next_request_id();
    let mut builder = planus::Builder::new();
    let request = Request { request_id, token: token.map(str::to_string), body };
    let bytes = builder.finish(&request, None).to_vec();

    let response_bytes = client.call(request_id, &bytes).await.map_err(|_| "request failed — connection may have dropped".to_string())?;
    let response_ref = ResponseRef::read_as_root(&response_bytes).map_err(|e| e.to_string())?;
    let response = Response::try_from(response_ref).map_err(|e| e.to_string())?;

    match response.body {
        ResponseBody::Error(e) => Err(e.message),
        body => Ok(body),
    }
}

/// Distinguishes "your username/password were fine, this account just
/// also needs a TOTP code" from every other failure — the login form
/// needs to react completely differently to it (reveal the code field
/// rather than show an error), and that distinction only exists on the
/// wire as `ErrorCode::OtpRequired`, which the generic `call()` helper's
/// `Err(String)` throws away.
pub enum LoginOutcome {
    Success { token: String, is_admin: bool },
    OtpRequired,
    Error(String),
}

pub async fn login(client: &WsClient, username: &str, password: &str, otp_code: Option<&str>) -> LoginOutcome {
    let request_id = client.next_request_id();
    let mut builder = planus::Builder::new();
    let body = RequestBody::LoginRequest(Box::new(LoginRequest {
        username: username.to_string(),
        password: password.to_string(),
        otp_code: otp_code.map(str::to_string),
        device_label: "web".to_string(),
    }));
    let request = Request { request_id, token: None, body };
    let bytes = builder.finish(&request, None).to_vec();

    let response_bytes = match client.call(request_id, &bytes).await {
        Ok(b) => b,
        Err(_) => return LoginOutcome::Error("request failed — connection may have dropped".to_string()),
    };
    let response = match ResponseRef::read_as_root(&response_bytes).map_err(|e| e.to_string()).and_then(|r| Response::try_from(r).map_err(|e| e.to_string())) {
        Ok(r) => r,
        Err(message) => return LoginOutcome::Error(message),
    };

    match response.body {
        ResponseBody::LoginResponse(r) => LoginOutcome::Success { token: r.token, is_admin: r.is_admin },
        ResponseBody::Error(e) if e.code == ErrorCode::OtpRequired => LoginOutcome::OtpRequired,
        ResponseBody::Error(e) => LoginOutcome::Error(e.message),
        other => LoginOutcome::Error(unexpected(&other)),
    }
}

pub async fn create_user(client: &WsClient, username: &str, password: &str) -> Result<String, String> {
    let body = RequestBody::CreateUserRequest(Box::new(CreateUserRequest { username: username.to_string(), password: password.to_string() }));
    match call(client, None, body).await? {
        ResponseBody::CreateUserResponse(r) => Ok(r.user_id),
        other => Err(unexpected(&other)),
    }
}

pub async fn list_library(client: &WsClient, token: &str) -> Result<Vec<FileInfo>, String> {
    match call(client, Some(token), RequestBody::ListLibraryRequest(Box::new(ListLibraryRequest {}))).await? {
        ResponseBody::ListFilesResponse(r) => Ok(r.files.into_iter().map(|f| FileInfo { file_id: f.file_id, name: f.name }).collect()),
        other => Err(unexpected(&other)),
    }
}

pub async fn list_album_files(client: &WsClient, token: &str, album_id: &str) -> Result<Vec<FileInfo>, String> {
    let body = RequestBody::ListAlbumFilesRequest(Box::new(ListAlbumFilesRequest { album_id: album_id.to_string() }));
    match call(client, Some(token), body).await? {
        ResponseBody::ListFilesResponse(r) => Ok(r.files.into_iter().map(|f| FileInfo { file_id: f.file_id, name: f.name }).collect()),
        other => Err(unexpected(&other)),
    }
}

pub async fn list_my_albums(client: &WsClient, token: &str) -> Result<Vec<AlbumSummary>, String> {
    match call(client, Some(token), RequestBody::ListMyAlbumsRequest(Box::new(ListMyAlbumsRequest {}))).await? {
        ResponseBody::ListMyAlbumsResponse(r) => Ok(r.albums.into_iter().map(|a| AlbumSummary { album_id: a.album_id, name: a.name }).collect()),
        other => Err(unexpected(&other)),
    }
}

pub async fn create_album(client: &WsClient, token: &str, name: &str) -> Result<String, String> {
    let body = RequestBody::CreateAlbumRequest(Box::new(CreateAlbumRequest { name: name.to_string(), kind: AlbumKind::User }));
    match call(client, Some(token), body).await? {
        ResponseBody::CreateAlbumResponse(r) => Ok(r.album_id),
        other => Err(unexpected(&other)),
    }
}

pub async fn rename_album(client: &WsClient, token: &str, album_id: &str, name: &str) -> Result<(), String> {
    let body = RequestBody::RenameAlbumRequest(Box::new(RenameAlbumRequest { album_id: album_id.to_string(), name: name.to_string() }));
    call(client, Some(token), body).await.map(|_| ())
}

pub async fn hide_album(client: &WsClient, token: &str, album_id: &str) -> Result<(), String> {
    let body = RequestBody::HideAlbumRequest(Box::new(HideAlbumRequest { album_id: album_id.to_string() }));
    call(client, Some(token), body).await.map(|_| ())
}

pub async fn unhide_album(client: &WsClient, token: &str, album_id: &str) -> Result<(), String> {
    let body = RequestBody::UnhideAlbumRequest(Box::new(UnhideAlbumRequest { album_id: album_id.to_string() }));
    call(client, Some(token), body).await.map(|_| ())
}

pub async fn list_hidden_albums(client: &WsClient, token: &str) -> Result<Vec<AlbumSummary>, String> {
    match call(client, Some(token), RequestBody::ListHiddenAlbumsRequest(Box::new(ListHiddenAlbumsRequest {}))).await? {
        ResponseBody::ListMyAlbumsResponse(r) => Ok(r.albums.into_iter().map(|a| AlbumSummary { album_id: a.album_id, name: a.name }).collect()),
        other => Err(unexpected(&other)),
    }
}

pub async fn add_to_album(client: &WsClient, token: &str, album_id: &str, file_id: &str) -> Result<(), String> {
    let body = RequestBody::AddToAlbumRequest(Box::new(lestobackupper_schema::protocol::AddToAlbumRequest { album_id: album_id.to_string(), file_id: file_id.to_string() }));
    call(client, Some(token), body).await.map(|_| ())
}

pub async fn trash_from_album(client: &WsClient, token: &str, album_id: &str, file_id: &str) -> Result<(), String> {
    let body = RequestBody::TrashFromAlbumRequest(Box::new(TrashFromAlbumRequest { album_id: album_id.to_string(), file_id: file_id.to_string() }));
    call(client, Some(token), body).await.map(|_| ())
}

pub async fn upload_file(client: &WsClient, token: &str, data: Vec<u8>, mime: &str, name: Option<&str>) -> Result<String, String> {
    let body = RequestBody::UploadFileRequest(Box::new(UploadFileRequest {
        data,
        mime: mime.to_string(),
        width: None,
        height: None,
        duration: None,
        previous_version: None,
        name: name.map(str::to_string),
        // A browser File only has lastModified, which isn't when it was
        // taken; without it the library sorts by upload time.
        taken_at: None,
    }));
    match call(client, Some(token), body).await? {
        ResponseBody::UploadFileResponse(r) => Ok(r.file_id),
        other => Err(unexpected(&other)),
    }
}

pub async fn rename_file(client: &WsClient, token: &str, file_id: &str, name: &str) -> Result<(), String> {
    let body = RequestBody::RenameFileRequest(Box::new(RenameFileRequest { file_id: file_id.to_string(), name: name.to_string() }));
    call(client, Some(token), body).await.map(|_| ())
}

pub async fn download_file(client: &WsClient, token: &str, file_id: &str, variant: ThumbnailVariant) -> Result<(Vec<u8>, String, Option<String>), String> {
    let body = RequestBody::DownloadFileRequest(Box::new(DownloadFileRequest { file_id: file_id.to_string(), variant }));
    match call(client, Some(token), body).await? {
        ResponseBody::DownloadFileResponse(r) => Ok((r.data, r.mime, r.name)),
        other => Err(unexpected(&other)),
    }
}

pub async fn list_pending_users(client: &WsClient, token: &str) -> Result<Vec<PendingUser>, String> {
    match call(client, Some(token), RequestBody::ListPendingUsersRequest(Box::new(ListPendingUsersRequest {}))).await? {
        ResponseBody::ListPendingUsersResponse(r) => Ok(r.users.into_iter().map(|u| PendingUser { user_id: u.user_id, username: u.username }).collect()),
        other => Err(unexpected(&other)),
    }
}

pub async fn approve_user(client: &WsClient, token: &str, user_id: &str) -> Result<(), String> {
    let body = RequestBody::ApproveUserRequest(Box::new(ApproveUserRequest { user_id: user_id.to_string() }));
    call(client, Some(token), body).await.map(|_| ())
}

pub async fn change_password(client: &WsClient, token: &str, old_password: &str, new_password: &str) -> Result<(), String> {
    let body = RequestBody::ChangePasswordRequest(Box::new(ChangePasswordRequest { old_password: old_password.to_string(), new_password: new_password.to_string() }));
    call(client, Some(token), body).await.map(|_| ())
}

pub async fn begin_otp_enrollment(client: &WsClient, token: &str) -> Result<(String, String), String> {
    match call(client, Some(token), RequestBody::BeginOtpEnrollmentRequest(Box::new(BeginOtpEnrollmentRequest {}))).await? {
        ResponseBody::OtpEnrollmentResponse(r) => Ok((r.secret_base32, r.otpauth_url)),
        other => Err(unexpected(&other)),
    }
}

pub async fn confirm_otp_enrollment(client: &WsClient, token: &str, secret_base32: &str, code: &str) -> Result<(), String> {
    let body = RequestBody::ConfirmOtpEnrollmentRequest(Box::new(ConfirmOtpEnrollmentRequest { secret_base32: secret_base32.to_string(), code: code.to_string() }));
    call(client, Some(token), body).await.map(|_| ())
}

pub async fn disable_otp(client: &WsClient, token: &str) -> Result<(), String> {
    call(client, Some(token), RequestBody::DisableOtpRequest(Box::new(DisableOtpRequest {}))).await.map(|_| ())
}

fn unexpected(body: &ResponseBody) -> String {
    format!("unexpected response shape: {body:?}")
}
