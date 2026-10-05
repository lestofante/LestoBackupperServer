use server::commands::{album, file, user};
use server::{AlbumKind, AlbumRole, EventLog, NodeId, ServerError};

fn open() -> (tempfile::TempDir, EventLog<server::Server>) {
    let dir = tempfile::tempdir().unwrap();
    let log = EventLog::open(dir.path().join("test.db"), NodeId("n".to_string())).unwrap();
    (dir, log)
}

fn upload(log: &EventLog<server::Server>, dir: &tempfile::TempDir, actor: server::UserId, seed: &str) -> server::FileId {
    let ev = file::upload_file(log, &dir.path().join("objects"), actor, seed.as_bytes(), "image/jpeg".to_string(), None, file::FileMetadata::default()).unwrap();
    match ev.action {
        server::Action::UploadFile { file_id, .. } => file_id,
        _ => unreachable!(),
    }
}

fn create_album(log: &EventLog<server::Server>, owner: server::UserId, name: &str) -> server::AlbumId {
    let ev = album::create_album(log, owner, name, AlbumKind::User).unwrap();
    match ev.action {
        server::Action::CreateAlbum { album_id, .. } => album_id,
        _ => unreachable!(),
    }
}

#[test]
fn writer_can_trash_but_not_restore_or_purge() {
    let (dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let bob = user::create_user(&log, "bob", "pw").unwrap().actor;

    let album_id = create_album(&log, alice, "Vacation");
    album::share_album(&log, alice, album_id, bob, AlbumRole::Writer).unwrap();
    let file_id = upload(&log, &dir, alice, "aaaa");
    album::add_to_album(&log, alice, album_id, file_id).unwrap();

    // A writer can hide it...
    album::trash_from_album(&log, bob, album_id, file_id).unwrap();
    assert!(matches!(album::restore_from_trash(&log, bob, album_id, file_id), Err(ServerError::PermissionDenied("owner"))));
    assert!(matches!(album::purge_from_album(&log, bob, album_id, file_id), Err(ServerError::PermissionDenied("owner"))));
}

#[test]
fn admin_can_trash_but_not_restore_or_purge() {
    let (dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let bob = user::create_user(&log, "bob", "pw").unwrap().actor;

    let album_id = create_album(&log, alice, "Vacation");
    album::share_album(&log, alice, album_id, bob, AlbumRole::Admin).unwrap();
    let file_id = upload(&log, &dir, alice, "aaaa");
    album::add_to_album(&log, alice, album_id, file_id).unwrap();

    album::trash_from_album(&log, bob, album_id, file_id).unwrap();
    // Only the owner sees and confirms the trash bin — admin isn't enough.
    assert!(matches!(album::restore_from_trash(&log, bob, album_id, file_id), Err(ServerError::PermissionDenied("owner"))));
    assert!(matches!(album::purge_from_album(&log, bob, album_id, file_id), Err(ServerError::PermissionDenied("owner"))));
}

#[test]
fn cannot_purge_a_file_that_is_still_active() {
    let (dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let album_id = create_album(&log, alice, "Vacation");
    let file_id = upload(&log, &dir, alice, "aaaa");
    album::add_to_album(&log, alice, album_id, file_id).unwrap();

    // Never trashed — purge must refuse, even for the owner.
    assert!(matches!(album::purge_from_album(&log, alice, album_id, file_id), Err(ServerError::NotTrashed)));
}

#[test]
fn owner_can_restore_from_trash() {
    let (dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let album_id = create_album(&log, alice, "Vacation");
    let file_id = upload(&log, &dir, alice, "aaaa");
    album::add_to_album(&log, alice, album_id, file_id).unwrap();

    album::trash_from_album(&log, alice, album_id, file_id).unwrap();
    assert_eq!(server::queries::list_album_files(&log, alice, album_id).unwrap().into_iter().map(|f| f.file_id).collect::<Vec<_>>(), Vec::<server::FileId>::new());

    album::restore_from_trash(&log, alice, album_id, file_id).unwrap();
    assert_eq!(server::queries::list_album_files(&log, alice, album_id).unwrap().into_iter().map(|f| f.file_id).collect::<Vec<_>>(), vec![file_id]);
}

#[test]
fn owner_can_purge_after_trashing() {
    let (dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let album_id = create_album(&log, alice, "Vacation");
    let file_id = upload(&log, &dir, alice, "aaaa");
    album::add_to_album(&log, alice, album_id, file_id).unwrap();

    album::trash_from_album(&log, alice, album_id, file_id).unwrap();
    album::purge_from_album(&log, alice, album_id, file_id).unwrap();

    // Gone even from the trash bin now, not just from the active list.
    assert_eq!(server::queries::list_trash(&log, alice, album_id).unwrap().len(), 0);
    assert!(matches!(album::purge_from_album(&log, alice, album_id, file_id), Err(ServerError::NotTrashed)));
}

#[test]
fn only_owner_can_see_the_trash_bin() {
    let (dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let bob = user::create_user(&log, "bob", "pw").unwrap().actor;

    let album_id = create_album(&log, alice, "Vacation");
    album::share_album(&log, alice, album_id, bob, AlbumRole::Admin).unwrap();
    let file_id = upload(&log, &dir, alice, "aaaa");
    album::add_to_album(&log, alice, album_id, file_id).unwrap();
    album::trash_from_album(&log, alice, album_id, file_id).unwrap();

    assert!(matches!(server::queries::list_trash(&log, bob, album_id), Err(ServerError::PermissionDenied("owner"))));

    let trash = server::queries::list_trash(&log, alice, album_id).unwrap();
    assert_eq!(trash.len(), 1);
    assert_eq!(trash[0].file_id, file_id);
    assert_eq!(trash[0].trashed_by, alice);
}
