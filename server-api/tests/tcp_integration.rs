// Drives the real TCP transport end to end: a genuine socket, real framing,
// real flatbuffers encode/decode on both ends — the only thing skipped is
// spawning the built binary as a separate process, which would test the
// same code path with more moving parts for no more confidence.
//
// commands::session's login rate limiter (server/src/commands/session.rs)
// is a single process-global static, so every test here that logs in must
// be serialized against the others in this file — see server/tests/
// session.rs's header comment for the same pattern in the domain crate's
// own tests.
//
// New accounts start Pending (server::commands::user::create_user), so
// every test that needs a *working* login goes through the bootstrap admin
// (server::commands::user::ensure_bootstrap_admin, called in spawn_server)
// to approve one first — over the wire, via ApproveUserRequest, the same
// way a real admin client would.

use std::sync::Arc;
use std::time::Duration;

use lestobackupper_schema::protocol::{
    AlbumKind, AlbumRole, ApproveUserRequest, BeginUploadRequest, CommitUploadRequest, CreateAlbumRequest, CreateUserRequest, ListAlbumMembersRequest, ShareAlbumWithRequest, DownloadChunkRequest, DownloadFileRequest, ErrorCode, FindLibraryFilesByHashRequest, ListLibraryPageRequest, ListAlbumFilesRequest, ListLibraryRequest, LoginRequest, Request, RequestBody, Response, ResponseBody,
    ResponseRef, UploadChunkRequest, UploadFileRequest,
};
use planus::ReadAsRoot;
use server::{EventLog, NodeId, Server};
use server_api::tcp;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

static TEST_LOCK: Mutex<()> = Mutex::const_new(());

async fn wait_out_rate_limit() {
    tokio::time::sleep(Duration::from_millis(1100)).await;
}

async fn spawn_server() -> (tempfile::TempDir, std::net::SocketAddr) {
    let dir = tempfile::tempdir().unwrap();
    let log = EventLog::<Server>::open(dir.path().join("test.db"), NodeId("n".to_string())).unwrap();
    server::commands::user::ensure_bootstrap_admin(&log).unwrap();
    let state = Arc::new(server_api::AppState { log, objects_dir: dir.path().join("objects") });

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { continue };
            tokio::spawn(tcp::handle_connection(stream, state.clone()));
        }
    });

    (dir, addr)
}

async fn send(stream: &mut TcpStream, request_id: u64, token: Option<&str>, body: RequestBody) -> Response {
    let mut builder = planus::Builder::new();
    let req = Request { request_id, token: token.map(str::to_string), body };
    let bytes = builder.finish(&req, None);

    stream.write_u32(bytes.len() as u32).await.unwrap();
    stream.write_all(bytes).await.unwrap();
    stream.flush().await.unwrap();

    let len = stream.read_u32().await.unwrap();
    let mut buf = vec![0u8; len as usize];
    stream.read_exact(&mut buf).await.unwrap();

    Response::try_from(ResponseRef::read_as_root(&buf).unwrap()).unwrap()
}

/// Writes a request without waiting for its answer (see `receive`).
async fn send_only(stream: &mut TcpStream, request_id: u64, token: Option<&str>, body: RequestBody) {
    let mut builder = planus::Builder::new();
    let req = Request { request_id, token: token.map(str::to_string), body };
    let bytes = builder.finish(&req, None);
    stream.write_u32(bytes.len() as u32).await.unwrap();
    stream.write_all(bytes).await.unwrap();
    stream.flush().await.unwrap();
}

async fn receive(stream: &mut TcpStream) -> Response {
    let len = stream.read_u32().await.unwrap();
    let mut buf = vec![0u8; len as usize];
    stream.read_exact(&mut buf).await.unwrap();
    Response::try_from(ResponseRef::read_as_root(&buf).unwrap()).unwrap()
}

async fn login(stream: &mut TcpStream, username: &str, password: &str) -> Response {
    send(stream, 0, None, RequestBody::LoginRequest(Box::new(LoginRequest { username: username.to_string(), password: password.to_string(), otp_code: None, device_label: "test".to_string() })))
        .await
}

fn expect_login_token(resp: Response) -> String {
    let ResponseBody::LoginResponse(login) = resp.body else { panic!("expected LoginResponse, got {:?}", resp.body) };
    login.token
}

