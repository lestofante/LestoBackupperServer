use server::commands::{album, file, user};
use server::queries;
use server::{AlbumId, AlbumKind, AlbumRole, EventLog, FileId, NodeId, ServerError, UserId};

fn open() -> (tempfile::TempDir, EventLog<server::Server>) {
    let dir = tempfile::tempdir().unwrap();
    let log = EventLog::open(dir.path().join("test.db"), NodeId("n".to_string())).unwrap();
    (dir, log)
}

fn upload(log: &EventLog<server::Server>, dir: &tempfile::TempDir, actor: UserId, data: &[u8], taken_at: Option<i64>) -> FileId {
    let meta = file::FileMetadata { taken_at, ..Default::default() };
    match file::upload_file(log, &dir.path().join("objects"), actor, data, "image/jpeg".to_string(), None, meta).unwrap().action {
        server::Action::UploadFile { file_id, .. } => file_id,
        _ => unreachable!(),
    }
}

fn create(log: &EventLog<server::Server>, owner: UserId, name: &str) -> AlbumId {
    match album::create_album(log, owner, name, AlbumKind::User).unwrap().action {
        server::Action::CreateAlbum { album_id, .. } => album_id,
        _ => unreachable!(),
    }
}

#[test]
fn deleting_an_album_removes_it_for_everyone_but_keeps_the_files() {
    let (dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let bob = user::create_user(&log, "bob", "pw").unwrap().actor;
    let photo = upload(&log, &dir, alice, b"photo", None);
    let trip = create(&log, alice, "Trip");
    album::add_to_album(&log, alice, trip, photo).unwrap();
    album::share_album(&log, alice, trip, bob, AlbumRole::Writer).unwrap();

    assert!(matches!(album::delete_album(&log, bob, trip), Err(ServerError::PermissionDenied(_))), "a writer can't delete");
    album::delete_album(&log, alice, trip).unwrap();

    assert!(queries::list_my_albums(&log, alice).unwrap().is_empty());
    assert!(queries::list_my_albums(&log, bob).unwrap().is_empty());
    assert!(matches!(queries::list_album_page(&log, alice, trip, None, 10), Err(ServerError::AlbumNotFound)));
    assert_eq!(queries::list_library(&log, alice).unwrap().len(), 1, "the photo stays in the library");
    // The name is free again.
    create(&log, alice, "Trip");
}

#[test]
fn removing_from_the_library_keeps_the_file_reachable_through_albums() {
    let (dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let objects = dir.path().join("objects");
    let photo = upload(&log, &dir, alice, b"photo", None);
    let kept = upload(&log, &dir, alice, b"kept", None);
    let trip = create(&log, alice, "Trip");
    album::add_to_album(&log, alice, trip, photo).unwrap();

    file::remove_from_library(&log, alice, photo).unwrap();
    let library: Vec<_> = queries::list_library(&log, alice).unwrap().into_iter().map(|f| f.file_id).collect();
    assert_eq!(library, vec![kept]);
    assert!(file::download_file(&log, &objects, alice, photo, file::FileVariant::Original).is_ok(), "still in an album alice is in");
    assert!(matches!(file::remove_from_library(&log, alice, photo), Err(ServerError::FileNotFound)), "not in the library any more");

    // Uploading the same bytes again brings it back.
    assert_eq!(upload(&log, &dir, alice, b"photo", None), photo);
    assert_eq!(queries::list_library(&log, alice).unwrap().len(), 2);
}

#[test]
fn album_write_access_is_not_a_way_to_reach_other_peoples_files() {
    let (dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let bob = user::create_user(&log, "bob", "pw").unwrap().actor;
    let alices_private = upload(&log, &dir, alice, b"private", None);
    let bobs = upload(&log, &dir, bob, b"bob's", None);
    let shared = create(&log, alice, "Shared");
    album::share_album(&log, alice, shared, bob, AlbumRole::Writer).unwrap();

    let bobs_album = create(&log, bob, "Mine");
    assert!(matches!(album::add_to_album(&log, bob, bobs_album, alices_private), Err(ServerError::FileNotFound)));
    album::add_to_album(&log, bob, shared, bobs).unwrap();
}

#[test]
fn re_adding_a_trashed_file_restores_it_for_the_owner_only() {
    let (dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let bob = user::create_user(&log, "bob", "pw").unwrap().actor;
    let photo = upload(&log, &dir, alice, b"photo", None);
    let trip = create(&log, alice, "Trip");
    album::share_album(&log, alice, trip, bob, AlbumRole::Writer).unwrap();
    album::add_to_album(&log, alice, trip, photo).unwrap();
    album::trash_from_album(&log, alice, trip, photo).unwrap();
    assert!(queries::list_album_page(&log, alice, trip, None, 10).unwrap().files.is_empty());

    // Bob can see it through... nothing now (trashed), and can't restore.
    assert!(album::add_to_album(&log, bob, trip, photo).is_err());

    album::add_to_album(&log, alice, trip, photo).unwrap();
    assert_eq!(queries::list_album_page(&log, alice, trip, None, 10).unwrap().files.len(), 1, "back from the trash");
}

#[test]
fn album_summaries_count_active_files_and_pick_the_newest_as_cover() {
    let (dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let old = upload(&log, &dir, alice, b"old", Some(1_000));
    let new = upload(&log, &dir, alice, b"new", Some(5_000));
    let trashed = upload(&log, &dir, alice, b"trashed", Some(9_000));
    let trip = create(&log, alice, "Trip");
    let empty = create(&log, alice, "Empty");
    for f in [old, new, trashed] {
        album::add_to_album(&log, alice, trip, f).unwrap();
    }
    album::trash_from_album(&log, alice, trip, trashed).unwrap();

    let albums = queries::list_my_albums(&log, alice).unwrap();
    let trip_summary = albums.iter().find(|a| a.album_id == trip).unwrap();
    assert_eq!((trip_summary.file_count, trip_summary.cover_file_id), (2, Some(new)));
    let empty_summary = albums.iter().find(|a| a.album_id == empty).unwrap();
    assert_eq!((empty_summary.file_count, empty_summary.cover_file_id), (0, None));
}

#[test]
fn album_pages_are_newest_first_and_members_only() {
    let (dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let eve = user::create_user(&log, "eve", "pw").unwrap().actor;
    let trip = create(&log, alice, "Trip");
    let mut expected = Vec::new();
    for i in 0..7i64 {
        let f = upload(&log, &dir, alice, format!("photo {i}").as_bytes(), Some(1_000 * i));
        album::add_to_album(&log, alice, trip, f).unwrap();
        expected.push(f.to_string());
    }
    expected.reverse();

    let mut seen = Vec::new();
    let mut after = None;
    loop {
        let page = queries::list_album_page(&log, alice, trip, after.as_deref(), 3).unwrap();
        seen.extend(page.files.into_iter().map(|f| f.file_id));
        match page.next {
            Some(n) => after = Some(n),
            None => break,
        }
    }
    assert_eq!(seen, expected);
    assert!(matches!(queries::list_album_page(&log, eve, trip, None, 10), Err(ServerError::AlbumNotFound)), "not a member");
}
