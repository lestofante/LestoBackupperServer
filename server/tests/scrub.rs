use std::time::{Duration, Instant};

use server::commands::{file, user};
use server::scrub::{self, BlobStatus, Throttle};
use server::{EventLog, NodeId, ObjectId, UserId};

const WEEK: Duration = Duration::from_secs(7 * 24 * 3600);
const UNLIMITED: u64 = u64::MAX;

fn open() -> (tempfile::TempDir, EventLog<server::Server>) {
    let dir = tempfile::tempdir().unwrap();
    let log = EventLog::open(dir.path().join("test.db"), NodeId("n".to_string())).unwrap();
    (dir, log)
}

fn upload(log: &EventLog<server::Server>, objects: &std::path::Path, actor: UserId, data: &[u8]) -> ObjectId {
    match file::upload_file(log, objects, actor, data, "image/jpeg".to_string(), None, file::FileMetadata::default()).unwrap().action {
        server::Action::UploadFile { hash, .. } => hash,
        _ => unreachable!(),
    }
}

#[test]
fn an_upload_counts_as_verified_until_the_interval_passes() {
    let (dir, log) = open();
    let objects = dir.path().join("objects");
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let hash = upload(&log, &objects, alice, b"fresh photo");

    assert!(scrub::due(&log, WEEK).unwrap().is_empty(), "just uploaded: not due for a week");
    let due = scrub::due(&log, Duration::ZERO).unwrap();
    assert_eq!(due, vec![scrub::DueBlob { hash, size: 11 }]);
}

#[test]
fn a_healthy_blob_passes_and_is_not_due_again() {
    let (dir, log) = open();
    let objects = dir.path().join("objects");
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    upload(&log, &objects, alice, b"healthy");

    let mut throttle = Throttle::new(UNLIMITED);
    for blob in scrub::due(&log, Duration::ZERO).unwrap() {
        assert!(scrub::verify_and_record(&log, &objects, &blob, &mut throttle).unwrap().is_none());
    }
    assert!(scrub::due(&log, WEEK).unwrap().is_empty());
}

#[test]
fn a_corrupt_blob_is_quarantined_reported_missing_and_healed_by_reupload() {
    let (dir, log) = open();
    let objects = dir.path().join("objects");
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let data = b"precious photo bytes";
    let hash = upload(&log, &objects, alice, data);

    // Bit rot: same length, one byte flipped.
    let mut rotten = data.to_vec();
    rotten[3] ^= 0x01;
    std::fs::write(objects.join(hash.to_string()), &rotten).unwrap();

    let blob = scrub::due(&log, Duration::ZERO).unwrap().remove(0);
    let problem = scrub::verify_and_record(&log, &objects, &blob, &mut Throttle::new(UNLIMITED)).unwrap().expect("corruption is reported");
    assert_eq!(problem.status, BlobStatus::Corrupt);
    assert!(!objects.join(hash.to_string()).exists(), "moved out of the way");
    let quarantined: Vec<_> = std::fs::read_dir(objects.join(".corrupt")).unwrap().collect();
    assert_eq!(quarantined.len(), 1, "kept for a human to inspect, not deleted");

    // The library now reports it missing, so a phone that has it re-uploads it...
    let lookup = server::queries::find_library_files_by_hash(&log, &objects, alice, std::slice::from_ref(&hash)).unwrap();
    assert!(lookup.matches.is_empty());
    assert_eq!(lookup.missing_blobs, vec![hash.clone()]);

    // ...which restores the blob and resets the check.
    upload(&log, &objects, alice, data);
    assert_eq!(std::fs::read(objects.join(hash.to_string())).unwrap(), data);
    assert!(scrub::due(&log, WEEK).unwrap().is_empty());
}

#[test]
fn a_missing_blob_is_reported() {
    let (dir, log) = open();
    let objects = dir.path().join("objects");
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let hash = upload(&log, &objects, alice, b"about to vanish");
    std::fs::remove_file(objects.join(hash.to_string())).unwrap();

    let blob = scrub::due(&log, Duration::ZERO).unwrap().remove(0);
    let problem = scrub::verify_and_record(&log, &objects, &blob, &mut Throttle::new(UNLIMITED)).unwrap().unwrap();
    assert_eq!(problem.status, BlobStatus::Missing);
    assert!(scrub::due(&log, WEEK).unwrap().is_empty(), "reported once per interval, not every hour");
}

#[test]
fn reading_is_capped_by_the_throttle() {
    let (dir, log) = open();
    let objects = dir.path().join("objects");
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    upload(&log, &objects, alice, &vec![7u8; 2 * 1024 * 1024]);

    // 2 MiB at 4 MiB/s: at least ~half a second, however fast the disk is.
    let started = Instant::now();
    let blob = scrub::due(&log, Duration::ZERO).unwrap().remove(0);
    assert!(scrub::verify_and_record(&log, &objects, &blob, &mut Throttle::new(4 * 1024 * 1024)).unwrap().is_none());
    assert!(started.elapsed() >= Duration::from_millis(450), "took {:?}", started.elapsed());
}
