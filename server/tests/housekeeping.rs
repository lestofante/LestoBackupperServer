use std::path::Path;
use std::time::Duration;

use server::commands::{album, file, user};
use server::housekeeping::{self, Settings};
use server::scrub;
use server::{AlbumKind, EventLog, FileId, NodeId, ObjectId, UserId};

const WEEK: Duration = Duration::from_secs(7 * 24 * 3600);

fn open() -> (tempfile::TempDir, EventLog<server::Server>) {
    let dir = tempfile::tempdir().unwrap();
    let log = EventLog::open(dir.path().join("test.db"), NodeId("n".to_string())).unwrap();
    (dir, log)
}

fn upload(log: &EventLog<server::Server>, objects: &Path, actor: UserId, data: &[u8]) -> (FileId, ObjectId) {
    match file::upload_file(log, objects, actor, data, "image/jpeg".to_string(), None, file::FileMetadata::default()).unwrap().action {
        server::Action::UploadFile { file_id, hash, .. } => (file_id, hash),
        _ => unreachable!(),
    }
}

/// Nothing is old enough for anything time-based to happen.
fn patient() -> Settings {
    Settings { upload_expiry: WEEK, reclaim_after: Some(WEEK), leftover_min_age: WEEK }
}

/// Everything is old enough.
fn eager() -> Settings {
    Settings { upload_expiry: Duration::ZERO, reclaim_after: Some(Duration::ZERO), leftover_min_age: Duration::ZERO }
}

#[test]
fn an_abandoned_partial_upload_expires_and_a_recent_one_stays() {
    let (dir, log) = open();
    let objects = dir.path().join("objects");
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let hash = ObjectId::sha2_256("ab".repeat(32));
    file::begin_upload(&objects, alice, &hash, 100).unwrap();
    file::upload_chunk(&objects, alice, &hash, 0, &[7u8; 40]).unwrap();

    let report = housekeeping::run(&log, &objects, patient()).unwrap();
    assert_eq!(report.expired_uploads, 0);
    assert_eq!(file::begin_upload(&objects, alice, &hash, 100).unwrap(), 40, "still resumable");

    let report = housekeeping::run(&log, &objects, eager()).unwrap();
    assert_eq!(report.expired_uploads, 1);
    assert_eq!(report.freed_bytes, 40);
    assert_eq!(file::begin_upload(&objects, alice, &hash, 100).unwrap(), 0, "starts over");
}

#[test]
fn crash_leftovers_go_but_known_blobs_the_corrupt_folder_and_strangers_stay() {
    let (dir, log) = open();
    let objects = dir.path().join("objects");
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let (_, kept) = upload(&log, &objects, alice, b"in a library");

    let orphan = ObjectId::sha2_256("cd".repeat(32));
    std::fs::write(objects.join(orphan.to_string()), b"written, then the server crashed").unwrap();
    std::fs::write(objects.join(format!("{orphan}.thumb-small.jpg")), b"thumb").unwrap();
    std::fs::write(objects.join(".tmp-deadbeef"), b"half a write").unwrap();
    std::fs::create_dir_all(objects.join(".corrupt")).unwrap();
    std::fs::write(objects.join(".corrupt").join("x"), b"for a human").unwrap();
    std::fs::write(objects.join("README"), b"not ours").unwrap();

    let report = housekeeping::run(&log, &objects, patient()).unwrap();
    assert_eq!((report.orphan_blobs, report.temp_files), (0, 0), "too young: might be a write in progress");

    let report = housekeeping::run(&log, &objects, eager()).unwrap();
    assert_eq!((report.orphan_blobs, report.temp_files), (1, 1));
    assert!(!objects.join(orphan.to_string()).exists());
    assert!(!objects.join(format!("{orphan}.thumb-small.jpg")).exists());
    assert!(!objects.join(".tmp-deadbeef").exists());
    assert!(objects.join(kept.to_string()).exists());
    assert!(objects.join(".corrupt").join("x").exists());
    assert!(objects.join("README").exists());
}

#[test]
fn a_file_removed_everywhere_is_reclaimed_after_the_grace_period() {
    let (dir, log) = open();
    let objects = dir.path().join("objects");
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let (file_id, hash) = upload(&log, &objects, alice, b"delete me from the server");
    std::fs::write(objects.join(format!("{hash}.thumb-small.jpg")), b"thumb").unwrap();
    file::remove_from_library(&log, alice, file_id).unwrap();

    let report = housekeeping::run(&log, &objects, patient()).unwrap();
    assert_eq!((report.newly_unreferenced, report.reclaimed_blobs), (1, 0));
    assert!(objects.join(hash.to_string()).exists(), "still in its grace period");

    let report = housekeeping::run(&log, &objects, eager()).unwrap();
    assert_eq!((report.newly_unreferenced, report.reclaimed_blobs), (0, 1), "the grace period started on the first pass");
    assert!(!objects.join(hash.to_string()).exists());
    assert!(!objects.join(format!("{hash}.thumb-small.jpg")).exists());
    assert_eq!(report.freed_bytes, 25 + 5);

    // Gone on purpose: the scrub doesn't report it missing.
    assert!(scrub::due(&log, Duration::ZERO).unwrap().is_empty());
    // And a second pass has nothing left to do.
    assert_eq!(housekeeping::run(&log, &objects, eager()).unwrap(), housekeeping::Report::default());
}

