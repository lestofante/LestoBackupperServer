use server::commands::{album, user};
use server::{AlbumKind, AlbumRole, EventLog, NodeId, ServerError};

fn open() -> (tempfile::TempDir, EventLog<server::Server>) {
    let dir = tempfile::tempdir().unwrap();
    let log = EventLog::open(dir.path().join("test.db"), NodeId("n".to_string())).unwrap();
    (dir, log)
}

fn create_album(log: &EventLog<server::Server>, owner: server::UserId, name: &str) -> server::AlbumId {
    match album::create_album(log, owner, name, AlbumKind::User).unwrap().action {
        server::Action::CreateAlbum { album_id, .. } => album_id,
        _ => unreachable!(),
    }
}

#[test]
fn owner_can_rename_and_the_new_name_is_visible_in_the_listing() {
    let (_dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let album_id = create_album(&log, alice, "Vacation");

    album::rename_album(&log, alice, album_id, "Vacation 2024").unwrap();

    let albums = server::queries::list_my_albums(&log, alice).unwrap();
    assert_eq!(albums.iter().find(|a| a.album_id == album_id).unwrap().name, "Vacation 2024");
}

#[test]
fn non_owner_cannot_rename() {
    let (_dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let bob = user::create_user(&log, "bob", "pw").unwrap().actor;
    let album_id = create_album(&log, alice, "Vacation");
    album::share_album(&log, alice, album_id, bob, AlbumRole::Admin).unwrap();

    let result = album::rename_album(&log, bob, album_id, "Hijacked");
    assert!(matches!(result, Err(ServerError::PermissionDenied("owner"))), "{result:?}");
}

#[test]
fn renaming_to_a_name_already_used_by_another_of_the_same_owners_albums_is_rejected() {
    let (_dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    create_album(&log, alice, "Vacation");
    let album_id = create_album(&log, alice, "Work");

    let result = album::rename_album(&log, alice, album_id, "Vacation");
    assert!(matches!(result, Err(ServerError::AlbumNameTaken(n)) if n == "Vacation"));
}

#[test]
fn renaming_to_its_own_current_name_is_allowed() {
    let (_dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let album_id = create_album(&log, alice, "Vacation");

    album::rename_album(&log, alice, album_id, "Vacation").unwrap();
}

#[test]
fn hiding_an_album_removes_it_from_my_albums_but_not_from_another_members_list() {
    let (_dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let bob = user::create_user(&log, "bob", "pw").unwrap().actor;
    let album_id = create_album(&log, alice, "Shared");
    album::share_album(&log, alice, album_id, bob, AlbumRole::Reader).unwrap();

    album::hide_album(&log, alice, album_id).unwrap();

    assert!(server::queries::list_my_albums(&log, alice).unwrap().iter().all(|a| a.album_id != album_id));
    assert!(server::queries::list_my_albums(&log, bob).unwrap().iter().any(|a| a.album_id == album_id));
}

#[test]
fn hidden_album_shows_up_in_list_hidden_albums_and_unhide_restores_it() {
    let (_dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let album_id = create_album(&log, alice, "Vacation");

    album::hide_album(&log, alice, album_id).unwrap();
    let hidden = server::queries::list_hidden_albums(&log, alice).unwrap();
    assert!(hidden.iter().any(|a| a.album_id == album_id));

    album::unhide_album(&log, alice, album_id).unwrap();
    assert!(server::queries::list_my_albums(&log, alice).unwrap().iter().any(|a| a.album_id == album_id));
    assert!(server::queries::list_hidden_albums(&log, alice).unwrap().iter().all(|a| a.album_id != album_id));
}

#[test]
fn a_reader_can_hide_an_album_from_their_own_list() {
    let (_dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let bob = user::create_user(&log, "bob", "pw").unwrap().actor;
    let album_id = create_album(&log, alice, "Shared");
    album::share_album(&log, alice, album_id, bob, AlbumRole::Reader).unwrap();

    album::hide_album(&log, bob, album_id).unwrap();
    assert!(server::queries::list_my_albums(&log, bob).unwrap().iter().all(|a| a.album_id != album_id));
}

#[test]
fn a_non_member_cannot_hide_an_album() {
    let (_dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let outsider = user::create_user(&log, "mallory", "pw").unwrap().actor;
    let album_id = create_album(&log, alice, "Private");

    assert!(matches!(album::hide_album(&log, outsider, album_id), Err(ServerError::AlbumNotFound)));
}