// Registers `username`/`password`, has the bootstrap admin approve it over
// the wire (proving ApproveUserRequest end to end, not just that the
// account happened to already be active), then logs the new user in.
// Leaves at least `wait_out_rate_limit` spacing around both logins it
// performs — callers still need one more before their *own* next login.
async fn register_and_login_approved(stream: &mut TcpStream, username: &str, password: &str) -> String {
    let create_resp =
        send(stream, 0, None, RequestBody::CreateUserRequest(Box::new(CreateUserRequest { username: username.to_string(), password: password.to_string() }))).await;
    let ResponseBody::CreateUserResponse(created) = create_resp.body else { panic!("expected CreateUserResponse, got {:?}", create_resp.body) };

    wait_out_rate_limit().await;
    let admin_token = expect_login_token(login(stream, "admin", "admin").await);

    let approve_resp = send(stream, 0, Some(&admin_token), RequestBody::ApproveUserRequest(Box::new(ApproveUserRequest { user_id: created.user_id.clone() }))).await;
    assert!(matches!(approve_resp.body, ResponseBody::Ack(_)), "expected Ack, got {:?}", approve_resp.body);

    wait_out_rate_limit().await;
    expect_login_token(login(stream, username, password).await)
}

#[tokio::test]
async fn create_user_login_and_call_a_protected_op() {
    let _guard = TEST_LOCK.lock().await;
    wait_out_rate_limit().await;
    let (_dir, addr) = spawn_server().await;
    let mut stream = TcpStream::connect(addr).await.unwrap();

    let token = register_and_login_approved(&mut stream, "alice", "hunter2").await;
    assert!(!token.is_empty());

    let library_resp = send(&mut stream, 3, Some(&token), RequestBody::ListLibraryRequest(Box::new(ListLibraryRequest {}))).await;
    assert_eq!(library_resp.request_id, 3);
    let ResponseBody::ListFilesResponse(files) = library_resp.body else { panic!("expected ListFilesResponse, got {:?}", library_resp.body) };
    assert!(files.files.is_empty());
}

#[tokio::test]
async fn protected_op_without_a_token_is_rejected() {
    let (_dir, addr) = spawn_server().await;
    let mut stream = TcpStream::connect(addr).await.unwrap();

    let resp = send(&mut stream, 1, None, RequestBody::ListLibraryRequest(Box::new(ListLibraryRequest {}))).await;
    let ResponseBody::Error(err) = resp.body else { panic!("expected Error, got {:?}", resp.body) };
    assert_eq!(err.code, ErrorCode::InvalidToken);
}

#[tokio::test]
async fn malformed_id_field_is_rejected_as_invalid_request() {
    let _guard = TEST_LOCK.lock().await;
    wait_out_rate_limit().await;
    let (_dir, addr) = spawn_server().await;
    let mut stream = TcpStream::connect(addr).await.unwrap();

    let token = register_and_login_approved(&mut stream, "bob", "swordfish").await;

    let resp = send(&mut stream, 5, Some(&token), RequestBody::ListAlbumFilesRequest(Box::new(ListAlbumFilesRequest { album_id: "not-a-real-ulid".to_string() }))).await;
    let ResponseBody::Error(err) = resp.body else { panic!("expected Error, got {:?}", resp.body) };
    assert_eq!(err.code, ErrorCode::InvalidRequest);
}

#[tokio::test]
async fn pending_account_is_rejected_until_an_admin_approves_it_over_the_wire() {
    let _guard = TEST_LOCK.lock().await;
    wait_out_rate_limit().await;
    let (_dir, addr) = spawn_server().await;
    let mut stream = TcpStream::connect(addr).await.unwrap();

    send(&mut stream, 0, None, RequestBody::CreateUserRequest(Box::new(CreateUserRequest { username: "carol".to_string(), password: "pw".to_string() }))).await;

    wait_out_rate_limit().await;
    let resp = login(&mut stream, "carol", "pw").await;
    let ResponseBody::Error(err) = resp.body else { panic!("expected Error, got {:?}", resp.body) };
    assert_eq!(err.code, ErrorCode::AccountPending);

    // register_and_login_approved re-registers the same flow cleanly for a
    // fresh user, so just confirm the general path works too.
    let token = register_and_login_approved(&mut stream, "dave", "pw").await;
    assert!(!token.is_empty());
}

