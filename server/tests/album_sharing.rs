use server::commands::{album, user};
use server::queries;
use server::{AlbumId, AlbumKind, AlbumRole, EventLog, NodeId, ServerError, UserId};

fn open() -> (tempfile::TempDir, EventLog<server::Server>) {
    let dir = tempfile::tempdir().unwrap();
    let log = EventLog::open(dir.path().join("test.db"), NodeId("n".to_string())).unwrap();
    (dir, log)
}

fn create(log: &EventLog<server::Server>, owner: UserId, name: &str) -> AlbumId {
    match album::create_album(log, owner, name, AlbumKind::User).unwrap().action {
        server::Action::CreateAlbum { album_id, .. } => album_id,
        _ => unreachable!(),
    }
}

fn members(log: &EventLog<server::Server>, actor: UserId, album_id: AlbumId) -> Vec<(String, AlbumRole)> {
    queries::list_album_members(log, actor, album_id).unwrap().into_iter().map(|m| (m.username, m.role)).collect()
}

#[test]
fn sharing_by_username_and_listing_members() {
    let (_dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let bob = user::create_user(&log, "bob", "pw").unwrap().actor;
    user::create_user(&log, "carol", "pw").unwrap();
    let trip = create(&log, alice, "Trip");

    album::share_album_with(&log, alice, trip, "carol", AlbumRole::Writer).unwrap();
    album::share_album_with(&log, alice, trip, "bob", AlbumRole::Reader).unwrap();
    assert_eq!(members(&log, alice, trip), vec![("alice".into(), AlbumRole::Owner), ("carol".into(), AlbumRole::Writer), ("bob".into(), AlbumRole::Reader)]);
    // Any member sees who else is in it.
    assert_eq!(members(&log, bob, trip).len(), 3);

    // Sharing again changes the role.
    album::share_album_with(&log, alice, trip, "bob", AlbumRole::Admin).unwrap();
    assert_eq!(members(&log, alice, trip)[1], ("bob".into(), AlbumRole::Admin));

    assert!(matches!(album::share_album_with(&log, alice, trip, "nobody", AlbumRole::Reader), Err(ServerError::UserNotFound)));
}

#[test]
fn strangers_see_no_members() {
    let (_dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let mallory = user::create_user(&log, "mallory", "pw").unwrap().actor;
    let trip = create(&log, alice, "Trip");
    assert!(matches!(queries::list_album_members(&log, mallory, trip), Err(ServerError::AlbumNotFound)));
}

#[test]
fn an_admin_cannot_make_owners_or_touch_them() {
    let (_dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let bob = user::create_user(&log, "bob", "pw").unwrap().actor;
    let carol = user::create_user(&log, "carol", "pw").unwrap().actor;
    let trip = create(&log, alice, "Trip");
    album::share_album(&log, alice, trip, bob, AlbumRole::Admin).unwrap();

    // Bob may share, and make other admins...
    album::share_album(&log, bob, trip, carol, AlbumRole::Admin).unwrap();
    album::unshare_album(&log, bob, trip, carol).unwrap();
    // ...but not owners, not himself either, and he can't demote or remove alice.
    assert!(matches!(album::share_album(&log, bob, trip, carol, AlbumRole::Owner), Err(ServerError::PermissionDenied("owner"))));
    assert!(matches!(album::share_album(&log, bob, trip, bob, AlbumRole::Owner), Err(ServerError::PermissionDenied("owner"))));
    assert!(matches!(album::share_album(&log, bob, trip, alice, AlbumRole::Reader), Err(ServerError::PermissionDenied("owner"))));
    assert!(matches!(album::unshare_album(&log, bob, trip, alice), Err(ServerError::PermissionDenied("owner"))));
    assert_eq!(members(&log, alice, trip)[0], ("alice".into(), AlbumRole::Owner));
}

#[test]
fn the_last_owner_stays_until_ownership_is_handed_over() {
    let (_dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let bob = user::create_user(&log, "bob", "pw").unwrap().actor;
    let trip = create(&log, alice, "Trip");

    assert!(matches!(album::unshare_album(&log, alice, trip, alice), Err(ServerError::InvalidArgument(_))));
    assert!(matches!(album::share_album(&log, alice, trip, alice, AlbumRole::Admin), Err(ServerError::InvalidArgument(_))));

    // Hand over, then step down.
    album::share_album(&log, alice, trip, bob, AlbumRole::Owner).unwrap();
    album::unshare_album(&log, alice, trip, alice).unwrap();
    assert_eq!(members(&log, bob, trip), vec![("bob".into(), AlbumRole::Owner)]);
}

#[test]
fn any_member_can_leave_but_not_remove_others() {
    let (_dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let bob = user::create_user(&log, "bob", "pw").unwrap().actor;
    let carol = user::create_user(&log, "carol", "pw").unwrap().actor;
    let trip = create(&log, alice, "Trip");
    album::share_album(&log, alice, trip, bob, AlbumRole::Writer).unwrap();
    album::share_album(&log, alice, trip, carol, AlbumRole::Reader).unwrap();

    assert!(matches!(album::unshare_album(&log, bob, trip, carol), Err(ServerError::PermissionDenied("admin"))));
    album::unshare_album(&log, carol, trip, carol).unwrap();
    assert!(queries::list_my_albums(&log, carol).unwrap().is_empty());
    assert!(matches!(album::unshare_album(&log, alice, trip, carol), Err(ServerError::UserNotFound)), "no longer a member");
}

#[test]
fn sharing_with_an_unknown_user_id_is_rejected() {
    let (_dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let trip = create(&log, alice, "Trip");
    assert!(matches!(album::share_album(&log, alice, trip, UserId::new(), AlbumRole::Reader), Err(ServerError::UserNotFound)));
}
