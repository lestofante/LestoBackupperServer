use server::commands::{album, file, user};
use server::{AlbumKind, AlbumRole, EventLog, NodeId, ServerError};

// Returns the TempDir too: dropping it deletes the directory, so it must
// outlive every use of the EventLog built on top of it.
fn open() -> (tempfile::TempDir, EventLog<server::Server>) {
    let dir = tempfile::tempdir().unwrap();
    let log = EventLog::open(dir.path().join("test.db"), NodeId("n".to_string())).unwrap();
    (dir, log)
}

#[test]
fn reader_cannot_add_files() {
    let (dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let bob = user::create_user(&log, "bob", "pw").unwrap().actor;

    let album_ev = album::create_album(&log, alice, "Vacation", AlbumKind::User).unwrap();
    let album_id = match album_ev.action {
        server::Action::CreateAlbum { album_id, .. } => album_id,
        _ => unreachable!(),
    };
    album::share_album(&log, alice, album_id, bob, AlbumRole::Reader).unwrap();

    let file_ev = file::upload_file(&log, &dir.path().join("objects"), alice, b"cccc", "image/jpeg".to_string(), None, file::FileMetadata::default()).unwrap();
    let file_id = match file_ev.action {
        server::Action::UploadFile { file_id, .. } => file_id,
        _ => unreachable!(),
    };

    let result = album::add_to_album(&log, bob, album_id, file_id);
    assert!(matches!(result, Err(ServerError::PermissionDenied("writer"))), "{result:?}");
}

#[test]
fn writer_cannot_share() {
    let (_dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let bob = user::create_user(&log, "bob", "pw").unwrap().actor;
    let carol = user::create_user(&log, "carol", "pw").unwrap().actor;

    let album_ev = album::create_album(&log, alice, "Vacation", AlbumKind::User).unwrap();
    let album_id = match album_ev.action {
        server::Action::CreateAlbum { album_id, .. } => album_id,
        _ => unreachable!(),
    };
    album::share_album(&log, alice, album_id, bob, AlbumRole::Writer).unwrap();

    let result = album::share_album(&log, bob, album_id, carol, AlbumRole::Reader);
    assert!(matches!(result, Err(ServerError::PermissionDenied("admin"))), "{result:?}");
}

#[test]
fn non_member_is_rejected() {
    let (dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let outsider = user::create_user(&log, "mallory", "pw").unwrap().actor;

    let album_ev = album::create_album(&log, alice, "Private", AlbumKind::User).unwrap();
    let album_id = match album_ev.action {
        server::Action::CreateAlbum { album_id, .. } => album_id,
        _ => unreachable!(),
    };

    assert!(album::role_of(&log, album_id, outsider).unwrap().is_none());

    let file_ev = file::upload_file(&log, &dir.path().join("objects"), alice, b"dddd", "image/jpeg".to_string(), None, file::FileMetadata::default()).unwrap();
    let file_id = match file_ev.action {
        server::Action::UploadFile { file_id, .. } => file_id,
        _ => unreachable!(),
    };
    assert!(matches!(album::add_to_album(&log, outsider, album_id, file_id), Err(ServerError::AlbumNotFound)));
}

#[test]
fn duplicate_username_is_rejected() {
    let (_dir, log) = open();
    user::create_user(&log, "alice", "pw").unwrap();
    assert!(matches!(user::create_user(&log, "alice", "other-pw"), Err(ServerError::UsernameTaken(u)) if u == "alice"));
}

#[test]
fn duplicate_album_name_per_owner_is_rejected_but_other_owners_can_reuse_it() {
    let (_dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let bob = user::create_user(&log, "bob", "pw").unwrap().actor;

    album::create_album(&log, alice, "Vacation", AlbumKind::User).unwrap();
    assert!(matches!(album::create_album(&log, alice, "Vacation", AlbumKind::User), Err(ServerError::AlbumNameTaken(n)) if n == "Vacation"));

    // Same name, different owner: scoped per-owner uniqueness (server.md).
    album::create_album(&log, bob, "Vacation", AlbumKind::User).unwrap();
}
