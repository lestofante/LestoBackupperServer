use std::path::PathBuf;
use std::sync::Arc;

use server::{EventLog, NodeId, Server};
use server_api::{AppState, SharedState};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let mut args = std::env::args().skip(1);
    let db_path = args.next().unwrap_or_else(|| {
        eprintln!("usage: lestobackupper-server <db-path> [objects-dir, default <db-path>.objects] [addr, default 0.0.0.0:9000]");
        eprintln!("  one port, TLS only: the app, and the web client at https://<addr>/ (from LESTO_WEB_DIR, default ./web, built with `cargo xtask web`)");
        std::process::exit(1);
    });
    let objects_dir: PathBuf = args.next().map(PathBuf::from).unwrap_or_else(|| PathBuf::from(format!("{db_path}.objects")));
    // All interfaces by default, so phones on the LAN can reach it; pass a
    // 127.0.0.1 address to keep it local-only.
    let addr = args.next().unwrap_or_else(|| "0.0.0.0:9000".to_string());
    if args.next().is_some() {
        tracing::warn!("a 4th argument (the old separate web port) is ignored: the web client is on {addr} too");
    }

    // TLS only (tls.rs); its fingerprint is what the app shows on first login.
    let tls = server_api::tls::from_env(&db_path).unwrap_or_else(|e| {
        eprintln!("TLS: {e}");
        std::process::exit(1);
    });
    tracing::info!(fingerprint = %tls.fingerprint, "TLS certificate (SHA-256): the app shows this fingerprint on first login");
    server_api::tls::configure(tls);

    // Thumbnails decode whole photos: only a few at a time (server::thumbnail).
    let thumbnail_jobs = server_api::config_thumbnail_jobs();
    server::thumbnail::set_max_at_once(thumbnail_jobs);

    // The web client, served on the same port (web.rs).
    let web_dir = server_api::web::dir_from_env();
    server_api::web::configure(web_dir.clone());

    let log = EventLog::<Server>::open(&db_path, NodeId("server-api".to_string())).expect("failed to open database");

    // Created up front rather than on the first upload, so a bad path or
    // missing permission shows up at startup, not as a failed upload later.
    std::fs::create_dir_all(&objects_dir).unwrap_or_else(|e| panic!("failed to create objects dir {}: {e}", objects_dir.display()));

    // A no-op past the very first run against a given db (see the doc
    // comment on `ensure_bootstrap_admin`) — safe to call unconditionally
    // on every startup. Blocking here, not spawn_blocking: this runs once,
    // before the listener is accepting connections, so there's no
    // other async work on this runtime for it to compete with.
    if server::commands::user::ensure_bootstrap_admin(&log).expect("failed to check/create the bootstrap admin account").is_some() {
        tracing::warn!("created default admin account (username \"admin\", password \"admin\") — change the password immediately");
    }

    let state: SharedState = Arc::new(AppState { log, objects_dir: objects_dir.clone() });

    if let Some(config) = server_api::scrub::ScrubConfig::from_env() {
        tokio::spawn(server_api::scrub::run_forever(state.clone(), config));
    }
    tokio::spawn(server_api::housekeeping::run_forever(state.clone(), server_api::housekeeping::settings_from_env()));

    let listener = TcpListener::bind(&addr).await.expect("failed to bind the listener");
    tracing::info!("listening on {addr} (TLS), db {db_path}, objects {}", objects_dir.display());
    match &web_dir {
        Some(dir) => tracing::info!("web client from {} at https://{addr}/", dir.display()),
        None => tracing::warn!("no web client: build it with `cargo xtask web`, or set LESTO_WEB_DIR"),
    }

    loop {
        let Ok((stream, _peer)) = listener.accept().await else { continue };
        tokio::spawn(server_api::handle_connection(stream, state.clone()));
    }
}
