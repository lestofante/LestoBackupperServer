// A minimal end-to-end walk through the domain layer: two users, a shared
// album, a deduped upload, and the trash-bin lifecycle.

use server::commands::{album, file, user};
use server::{queries, AlbumKind, AlbumRole, EventLog, NodeId, Server};

fn main() {
    let path = std::env::temp_dir().join("server-gallery-example.db");
    let _ = std::fs::remove_file(&path);
    let objects_dir = std::env::temp_dir().join("server-gallery-example-objects");
    let log = EventLog::<Server>::open(&path, NodeId("this-machine".to_string())).unwrap();

    let alice = user::create_user(&log, "alice", "hunter2").unwrap().actor;
    let bob = user::create_user(&log, "bob", "correct-horse-battery-staple").unwrap().actor;

    let uploaded = file::upload_file(
        &log,
        &objects_dir,
        alice,
        b"not a real jpeg, just example bytes for this walkthrough",
        "image/jpeg".to_string(),
        Some("beach-sunset.jpg".to_string()),
        file::FileMetadata { width: Some(4032), height: Some(3024), ..Default::default() },
    )
    .unwrap();
    let file_id = match uploaded.action {
        server::Action::UploadFile { file_id, .. } => file_id,
        _ => unreachable!(),
    };

    let created = album::create_album(&log, alice, "Vacation", AlbumKind::User).unwrap();
    let album_id = match created.action {
        server::Action::CreateAlbum { album_id, .. } => album_id,
        _ => unreachable!(),
    };

    album::add_to_album(&log, alice, album_id, file_id).unwrap();
    album::share_album(&log, alice, album_id, bob, AlbumRole::Reader).unwrap();

    assert!(user::verify_password(&log, "alice", "hunter2").unwrap().is_some());
    assert!(user::verify_password(&log, "alice", "wrong").unwrap().is_none());

    let bob_role = album::role_of(&log, album_id, bob).unwrap();
    println!("bob's role on album {album_id} is {bob_role:?}");

    println!("alice's albums: {:?}", queries::list_my_albums(&log, alice).unwrap());
    println!("alice's library: {:?}", queries::list_library(&log, alice).unwrap());

    // Bob can only read here, so hiding the file is not his to do.
    assert!(matches!(album::trash_from_album(&log, bob, album_id, file_id), Err(server::ServerError::PermissionDenied("writer"))));

    // Alice (owner) hides it — reversible, still referenced, just out of view.
    album::trash_from_album(&log, alice, album_id, file_id).unwrap();
    println!("active files after trash: {:?}", queries::list_album_files(&log, alice, album_id).unwrap());
    println!("alice's trash bin: {:?}", queries::list_trash(&log, alice, album_id).unwrap());

    // Bob can't see the trash bin at all — owner-only.
    assert!(matches!(queries::list_trash(&log, bob, album_id), Err(server::ServerError::PermissionDenied("owner"))));

    album::restore_from_trash(&log, alice, album_id, file_id).unwrap();
    println!("active files after restore: {:?}", queries::list_album_files(&log, alice, album_id).unwrap());
}
