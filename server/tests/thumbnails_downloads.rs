use std::process::Command;

use server::commands::{file, user};
use server::{EventLog, FileId, NodeId, ServerError, UserId};

fn open() -> (tempfile::TempDir, EventLog<server::Server>) {
    let dir = tempfile::tempdir().unwrap();
    let log = EventLog::open(dir.path().join("test.db"), NodeId("n".to_string())).unwrap();
    (dir, log)
}

fn upload(log: &EventLog<server::Server>, objects: &std::path::Path, actor: UserId, data: &[u8], mime: &str) -> (FileId, server::ObjectId) {
    match file::upload_file(log, objects, actor, data, mime.to_string(), None, file::FileMetadata::default()).unwrap().action {
        server::Action::UploadFile { file_id, hash, .. } => (file_id, hash),
        _ => unreachable!(),
    }
}

fn is_jpeg(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xFF, 0xD8])
}

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

#[test]
fn a_video_gets_a_frame_as_its_thumbnail_not_the_whole_video() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not installed, skipping");
        return;
    }
    let (dir, log) = open();
    let objects = dir.path().join("objects");
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;

    let clip = dir.path().join("clip.mp4");
    let made = Command::new("ffmpeg")
        .args(["-v", "error", "-f", "lavfi", "-i", "testsrc=duration=2:size=640x480:rate=10", "-pix_fmt", "yuv420p"])
        .arg(&clip)
        .status()
        .unwrap();
    assert!(made.success());
    let video = std::fs::read(&clip).unwrap();
    let (file_id, _) = upload(&log, &objects, alice, &video, "video/mp4");

    let (thumb, mime, _) = file::download_file(&log, &objects, alice, file_id, file::FileVariant::Small).unwrap();
    assert_eq!(mime, "image/jpeg");
    assert!(is_jpeg(&thumb));
    let img = image::load_from_memory(&thumb).unwrap();
    assert_eq!((img.width(), img.height()), (240, 180), "fitted in the Small box, aspect kept");
}

#[test]
fn without_a_thumbnail_only_a_small_original_stands_in() {
    let (dir, log) = open();
    let objects = dir.path().join("objects");
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;

    let small = vec![1u8; 1024];
    let (small_id, _) = upload(&log, &objects, alice, &small, "application/octet-stream");
    let (bytes, mime, _) = file::download_file(&log, &objects, alice, small_id, file::FileVariant::Small).unwrap();
    assert_eq!((bytes, mime.as_str()), (small, "application/octet-stream"));

    let big = vec![2u8; 3 * 1024 * 1024];
    let (big_id, _) = upload(&log, &objects, alice, &big, "application/octet-stream");
    assert!(matches!(file::download_file(&log, &objects, alice, big_id, file::FileVariant::Small), Err(ServerError::NoThumbnail)));
}

#[test]
fn a_cached_thumbnail_does_not_read_the_original() {
    let (dir, log) = open();
    let objects = dir.path().join("objects");
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;

    let mut png = Vec::new();
    image::DynamicImage::new_rgb8(800, 600).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
    let (file_id, hash) = upload(&log, &objects, alice, &png, "image/png");

    let (first, _, _) = file::download_file(&log, &objects, alice, file_id, file::FileVariant::Small).unwrap();
    std::fs::remove_file(objects.join(hash.to_string())).unwrap();
    let (again, mime, _) = file::download_file(&log, &objects, alice, file_id, file::FileVariant::Small).unwrap();
    assert_eq!((again, mime.as_str()), (first, "image/jpeg"));
}

#[test]
fn ranges_reassemble_the_file_and_whole_downloads_are_capped() {
    let (dir, log) = open();
    let objects = dir.path().join("objects");
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let bob = user::create_user(&log, "bob", "pw").unwrap().actor;

    let data: Vec<u8> = (0..16 * 1024 * 1024 + 100u32).map(|i| (i % 251) as u8).collect();
    let (file_id, _) = upload(&log, &objects, alice, &data, "video/mp4");

    assert!(matches!(file::download_file(&log, &objects, alice, file_id, file::FileVariant::Original), Err(ServerError::TooLarge { .. })));

    let mut got = Vec::new();
    loop {
        // Asking for more than the cap gets the cap.
        let range = file::download_range(&log, &objects, alice, file_id, got.len() as u64, u32::MAX).unwrap();
        assert_eq!(range.total_size, data.len() as u64);
        assert!(range.data.len() <= file::MAX_DOWNLOAD_CHUNK_BYTES as usize);
        if range.data.is_empty() {
            break;
        }
        got.extend(range.data);
    }
    assert_eq!(got, data);

    assert!(matches!(file::download_range(&log, &objects, alice, file_id, data.len() as u64 + 1, 10), Err(ServerError::InvalidArgument(_))));
    assert!(matches!(file::download_range(&log, &objects, bob, file_id, 0, 10), Err(ServerError::FileNotFound)), "same visibility as download_file");
}

#[test]
fn a_photo_stored_sideways_with_an_exif_flag_gets_an_upright_thumbnail() {
    let (dir, log) = open();
    let objects = dir.path().join("objects");
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;

    // 80x40 pixels, EXIF orientation 6: displayed as a 40x80 portrait.
    let photo = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/landscape_pixels_exif_rotate90.jpg")).unwrap();
    let (file_id, _) = upload(&log, &objects, alice, &photo, "image/jpeg");

    let (thumb, _, _) = file::download_file(&log, &objects, alice, file_id, file::FileVariant::Small).unwrap();
    let img = image::load_from_memory(&thumb).unwrap();
    assert!(img.height() > img.width(), "upright portrait, got {}x{}", img.width(), img.height());
}