#[test]
fn reclaiming_can_be_turned_off() {
    let (dir, log) = open();
    let objects = dir.path().join("objects");
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let (file_id, hash) = upload(&log, &objects, alice, b"keep forever");
    file::remove_from_library(&log, alice, file_id).unwrap();

    let report = housekeeping::run(&log, &objects, Settings { reclaim_after: None, ..eager() }).unwrap();
    assert_eq!(report.reclaimed_blobs, 0);
    assert!(objects.join(hash.to_string()).exists());
}

#[test]
fn another_library_an_album_or_album_trash_keeps_the_blob() {
    let (dir, log) = open();
    let objects = dir.path().join("objects");
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let bob = user::create_user(&log, "bob", "pw").unwrap().actor;

    // Same bytes in two libraries: one removal leaves the other.
    let (shared, shared_hash) = upload(&log, &objects, alice, b"both have it");
    upload(&log, &objects, bob, b"both have it");
    file::remove_from_library(&log, alice, shared).unwrap();

    // In an album, then trashed there: the owner can still restore it.
    let (trashed, trashed_hash) = upload(&log, &objects, alice, b"in an album's trash");
    let album_id = match album::create_album(&log, alice, "Trip", AlbumKind::User).unwrap().action {
        server::Action::CreateAlbum { album_id, .. } => album_id,
        _ => unreachable!(),
    };
    album::add_to_album(&log, alice, album_id, trashed).unwrap();
    file::remove_from_library(&log, alice, trashed).unwrap();
    album::trash_from_album(&log, alice, album_id, trashed).unwrap();

    let report = housekeeping::run(&log, &objects, eager()).unwrap();
    assert_eq!((report.newly_unreferenced, report.reclaimed_blobs), (0, 0));
    assert!(objects.join(shared_hash.to_string()).exists());
    assert!(objects.join(trashed_hash.to_string()).exists());

    // Purged from the trash: now nothing holds it.
    album::purge_from_album(&log, alice, album_id, trashed).unwrap();
    let report = housekeeping::run(&log, &objects, eager()).unwrap();
    assert_eq!(report.reclaimed_blobs, 1);
    assert!(!objects.join(trashed_hash.to_string()).exists());
}

#[test]
fn uploading_again_during_the_grace_period_keeps_the_blob() {
    let (dir, log) = open();
    let objects = dir.path().join("objects");
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let (file_id, hash) = upload(&log, &objects, alice, b"changed my mind");
    file::remove_from_library(&log, alice, file_id).unwrap();
    housekeeping::run(&log, &objects, patient()).unwrap();

    upload(&log, &objects, alice, b"changed my mind");
    let report = housekeeping::run(&log, &objects, eager()).unwrap();
    assert_eq!(report.reclaimed_blobs, 0);
    assert!(objects.join(hash.to_string()).exists());
}

#[test]
fn uploading_after_reclaiming_brings_the_bytes_back() {
    let (dir, log) = open();
    let objects = dir.path().join("objects");
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let (file_id, hash) = upload(&log, &objects, alice, b"gone, then back");
    file::remove_from_library(&log, alice, file_id).unwrap();
    housekeeping::run(&log, &objects, eager()).unwrap();
    assert!(!objects.join(hash.to_string()).exists());

    let (again, _) = upload(&log, &objects, alice, b"gone, then back");
    assert_eq!(again, file_id, "same bytes, same file");
    assert_eq!(std::fs::read(objects.join(hash.to_string())).unwrap(), b"gone, then back");
    let (bytes, _, _) = file::download_file(&log, &objects, alice, file_id, file::FileVariant::Original).unwrap();
    assert_eq!(bytes, b"gone, then back");

    // Tracked like any other blob again: verified by the upload, nothing lost.
    let report = housekeeping::run(&log, &objects, eager()).unwrap();
    assert_eq!((report.reclaimed_blobs, report.lost.len()), (0, 0));
    assert_eq!(scrub::due(&log, Duration::ZERO).unwrap().len(), 1);
}

#[test]
fn a_chunked_upload_after_reclaiming_brings_the_bytes_back() {
    let (dir, log) = open();
    let objects = dir.path().join("objects");
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let data = b"a video, in chunks";
    let (file_id, hash) = upload(&log, &objects, alice, data);
    file::remove_from_library(&log, alice, file_id).unwrap();
    housekeeping::run(&log, &objects, eager()).unwrap();

    file::begin_upload(&objects, alice, &hash, data.len() as u64).unwrap();
    file::upload_chunk(&objects, alice, &hash, 0, data).unwrap();
    file::commit_upload(&log, &objects, alice, &hash, "video/mp4".to_string(), None, file::FileMetadata::default()).unwrap();
    assert_eq!(std::fs::read(objects.join(hash.to_string())).unwrap(), data);
    assert_eq!(housekeeping::run(&log, &objects, eager()).unwrap().reclaimed_blobs, 0);
}