#[tokio::test]
async fn upload_then_download_round_trips_real_bytes_over_the_wire() {
    let _guard = TEST_LOCK.lock().await;
    wait_out_rate_limit().await;
    let (_dir, addr) = spawn_server().await;
    let mut stream = TcpStream::connect(addr).await.unwrap();

    let token = register_and_login_approved(&mut stream, "alice", "hunter2").await;

    let data = b"these are the actual photo bytes, sent over a real socket".to_vec();
    let upload_resp = send(
        &mut stream,
        10,
        Some(&token),
        RequestBody::UploadFileRequest(Box::new(UploadFileRequest { data: data.clone(), mime: "image/jpeg".to_string(), width: None, height: None, duration: None, previous_version: None, name: None, taken_at: None })),
    )
    .await;
    let ResponseBody::UploadFileResponse(uploaded) = upload_resp.body else { panic!("expected UploadFileResponse, got {:?}", upload_resp.body) };

    let download_resp = send(
        &mut stream,
        11,
        Some(&token),
        RequestBody::DownloadFileRequest(Box::new(DownloadFileRequest { file_id: uploaded.file_id.clone(), variant: lestobackupper_schema::protocol::ThumbnailVariant::Original })),
    )
    .await;
    let ResponseBody::DownloadFileResponse(downloaded) = download_resp.body else { panic!("expected DownloadFileResponse, got {:?}", download_resp.body) };
    assert_eq!(downloaded.data, data);
    assert_eq!(downloaded.mime, "image/jpeg");

    // It shows up in the library too.
    let library_resp = send(&mut stream, 12, Some(&token), RequestBody::ListLibraryRequest(Box::new(ListLibraryRequest {}))).await;
    let ResponseBody::ListFilesResponse(files) = library_resp.body else { panic!("expected ListFilesResponse, got {:?}", library_resp.body) };
    assert_eq!(files.files.iter().map(|f| f.file_id.clone()).collect::<Vec<_>>(), vec![uploaded.file_id]);
}

#[tokio::test]
async fn find_library_files_by_hash_reports_what_was_uploaded() {
    let _guard = TEST_LOCK.lock().await;
    wait_out_rate_limit().await;
    let (_dir, addr) = spawn_server().await;
    let mut stream = TcpStream::connect(addr).await.unwrap();

    let token = register_and_login_approved(&mut stream, "alice", "hunter2").await;

    // sha256("abc"), the FIPS 180-2 test vector — computed client-side the
    // way a phone would, independently of the server's own hashing.
    let abc_hash = "sha2-256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".to_string();
    let unknown_hash = format!("sha2-256:{}", "00".repeat(32));

    let upload_resp = send(
        &mut stream,
        20,
        Some(&token),
        RequestBody::UploadFileRequest(Box::new(UploadFileRequest { data: b"abc".to_vec(), mime: "image/jpeg".to_string(), width: None, height: None, duration: None, previous_version: None, name: None, taken_at: None })),
    )
    .await;
    let ResponseBody::UploadFileResponse(uploaded) = upload_resp.body else { panic!("expected UploadFileResponse, got {:?}", upload_resp.body) };

    let resp = send(&mut stream, 21, Some(&token), RequestBody::FindLibraryFilesByHashRequest(Box::new(FindLibraryFilesByHashRequest { hashes: vec![abc_hash.clone(), unknown_hash] }))).await;
    let ResponseBody::FindLibraryFilesByHashResponse(found) = resp.body else { panic!("expected FindLibraryFilesByHashResponse, got {:?}", resp.body) };
    assert_eq!(found.matches.len(), 1);
    assert_eq!(found.matches[0].hash, abc_hash);
    assert_eq!(found.matches[0].file_id, uploaded.file_id);

    let resp = send(&mut stream, 22, Some(&token), RequestBody::FindLibraryFilesByHashRequest(Box::new(FindLibraryFilesByHashRequest { hashes: vec!["untagged".to_string()] }))).await;
    let ResponseBody::Error(err) = resp.body else { panic!("expected Error, got {:?}", resp.body) };
    assert_eq!(err.code, ErrorCode::InvalidRequest);
}

