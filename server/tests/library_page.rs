use server::commands::{file, user};
use server::queries;
use server::{EventLog, NodeId, ServerError, UserId};

fn open() -> (tempfile::TempDir, EventLog<server::Server>) {
    let dir = tempfile::tempdir().unwrap();
    let log = EventLog::open(dir.path().join("test.db"), NodeId("n".to_string())).unwrap();
    (dir, log)
}

fn upload(log: &EventLog<server::Server>, objects: &std::path::Path, actor: UserId, data: &[u8], taken_at: Option<i64>) -> String {
    let meta = file::FileMetadata { width: Some(4), height: Some(3), taken_at, ..Default::default() };
    match file::upload_file(log, objects, actor, data, "image/jpeg".to_string(), Some("p.jpg".to_string()), meta).unwrap().action {
        server::Action::UploadFile { file_id, .. } => file_id.to_string(),
        _ => unreachable!(),
    }
}

#[test]
fn newest_taken_first_falling_back_to_when_added() {
    let (dir, log) = open();
    let objects = dir.path().join("objects");
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;

    let old = upload(&log, &objects, alice, b"taken in 2001", Some(1_000_000_000_000));
    let undated = upload(&log, &objects, alice, b"no capture time", None); // sorts by added_at: now
    let newer = upload(&log, &objects, alice, b"taken in 2020", Some(1_600_000_000_000));

    let page = queries::list_library_page(&log, alice, None, 10).unwrap();
    let order: Vec<_> = page.files.iter().map(|f| f.file_id.clone()).collect();
    assert_eq!(order, vec![undated, newer, old]);
    assert_eq!(page.next, None);

    let first = &page.files[1];
    assert_eq!(first.taken_at, Some(1_600_000_000_000));
    assert_eq!((first.width, first.height, first.mime.as_str(), first.name.as_deref()), (Some(4), Some(3), "image/jpeg", Some("p.jpg")));
    assert!(first.hash.starts_with("sha2-256:"));
}

#[test]
fn paging_covers_everything_once_and_is_per_user() {
    let (dir, log) = open();
    let objects = dir.path().join("objects");
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    let bob = user::create_user(&log, "bob", "pw").unwrap().actor;

    let mut expected = Vec::new();
    for i in 0..25i64 {
        // Several share a taken_at, so the file_id tiebreak is exercised too.
        expected.push(upload(&log, &objects, alice, format!("photo {i}").as_bytes(), Some(1_000 + i / 3)));
    }
    upload(&log, &objects, bob, b"bob's", Some(5_000));

    let mut seen = Vec::new();
    let mut after = None;
    loop {
        let page = queries::list_library_page(&log, alice, after.as_deref(), 7).unwrap();
        assert!(page.files.len() <= 7);
        seen.extend(page.files.iter().map(|f| f.file_id.clone()));
        match page.next {
            Some(next) => after = Some(next),
            None => break,
        }
    }

    assert_eq!(seen.len(), 25, "every file exactly once, none of bob's");
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 25);
    expected.sort();
    assert_eq!(unique, expected);
}

#[test]
fn a_malformed_cursor_is_rejected() {
    let (_dir, log) = open();
    let alice = user::create_user(&log, "alice", "pw").unwrap().actor;
    assert!(matches!(queries::list_library_page(&log, alice, Some("not-a-cursor"), 10), Err(ServerError::InvalidArgument(_))));
}
