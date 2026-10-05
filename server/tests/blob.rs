use server::commands::{album, file, user};
use server::{AlbumKind, AlbumRole, EventLog, NodeId, ServerError};

fn open() -> (tempfile::TempDir, EventLog<server::Server>) {
    let dir = tempfile::tempdir().unwrap();
    let log = EventLog::open(dir.path().join("test.db"), NodeId("n".to_string())).unwrap();
    (dir, log)
}

#[test]
fn upload_then_download_round_trips_the_exact_bytes() {
    let (dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;

    let data = b"these are the actual photo bytes";
    let ev = file::upload_file(&log, &dir.path().join("objects"), alice, data, "image/jpeg".to_string(), None, file::FileMetadata::default()).unwrap();
    let file_id = match ev.action {
        server::Action::UploadFile { file_id, .. } => file_id,
        _ => unreachable!(),
    };

    let (bytes, mime, _name) = file::download_file(&log, &dir.path().join("objects"), alice, file_id, file::FileVariant::Original).unwrap();
    assert_eq!(bytes, data);
    assert_eq!(mime, "image/jpeg");
}

#[test]
fn uploading_identical_bytes_twice_dedups_onto_the_same_file_and_stays_downloadable() {
    let (dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let bob = user::create_user(&log, "bob", "pw").unwrap().actor;
    let objects_dir = dir.path().join("objects");

    let data = b"identical bytes from two different uploaders";
    let ev1 = file::upload_file(&log, &objects_dir, alice, data, "image/png".to_string(), None, file::FileMetadata::default()).unwrap();
    let ev2 = file::upload_file(&log, &objects_dir, bob, data, "image/png".to_string(), None, file::FileMetadata::default()).unwrap();

    let id1 = match ev1.action { server::Action::UploadFile { file_id, .. } => file_id, _ => unreachable!() };
    let id2 = match ev2.action { server::Action::UploadFile { file_id, .. } => file_id, _ => unreachable!() };
    assert_eq!(id1, id2);

    // Both uploaders can read it back — it's genuinely one blob on disk.
    assert_eq!(file::download_file(&log, &objects_dir, alice, id1, file::FileVariant::Original).unwrap().0, data);
    assert_eq!(file::download_file(&log, &objects_dir, bob, id1, file::FileVariant::Original).unwrap().0, data);
}

#[test]
fn download_is_denied_without_ownership_or_album_membership() {
    let (dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let mallory = user::create_user(&log, "mallory", "pw").unwrap().actor;
    let objects_dir = dir.path().join("objects");

    let ev = file::upload_file(&log, &objects_dir, alice, b"private photo", "image/jpeg".to_string(), None, file::FileMetadata::default()).unwrap();
    let file_id = match ev.action { server::Action::UploadFile { file_id, .. } => file_id, _ => unreachable!() };

    assert!(matches!(file::download_file(&log, &objects_dir, mallory, file_id, file::FileVariant::Original), Err(ServerError::FileNotFound)));
}

#[test]
fn download_works_via_album_membership_even_without_owning_the_file() {
    let (dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let bob = user::create_user(&log, "bob", "pw").unwrap().actor;
    let objects_dir = dir.path().join("objects");

    let ev = file::upload_file(&log, &objects_dir, alice, b"shared photo", "image/jpeg".to_string(), None, file::FileMetadata::default()).unwrap();
    let file_id = match ev.action { server::Action::UploadFile { file_id, .. } => file_id, _ => unreachable!() };

    let album_ev = album::create_album(&log, alice, "Vacation", AlbumKind::User).unwrap();
    let album_id = match album_ev.action { server::Action::CreateAlbum { album_id, .. } => album_id, _ => unreachable!() };
    album::add_to_album(&log, alice, album_id, file_id).unwrap();

    // Bob doesn't own it, but reading it fails until he's actually a member.
    assert!(matches!(file::download_file(&log, &objects_dir, bob, file_id, file::FileVariant::Original), Err(ServerError::FileNotFound)));

    album::share_album(&log, alice, album_id, bob, AlbumRole::Reader).unwrap();
    assert_eq!(file::download_file(&log, &objects_dir, bob, file_id, file::FileVariant::Original).unwrap().0, b"shared photo");
}

#[test]
fn small_and_medium_variants_are_resized_jpeg_thumbnails_and_original_is_untouched() {
    let (dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let objects_dir = dir.path().join("objects");

    // A synthetic 600x300 PNG — big enough that both thumbnail sizes
    // actually resize it down, not just pass it through untouched.
    let img = image::RgbImage::from_fn(600, 300, |x, _y| image::Rgb([(x % 256) as u8, 0, 0]));
    let mut png_bytes = Vec::new();
    image::DynamicImage::ImageRgb8(img).write_to(&mut std::io::Cursor::new(&mut png_bytes), image::ImageFormat::Png).unwrap();

    let ev = file::upload_file(&log, &objects_dir, alice, &png_bytes, "image/png".to_string(), None, file::FileMetadata::default()).unwrap();
    let file_id = match ev.action { server::Action::UploadFile { file_id, .. } => file_id, _ => unreachable!() };

    let (small, small_mime, _name) = file::download_file(&log, &objects_dir, alice, file_id, file::FileVariant::Small).unwrap();
    assert_eq!(small_mime, "image/jpeg");
    let small_decoded = image::load_from_memory(&small).unwrap();
    assert!(small_decoded.width() <= 240 && small_decoded.height() <= 240);
    assert!(small_decoded.width() > small_decoded.height(), "aspect ratio (landscape) should survive resizing");

    // Requesting it again must hit the on-disk cache, not regenerate —
    // same bytes back either way, so this only proves it doesn't error.
    let (small_again, _, _) = file::download_file(&log, &objects_dir, alice, file_id, file::FileVariant::Small).unwrap();
    assert_eq!(small, small_again);

    let (medium, _, _) = file::download_file(&log, &objects_dir, alice, file_id, file::FileVariant::Medium).unwrap();
    let medium_decoded = image::load_from_memory(&medium).unwrap();
    assert!(medium_decoded.width() <= 960 && medium_decoded.height() <= 960);
    assert!(medium_decoded.width() > small_decoded.width(), "medium should be bigger than small");

    let (original, original_mime, _name) = file::download_file(&log, &objects_dir, alice, file_id, file::FileVariant::Original).unwrap();
    assert_eq!(original, png_bytes);
    assert_eq!(original_mime, "image/png");
}

#[test]
fn a_non_image_file_falls_back_to_the_original_for_thumbnail_variants() {
    let (dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let objects_dir = dir.path().join("objects");

    let data = b"not actually a video, but not an image either";
    let ev = file::upload_file(&log, &objects_dir, alice, data, "video/mp4".to_string(), None, file::FileMetadata::default()).unwrap();
    let file_id = match ev.action { server::Action::UploadFile { file_id, .. } => file_id, _ => unreachable!() };

    let (small, mime, _name) = file::download_file(&log, &objects_dir, alice, file_id, file::FileVariant::Small).unwrap();
    assert_eq!(small, data);
    assert_eq!(mime, "video/mp4");
}

#[test]
fn upload_can_set_a_name_and_it_shows_up_in_the_library_listing() {
    let (dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let objects_dir = dir.path().join("objects");

    let ev = file::upload_file(&log, &objects_dir, alice, b"named photo", "image/jpeg".to_string(), Some("sunset.jpg".to_string()), file::FileMetadata::default()).unwrap();
    let file_id = match ev.action { server::Action::UploadFile { file_id, .. } => file_id, _ => unreachable!() };

    let library = server::queries::list_library(&log, alice).unwrap();
    assert_eq!(library.iter().find(|f| f.file_id == file_id).unwrap().name.as_deref(), Some("sunset.jpg"));
}

#[test]
fn upload_without_a_name_leaves_it_unset() {
    let (dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let objects_dir = dir.path().join("objects");

    let ev = file::upload_file(&log, &objects_dir, alice, b"nameless photo", "image/jpeg".to_string(), None, file::FileMetadata::default()).unwrap();
    let file_id = match ev.action { server::Action::UploadFile { file_id, .. } => file_id, _ => unreachable!() };

    let library = server::queries::list_library(&log, alice).unwrap();
    assert_eq!(library.iter().find(|f| f.file_id == file_id).unwrap().name, None);
}

#[test]
fn owner_can_rename_a_file_and_it_shows_up_everywhere_the_file_is_visible() {
    let (dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let bob = user::create_user(&log, "bob", "pw").unwrap().actor;
    let objects_dir = dir.path().join("objects");

    let ev = file::upload_file(&log, &objects_dir, alice, b"shared photo", "image/jpeg".to_string(), None, file::FileMetadata::default()).unwrap();
    let file_id = match ev.action { server::Action::UploadFile { file_id, .. } => file_id, _ => unreachable!() };

    let album_ev = album::create_album(&log, alice, "Vacation", AlbumKind::User).unwrap();
    let album_id = match album_ev.action { server::Action::CreateAlbum { album_id, .. } => album_id, _ => unreachable!() };
    album::add_to_album(&log, alice, album_id, file_id).unwrap();
    album::share_album(&log, alice, album_id, bob, AlbumRole::Reader).unwrap();

    // Bob only has visibility through the album (never uploaded or owns
    // it directly) — renaming from there still works, since the name is
    // global to the file, not scoped to the album he's renaming it from.
    file::rename_file(&log, bob, file_id, "beach-day.jpg").unwrap();

    let alice_library = server::queries::list_library(&log, alice).unwrap();
    assert_eq!(alice_library.iter().find(|f| f.file_id == file_id).unwrap().name.as_deref(), Some("beach-day.jpg"));

    let album_files = server::queries::list_album_files(&log, bob, album_id).unwrap();
    assert_eq!(album_files.iter().find(|f| f.file_id == file_id).unwrap().name.as_deref(), Some("beach-day.jpg"));
}

#[test]
fn a_non_member_cannot_rename_a_file_they_cant_see() {
    let (dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let mallory = user::create_user(&log, "mallory", "pw").unwrap().actor;
    let objects_dir = dir.path().join("objects");

    let ev = file::upload_file(&log, &objects_dir, alice, b"private photo", "image/jpeg".to_string(), None, file::FileMetadata::default()).unwrap();
    let file_id = match ev.action { server::Action::UploadFile { file_id, .. } => file_id, _ => unreachable!() };

    assert!(matches!(file::rename_file(&log, mallory, file_id, "stolen.jpg"), Err(ServerError::FileNotFound)));
}