#[tokio::test]
async fn library_pages_and_ranged_downloads_over_the_wire() {
    let _guard = TEST_LOCK.lock().await;
    wait_out_rate_limit().await;
    let (_dir, addr) = spawn_server().await;
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let token = register_and_login_approved(&mut stream, "alice", "hunter2").await;

    let mut ids = Vec::new();
    for (i, taken_at) in [(0u8, Some(1_000)), (1, Some(3_000)), (2, Some(2_000))] {
        let resp = send(
            &mut stream,
            30 + u64::from(i),
            Some(&token),
            RequestBody::UploadFileRequest(Box::new(UploadFileRequest {
                data: vec![i; 2000],
                mime: "video/mp4".to_string(),
                width: Some(1920),
                height: Some(1080),
                duration: Some(4_000),
                previous_version: None,
                name: Some(format!("clip{i}.mp4")),
                taken_at,
            })),
        )
        .await;
        let ResponseBody::UploadFileResponse(up) = resp.body else { panic!("expected UploadFileResponse, got {:?}", resp.body) };
        ids.push(up.file_id);
    }

    // Two pages of 2 + 1, newest taken first.
    let resp = send(&mut stream, 40, Some(&token), RequestBody::ListLibraryPageRequest(Box::new(ListLibraryPageRequest { after: None, limit: 2 }))).await;
    let ResponseBody::LibraryPageResponse(first) = resp.body else { panic!("expected LibraryPageResponse, got {:?}", resp.body) };
    assert_eq!(first.files.iter().map(|f| f.file_id.clone()).collect::<Vec<_>>(), vec![ids[1].clone(), ids[2].clone()]);
    let clip = &first.files[0];
    assert_eq!((clip.taken_at, clip.duration, clip.width, clip.mime.as_str(), clip.size), (Some(3_000), Some(4_000), Some(1920), "video/mp4", 2000));
    assert!(clip.hash.starts_with("sha2-256:"));

    let resp = send(&mut stream, 41, Some(&token), RequestBody::ListLibraryPageRequest(Box::new(ListLibraryPageRequest { after: first.next.clone(), limit: 2 }))).await;
    let ResponseBody::LibraryPageResponse(second) = resp.body else { panic!("expected LibraryPageResponse, got {:?}", resp.body) };
    assert_eq!(second.files.iter().map(|f| f.file_id.clone()).collect::<Vec<_>>(), vec![ids[0].clone()]);
    assert_eq!(second.next, None);

    // A byte range of the original.
    let resp = send(&mut stream, 42, Some(&token), RequestBody::DownloadChunkRequest(Box::new(DownloadChunkRequest { file_id: ids[1].clone(), offset: 500, length: 1000 }))).await;
    let ResponseBody::DownloadChunkResponse(chunk) = resp.body else { panic!("expected DownloadChunkResponse, got {:?}", resp.body) };
    assert_eq!((chunk.data, chunk.total_size, chunk.mime.as_str()), (vec![1u8; 1000], 2000, "video/mp4"));

    // Not a decodable video, and too small to matter: the thumbnail falls back to the original.
    let resp = send(
        &mut stream,
        43,
        Some(&token),
        RequestBody::DownloadFileRequest(Box::new(DownloadFileRequest { file_id: ids[0].clone(), variant: lestobackupper_schema::protocol::ThumbnailVariant::Small })),
    )
    .await;
    let ResponseBody::DownloadFileResponse(fallback) = resp.body else { panic!("expected DownloadFileResponse, got {:?}", resp.body) };
    assert_eq!(fallback.data.len(), 2000);
}

#[tokio::test]
async fn non_admin_cannot_approve_accounts() {
    let _guard = TEST_LOCK.lock().await;
    wait_out_rate_limit().await;
    let (_dir, addr) = spawn_server().await;
    let mut stream = TcpStream::connect(addr).await.unwrap();

    let alice_token = register_and_login_approved(&mut stream, "alice", "hunter2").await;

    let create_resp = send(&mut stream, 0, None, RequestBody::CreateUserRequest(Box::new(CreateUserRequest { username: "eve".to_string(), password: "pw".to_string() }))).await;
    let ResponseBody::CreateUserResponse(eve) = create_resp.body else { panic!("expected CreateUserResponse, got {:?}", create_resp.body) };

    let resp = send(&mut stream, 0, Some(&alice_token), RequestBody::ApproveUserRequest(Box::new(ApproveUserRequest { user_id: eve.user_id }))).await;
    let ResponseBody::Error(err) = resp.body else { panic!("expected Error, got {:?}", resp.body) };
    assert_eq!(err.code, ErrorCode::AdminRequired);
}

