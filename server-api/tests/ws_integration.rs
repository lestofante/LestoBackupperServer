// Same coverage as tests/tcp_integration.rs, over the WebSocket transport
// instead — this is what the WASM web client actually speaks, since a
// browser sandbox can't open a raw TCP socket.
//
// New accounts start Pending (server::commands::user::create_user), so the
// login test goes through the bootstrap admin
// (server::commands::user::ensure_bootstrap_admin, called in spawn_server)
// to approve the new user first — over the wire, via ApproveUserRequest,
// same as tests/tcp_integration.rs.

use std::sync::Arc;
use std::time::Duration;

use lestobackupper_schema::protocol::{ApproveUserRequest, CreateUserRequest, ListLibraryRequest, LoginRequest, Request, RequestBody, Response, ResponseBody, ResponseRef};
use futures_util::{SinkExt, StreamExt};
use planus::ReadAsRoot;
use server::{EventLog, NodeId, Server};
use server_api::ws;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::MaybeTlsStream;

static TEST_LOCK: Mutex<()> = Mutex::const_new(());

async fn wait_out_rate_limit() {
    tokio::time::sleep(Duration::from_millis(1100)).await;
}

type WsStream = tokio_tungstenite::WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

async fn spawn_server() -> (tempfile::TempDir, WsStream) {
    let dir = tempfile::tempdir().unwrap();
    let log = EventLog::<Server>::open(dir.path().join("test.db"), NodeId("n".to_string())).unwrap();
    server::commands::user::ensure_bootstrap_admin(&log).unwrap();
    let state = Arc::new(server_api::AppState { log, objects_dir: dir.path().join("objects") });

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { continue };
            tokio::spawn(ws::handle_connection(stream, state.clone()));
        }
    });

    let (client, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/")).await.unwrap();
    (dir, client)
}

async fn send(stream: &mut WsStream, request_id: u64, token: Option<&str>, body: RequestBody) -> Response {
    let mut builder = planus::Builder::new();
    let req = Request { request_id, token: token.map(str::to_string), body };
    let bytes = builder.finish(&req, None).to_vec();

    stream.send(Message::Binary(bytes)).await.unwrap();
    let msg = stream.next().await.unwrap().unwrap();
    let Message::Binary(buf) = msg else { panic!("expected a binary WS message, got {msg:?}") };

    Response::try_from(ResponseRef::read_as_root(&buf).unwrap()).unwrap()
}

async fn login(stream: &mut WsStream, username: &str, password: &str) -> Response {
    send(stream, 0, None, RequestBody::LoginRequest(Box::new(LoginRequest { username: username.to_string(), password: password.to_string(), otp_code: None, device_label: "test".to_string() })))
        .await
}

fn expect_login_token(resp: Response) -> String {
    let ResponseBody::LoginResponse(login) = resp.body else { panic!("expected LoginResponse, got {:?}", resp.body) };
    login.token
}

#[tokio::test]
async fn create_user_login_and_call_a_protected_op() {
    let _guard = TEST_LOCK.lock().await;
    wait_out_rate_limit().await;
    let (_dir, mut stream) = spawn_server().await;

    let create_resp = send(
        &mut stream,
        1,
        None,
        RequestBody::CreateUserRequest(Box::new(CreateUserRequest { username: "alice".to_string(), password: "hunter2".to_string() })),
    )
    .await;
    let ResponseBody::CreateUserResponse(alice) = create_resp.body else { panic!("expected CreateUserResponse, got {:?}", create_resp.body) };

    wait_out_rate_limit().await;
    let admin_token = expect_login_token(login(&mut stream, "admin", "admin").await);
    let approve_resp = send(&mut stream, 2, Some(&admin_token), RequestBody::ApproveUserRequest(Box::new(ApproveUserRequest { user_id: alice.user_id.clone() }))).await;
    assert!(matches!(approve_resp.body, ResponseBody::Ack(_)), "expected Ack, got {:?}", approve_resp.body);

    wait_out_rate_limit().await;
    let token = expect_login_token(login(&mut stream, "alice", "hunter2").await);

    let library_resp = send(&mut stream, 3, Some(&token), RequestBody::ListLibraryRequest(Box::new(ListLibraryRequest {}))).await;
    let ResponseBody::ListFilesResponse(files) = library_resp.body else { panic!("expected ListFilesResponse, got {:?}", library_resp.body) };
    assert!(files.files.is_empty());
}

#[tokio::test]
async fn protected_op_without_a_token_is_rejected() {
    let (_dir, mut stream) = spawn_server().await;
    let resp = send(&mut stream, 1, None, RequestBody::ListLibraryRequest(Box::new(ListLibraryRequest {}))).await;
    let ResponseBody::Error(err) = resp.body else { panic!("expected Error, got {:?}", resp.body) };
    assert_eq!(err.code, lestobackupper_schema::protocol::ErrorCode::InvalidToken);
}
