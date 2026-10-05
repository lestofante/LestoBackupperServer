use server::commands::{album, file, user};
use server::queries;
use server::{AlbumKind, AlbumRole, EventLog, NodeId};

fn open() -> (tempfile::TempDir, EventLog<server::Server>) {
    let dir = tempfile::tempdir().unwrap();
    let log = EventLog::open(dir.path().join("test.db"), NodeId("n".to_string())).unwrap();
    (dir, log)
}

#[test]
fn list_my_albums_reports_role_per_album() {
    let (_dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let bob = user::create_user(&log, "bob", "pw").unwrap().actor;

    let owned = match album::create_album(&log, alice, "Vacation", AlbumKind::User).unwrap().action {
        server::Action::CreateAlbum { album_id, .. } => album_id,
        _ => unreachable!(),
    };
    album::share_album(&log, alice, owned, bob, AlbumRole::Reader).unwrap();

    let alice_albums = queries::list_my_albums(&log, alice).unwrap();
    assert_eq!(alice_albums.len(), 1);
    assert_eq!(alice_albums[0].album_id, owned);
    assert_eq!(alice_albums[0].role, AlbumRole::Owner);

    let bob_albums = queries::list_my_albums(&log, bob).unwrap();
    assert_eq!(bob_albums.len(), 1);
    assert_eq!(bob_albums[0].role, AlbumRole::Reader);
}

#[test]
fn list_library_is_scoped_to_the_caller() {
    let (dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let bob = user::create_user(&log, "bob", "pw").unwrap().actor;

    let alice_file = match file::upload_file(&log, &dir.path().join("objects"), alice, b"aaaa", "image/jpeg".to_string(), None, file::FileMetadata::default()).unwrap().action {
        server::Action::UploadFile { file_id, .. } => file_id,
        _ => unreachable!(),
    };

    assert_eq!(queries::list_library(&log, alice).unwrap().into_iter().map(|f| f.file_id).collect::<Vec<_>>(), vec![alice_file]);
    assert_eq!(queries::list_library(&log, bob).unwrap().into_iter().map(|f| f.file_id).collect::<Vec<_>>(), Vec::<server::FileId>::new());
}

#[test]
fn find_library_files_by_hash_reports_only_the_callers_own_files() {
    let (dir, log) = open();
    let objects = dir.path().join("objects");
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let bob = user::create_user(&log, "bob", "pw").unwrap().actor;

    let (alice_file, alice_hash) = match file::upload_file(&log, &objects, alice, b"alice bytes", "image/jpeg".to_string(), None, file::FileMetadata::default()).unwrap().action {
        server::Action::UploadFile { file_id, hash, .. } => (file_id, hash),
        _ => unreachable!(),
    };
    let bob_hash = match file::upload_file(&log, &objects, bob, b"bob bytes", "image/jpeg".to_string(), None, file::FileMetadata::default()).unwrap().action {
        server::Action::UploadFile { hash, .. } => hash,
        _ => unreachable!(),
    };
    let unknown = server::ObjectId::sha2_256("00".repeat(32));

    let found = queries::find_library_files_by_hash(&log, &objects, alice, &[alice_hash.clone(), bob_hash.clone(), unknown]).unwrap();
    assert_eq!(found.matches, vec![queries::HashMatch { hash: alice_hash, file_id: alice_file }]);
    assert!(found.missing_blobs.is_empty());

    // Bob's bytes exist on the server, but that must not be visible to alice.
    assert_eq!(queries::find_library_files_by_hash(&log, &objects, alice, &[bob_hash]).unwrap(), queries::HashLookup::default());
}

#[test]
fn find_library_files_by_hash_reports_a_lost_blob_as_absent_until_reuploaded() {
    let (dir, log) = open();
    let objects = dir.path().join("objects");
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;

    let upload = || match file::upload_file(&log, &objects, alice, b"precious", "image/jpeg".to_string(), None, file::FileMetadata::default()).unwrap().action {
        server::Action::UploadFile { file_id, hash, .. } => (file_id, hash),
        _ => unreachable!(),
    };
    let (file_id, hash) = upload();

    std::fs::remove_dir_all(&objects).unwrap();
    let lost = queries::find_library_files_by_hash(&log, &objects, alice, std::slice::from_ref(&hash)).unwrap();
    assert!(lost.matches.is_empty());
    assert_eq!(lost.missing_blobs, vec![hash.clone()]);

    // Re-uploading the same bytes restores the blob under the same file_id.
    assert_eq!(upload(), (file_id, hash.clone()));
    let healed = queries::find_library_files_by_hash(&log, &objects, alice, std::slice::from_ref(&hash)).unwrap();
    assert_eq!(healed.matches, vec![queries::HashMatch { hash, file_id }]);
    assert!(healed.missing_blobs.is_empty());
}

#[test]
fn list_album_files_excludes_trashed() {
    let (dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let album_id = match album::create_album(&log, alice, "Vacation", AlbumKind::User).unwrap().action {
        server::Action::CreateAlbum { album_id, .. } => album_id,
        _ => unreachable!(),
    };
    let file_id = match file::upload_file(&log, &dir.path().join("objects"), alice, b"aaaa", "image/jpeg".to_string(), None, file::FileMetadata::default()).unwrap().action {
        server::Action::UploadFile { file_id, .. } => file_id,
        _ => unreachable!(),
    };
    album::add_to_album(&log, alice, album_id, file_id).unwrap();

    assert_eq!(queries::list_album_files(&log, alice, album_id).unwrap().into_iter().map(|f| f.file_id).collect::<Vec<_>>(), vec![file_id]);
    album::trash_from_album(&log, alice, album_id, file_id).unwrap();
    assert_eq!(queries::list_album_files(&log, alice, album_id).unwrap().into_iter().map(|f| f.file_id).collect::<Vec<_>>(), Vec::<server::FileId>::new());
}

#[test]
fn list_albums_for_files_batches_membership_across_multiple_files_and_albums() {
    let (dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;

    let vacation = match album::create_album(&log, alice, "Vacation", AlbumKind::User).unwrap().action {
        server::Action::CreateAlbum { album_id, .. } => album_id,
        _ => unreachable!(),
    };
    let work = match album::create_album(&log, alice, "Work", AlbumKind::User).unwrap().action {
        server::Action::CreateAlbum { album_id, .. } => album_id,
        _ => unreachable!(),
    };

    let in_both = match file::upload_file(&log, &dir.path().join("objects"), alice, b"aaaa", "image/jpeg".to_string(), None, file::FileMetadata::default()).unwrap().action {
        server::Action::UploadFile { file_id, .. } => file_id,
        _ => unreachable!(),
    };
    let in_neither = match file::upload_file(&log, &dir.path().join("objects"), alice, b"bbbb", "image/jpeg".to_string(), None, file::FileMetadata::default()).unwrap().action {
        server::Action::UploadFile { file_id, .. } => file_id,
        _ => unreachable!(),
    };
    album::add_to_album(&log, alice, vacation, in_both).unwrap();
    album::add_to_album(&log, alice, work, in_both).unwrap();

    let result = queries::list_albums_for_files(&log, alice, &[in_both, in_neither]).unwrap();
    let mut in_both_albums: Vec<_> = result[&in_both].iter().map(|a| a.album_id).collect();
    in_both_albums.sort();
    let mut expected = vec![vacation, work];
    expected.sort();
    assert_eq!(in_both_albums, expected);
    assert!(!result.contains_key(&in_neither));
}

#[test]
fn list_albums_for_files_excludes_trashed_membership() {
    let (dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let album_id = match album::create_album(&log, alice, "Vacation", AlbumKind::User).unwrap().action {
        server::Action::CreateAlbum { album_id, .. } => album_id,
        _ => unreachable!(),
    };
    let file_id = match file::upload_file(&log, &dir.path().join("objects"), alice, b"aaaa", "image/jpeg".to_string(), None, file::FileMetadata::default()).unwrap().action {
        server::Action::UploadFile { file_id, .. } => file_id,
        _ => unreachable!(),
    };
    album::add_to_album(&log, alice, album_id, file_id).unwrap();
    album::trash_from_album(&log, alice, album_id, file_id).unwrap();

    let result = queries::list_albums_for_files(&log, alice, &[file_id]).unwrap();
    assert!(!result.contains_key(&file_id));
}