#[tokio::test]
async fn pipelined_chunks_are_stored_in_order() {
    use sha2::{Digest, Sha256};

    let _guard = TEST_LOCK.lock().await;
    wait_out_rate_limit().await;
    let (_dir, addr) = spawn_server().await;
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let token = register_and_login_approved(&mut stream, "alice", "hunter2").await;

    // Different bytes in every chunk, so a chunk stored in the wrong place shows.
    const CHUNK: usize = 256 * 1024;
    const CHUNKS: usize = 12;
    let data: Vec<u8> = (0..CHUNK * CHUNKS).map(|i| (i / 7 % 251) as u8).collect();
    let hash = format!("sha2-256:{}", Sha256::digest(&data).iter().map(|b| format!("{b:02x}")).collect::<String>());

    // Everything written before reading any answer, with an unrelated
    // request in the middle that may overtake the upload.
    send_only(&mut stream, 100, Some(&token), RequestBody::BeginUploadRequest(Box::new(BeginUploadRequest { hash: hash.clone(), size: data.len() as u64 }))).await;
    for (i, chunk) in data.chunks(CHUNK).enumerate() {
        let body = RequestBody::UploadChunkRequest(Box::new(UploadChunkRequest { hash: hash.clone(), offset: (i * CHUNK) as u64, data: chunk.to_vec() }));
        send_only(&mut stream, 101 + i as u64, Some(&token), body).await;
        if i == CHUNKS / 2 {
            send_only(&mut stream, 200, Some(&token), RequestBody::ListLibraryPageRequest(Box::new(ListLibraryPageRequest { after: None, limit: 10 }))).await;
        }
    }
    let commit = CommitUploadRequest {
        hash: hash.clone(),
        mime: "video/mp4".to_string(),
        width: None,
        height: None,
        duration: None,
        previous_version: None,
        name: Some("clip.mp4".to_string()),
        taken_at: None,
    };
    send_only(&mut stream, 150, Some(&token), RequestBody::CommitUploadRequest(Box::new(commit))).await;

    let mut file_id = None;
    let mut upload_answers = Vec::new();
    for _ in 0..CHUNKS + 3 {
        let resp = receive(&mut stream).await;
        match resp.body {
            ResponseBody::LibraryPageResponse(_) => assert_eq!(resp.request_id, 200),
            ResponseBody::BeginUploadResponse(begin) => {
                assert_eq!(begin.offset, 0);
                upload_answers.push(resp.request_id);
            }
            ResponseBody::Ack(_) => upload_answers.push(resp.request_id),
            ResponseBody::UploadFileResponse(up) => {
                file_id = Some(up.file_id);
                upload_answers.push(resp.request_id);
            }
            other => panic!("request {} failed: {other:?}", resp.request_id),
        }
    }
    let expected: Vec<u64> = (100..=100 + CHUNKS as u64).chain([150]).collect();
    assert_eq!(upload_answers, expected, "the upload's requests ran in the order they were sent");

    let resp = send(
        &mut stream,
        300,
        Some(&token),
        RequestBody::DownloadChunkRequest(Box::new(DownloadChunkRequest { file_id: file_id.unwrap(), offset: 0, length: data.len() as u32 })),
    )
    .await;
    let ResponseBody::DownloadChunkResponse(back) = resp.body else { panic!("expected DownloadChunkResponse, got {:?}", resp.body) };
    assert!(back.data == data, "the stored file is the one sent");
}

#[tokio::test]
async fn sharing_an_album_by_username_over_the_wire() {
    let _guard = TEST_LOCK.lock().await;
    wait_out_rate_limit().await;
    let (_dir, addr) = spawn_server().await;
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let token = register_and_login_approved(&mut stream, "alice", "hunter2").await;
    let created = send(&mut stream, 0, None, RequestBody::CreateUserRequest(Box::new(CreateUserRequest { username: "bob".to_string(), password: "pw".to_string() }))).await;
    assert!(matches!(created.body, ResponseBody::CreateUserResponse(_)));

    let resp = send(&mut stream, 1, Some(&token), RequestBody::CreateAlbumRequest(Box::new(CreateAlbumRequest { name: "Trip".to_string(), kind: AlbumKind::User }))).await;
    let ResponseBody::CreateAlbumResponse(album) = resp.body else { panic!("expected CreateAlbumResponse, got {:?}", resp.body) };

    let share = |username: &str| {
        RequestBody::ShareAlbumWithRequest(Box::new(ShareAlbumWithRequest { album_id: album.album_id.clone(), username: username.to_string(), role: AlbumRole::Writer }))
    };
    let resp = send(&mut stream, 2, Some(&token), share("bob")).await;
    assert!(matches!(resp.body, ResponseBody::Ack(_)), "expected Ack, got {:?}", resp.body);
    let resp = send(&mut stream, 3, Some(&token), share("nobody")).await;
    let ResponseBody::Error(err) = resp.body else { panic!("expected an error, got {:?}", resp.body) };
    assert_eq!(err.code, ErrorCode::UserNotFound);

    let resp = send(&mut stream, 4, Some(&token), RequestBody::ListAlbumMembersRequest(Box::new(ListAlbumMembersRequest { album_id: album.album_id.clone() }))).await;
    let ResponseBody::AlbumMembersResponse(list) = resp.body else { panic!("expected AlbumMembersResponse, got {:?}", resp.body) };
    let members: Vec<(String, AlbumRole)> = list.members.into_iter().map(|m| (m.username, m.role)).collect();
    assert_eq!(members, vec![("alice".to_string(), AlbumRole::Owner), ("bob".to_string(), AlbumRole::Writer)]);
}
