use sha2::{Digest, Sha256};

use server::commands::{file, user};
use server::{EventLog, NodeId, ObjectId, ServerError};

fn open() -> (tempfile::TempDir, EventLog<server::Server>) {
    let dir = tempfile::tempdir().unwrap();
    let log = EventLog::open(dir.path().join("test.db"), NodeId("n".to_string())).unwrap();
    (dir, log)
}

fn hash_of(data: &[u8]) -> ObjectId {
    ObjectId::sha2_256(Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect::<String>())
}

/// Deterministic, non-repeating bytes: big enough to need several chunks.
fn video_bytes() -> Vec<u8> {
    (0..300_000u32).flat_map(|i| i.wrapping_mul(2_654_435_761).to_le_bytes()).collect()
}

fn file_id_of(ev: chronicler::Event<server::UserId, server::Action>) -> server::FileId {
    match ev.action {
        server::Action::UploadFile { file_id, .. } => file_id,
        _ => unreachable!(),
    }
}

#[test]
fn chunked_upload_round_trips_and_lands_in_the_library() {
    let (dir, log) = open();
    let objects = dir.path().join("objects");
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let data = video_bytes();
    let hash = hash_of(&data);

    assert_eq!(file::begin_upload(&objects, alice, &hash, data.len() as u64).unwrap(), 0);
    let mut offset = 0;
    for chunk in data.chunks(256 * 1024) {
        file::upload_chunk(&objects, alice, &hash, offset, chunk).unwrap();
        offset += chunk.len() as u64;
    }
    let ev = file::commit_upload(&log, &objects, alice, &hash, "video/mp4".to_string(), Some("clip.mp4".to_string()), file::FileMetadata::default()).unwrap();
    match &ev.action {
        server::Action::UploadFile { hash: h, size, .. } => {
            assert_eq!(h, &hash);
            assert_eq!(*size, data.len() as i64);
        }
        _ => unreachable!(),
    }
    let file_id = file_id_of(ev);

    let (bytes, mime, name) = file::download_file(&log, &objects, alice, file_id, file::FileVariant::Original).unwrap();
    assert_eq!(bytes, data);
    assert_eq!(mime, "video/mp4");
    assert_eq!(name.as_deref(), Some("clip.mp4"));
    assert_eq!(server::queries::list_library(&log, alice).unwrap().len(), 1);
    assert!(std::fs::read_dir(objects.join(".uploads")).unwrap().next().is_none(), "the partial is moved, not copied");
}

#[test]
fn begin_upload_resumes_where_the_server_left_off() {
    let (dir, log) = open();
    let objects = dir.path().join("objects");
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let data = video_bytes();
    let hash = hash_of(&data);
    let size = data.len() as u64;

    file::begin_upload(&objects, alice, &hash, size).unwrap();
    file::upload_chunk(&objects, alice, &hash, 0, &data[..100_000]).unwrap();

    // The connection drops; a new session asks where to continue.
    assert_eq!(file::begin_upload(&objects, alice, &hash, size).unwrap(), 100_000);
    file::upload_chunk(&objects, alice, &hash, 100_000, &data[100_000..]).unwrap();
    assert_eq!(file::begin_upload(&objects, alice, &hash, size).unwrap(), size, "everything arrived, only the commit is missing");

    file::commit_upload(&log, &objects, alice, &hash, "video/mp4".to_string(), None, file::FileMetadata::default()).unwrap();
}

#[test]
fn a_chunk_at_the_wrong_offset_is_rejected_with_the_right_one() {
    let (dir, log) = open();
    let objects = dir.path().join("objects");
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let data = video_bytes();
    let hash = hash_of(&data);

    file::begin_upload(&objects, alice, &hash, data.len() as u64).unwrap();
    file::upload_chunk(&objects, alice, &hash, 0, &data[..1000]).unwrap();
    match file::upload_chunk(&objects, alice, &hash, 5000, &data[5000..6000]) {
        Err(ServerError::WrongOffset { expected: 1000, got: 5000 }) => {}
        other => panic!("expected WrongOffset, got {other:?}"),
    }
}

#[test]
fn bytes_that_do_not_match_the_declared_hash_are_dropped() {
    let (dir, log) = open();
    let objects = dir.path().join("objects");
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let data = video_bytes();
    let hash = hash_of(&data);

    // The file changes on the phone mid-upload.
    let mut edited = data.clone();
    edited[123_456] ^= 0xff;
    file::begin_upload(&objects, alice, &hash, data.len() as u64).unwrap();
    file::upload_chunk(&objects, alice, &hash, 0, &edited).unwrap();

    match file::commit_upload(&log, &objects, alice, &hash, "video/mp4".to_string(), None, file::FileMetadata::default()) {
        Err(ServerError::HashMismatch { .. }) => {}
        other => panic!("expected HashMismatch, got {other:?}"),
    }
    assert!(server::queries::list_library(&log, alice).unwrap().is_empty());
    assert_eq!(file::begin_upload(&objects, alice, &hash, data.len() as u64).unwrap(), 0, "the bad partial is gone");
}

#[test]
fn a_chunked_upload_of_known_bytes_dedups_to_the_same_file() {
    let (dir, log) = open();
    let objects = dir.path().join("objects");
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let data = video_bytes();
    let hash = hash_of(&data);

    let single = file_id_of(file::upload_file(&log, &objects, alice, &data, "video/mp4".to_string(), None, file::FileMetadata::default()).unwrap());

    file::begin_upload(&objects, alice, &hash, data.len() as u64).unwrap();
    file::upload_chunk(&objects, alice, &hash, 0, &data).unwrap();
    let chunked = file_id_of(file::commit_upload(&log, &objects, alice, &hash, "video/mp4".to_string(), None, file::FileMetadata::default()).unwrap());

    assert_eq!(single, chunked);
}

#[test]
fn chunks_need_a_begun_upload_and_uploads_are_per_user() {
    let (dir, log) = open();
    let objects = dir.path().join("objects");
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let bob = user::create_user(&log, "bob", "pw").unwrap().actor;
    let data = video_bytes();
    let hash = hash_of(&data);

    assert!(matches!(file::upload_chunk(&objects, alice, &hash, 0, &data), Err(ServerError::InvalidUpload(_))));

    file::begin_upload(&objects, alice, &hash, data.len() as u64).unwrap();
    file::upload_chunk(&objects, alice, &hash, 0, &data[..50_000]).unwrap();
    // Bob uploading the same bytes starts his own upload, from zero.
    assert_eq!(file::begin_upload(&objects, bob, &hash, data.len() as u64).unwrap(), 0);
}
