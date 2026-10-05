// The proof from eventlog.md: replaying the logged events into a fresh
// database must produce the same projection state as running the commands
// live did. Hand-written rather than proptest-driven (unlike chronicler's
// own tests/determinism_pattern.rs) because most of these actions have
// cross-row dependencies (a file must exist before it can be added to an
// album) that a naive random generator would mostly reject.

use diesel::prelude::*;
use server::commands::{album, file, user};
use server::{AlbumKind, AlbumRole, EventLog, NodeId, Server};

diesel::table! {
    album_file (album_id, file_id) {
        album_id -> Text,
        file_id -> Text,
        added_at -> BigInt,
        added_by -> Text,
        trashed_at -> Nullable<BigInt>,
        trashed_by -> Nullable<Text>,
    }
}

diesel::table! {
    album_member (album_id, user_id) {
        album_id -> Text,
        user_id -> Text,
        role -> Text,
    }
}

diesel::table! {
    app_user (user_id) {
        user_id -> Text,
        username -> Text,
        is_admin -> Bool,
        status -> Text,
    }
}

type Snapshot = (Vec<(String, String, bool)>, Vec<(String, String, String)>, Vec<(String, bool, String)>);

fn snapshot(log: &EventLog<Server>) -> Snapshot {
    let mut conn = log.connection().unwrap();
    let files: Vec<(String, String, bool)> = album_file::table
        .select((album_file::album_id, album_file::file_id, album_file::trashed_at.is_not_null()))
        .order((album_file::album_id.asc(), album_file::file_id.asc()))
        .load(&mut conn)
        .unwrap();
    let members: Vec<(String, String, String)> = album_member::table
        .select((album_member::album_id, album_member::user_id, album_member::role))
        .order((album_member::album_id.asc(), album_member::user_id.asc()))
        .load(&mut conn)
        .unwrap();
    let users: Vec<(String, bool, String)> =
        app_user::table.select((app_user::username, app_user::is_admin, app_user::status)).order(app_user::username.asc()).load(&mut conn).unwrap();
    (files, members, users)
}

#[test]
fn replay_matches_live_state() {
    let dir = tempfile::tempdir().unwrap();
    let node = NodeId("n".to_string());

    let live = EventLog::<Server>::open(dir.path().join("live.db"), node.clone()).unwrap();
    let objects_dir = dir.path().join("objects");

    // Exercises the bootstrap admin (is_admin: true, status: Active from
    // the start) and ApproveUser (a Pending account flipped to Active) —
    // both must survive replay exactly like every other action.
    let admin = user::ensure_bootstrap_admin(&live).unwrap().unwrap().actor;
    let alice = user::create_user(&live, "alice", "hunter2").unwrap().actor;
    user::approve_user(&live, admin, alice).unwrap();
    let bob = user::create_user(&live, "bob", "swordfish").unwrap().actor;
    user::approve_user(&live, admin, bob).unwrap();
    // A third account, left pending — proves "never approved" (not just
    // "approved") survives replay too.
    user::create_user(&live, "carol", "pw").unwrap();

    let file_ev = file::upload_file(&live, &objects_dir, alice, b"aaaa", "image/jpeg".to_string(), None, file::FileMetadata::default()).unwrap();
    let file_id = match file_ev.action {
        server::Action::UploadFile { file_id, .. } => file_id,
        _ => unreachable!(),
    };

    let album_ev = album::create_album(&live, alice, "Vacation", AlbumKind::User).unwrap();
    let album_id = match album_ev.action {
        server::Action::CreateAlbum { album_id, .. } => album_id,
        _ => unreachable!(),
    };

    album::add_to_album(&live, alice, album_id, file_id).unwrap();
    album::share_album(&live, alice, album_id, bob, AlbumRole::Writer).unwrap();
    // Re-sharing with a different role upserts rather than erroring.
    album::share_album(&live, alice, album_id, bob, AlbumRole::Reader).unwrap();

    // A second upload of the exact same bytes: dedups onto the same file_id.
    let dedup_ev = file::upload_file(&live, &objects_dir, bob, b"aaaa", "image/jpeg".to_string(), None, file::FileMetadata::default()).unwrap();
    assert_eq!(match dedup_ev.action { server::Action::UploadFile { file_id, .. } => file_id, _ => unreachable!() }, file_id);

    // A second file: added, trashed, then purged — the resulting row must
    // be fully absent both live and after replay, not merely "never
    // inserted".
    let file2_ev = file::upload_file(&live, &objects_dir, alice, b"bbbb", "image/png".to_string(), None, file::FileMetadata::default()).unwrap();
    let file2_id = match file2_ev.action { server::Action::UploadFile { file_id, .. } => file_id, _ => unreachable!() };
    album::add_to_album(&live, alice, album_id, file2_id).unwrap();
    album::trash_from_album(&live, alice, album_id, file2_id).unwrap();
    album::purge_from_album(&live, alice, album_id, file2_id).unwrap();

    // A third file: added, trashed, restored — must end up present and
    // *not* trashed, proving restore isn't just "leave it alone".
    let file3_ev = file::upload_file(&live, &objects_dir, alice, b"cccc", "image/png".to_string(), None, file::FileMetadata::default()).unwrap();
    let file3_id = match file3_ev.action { server::Action::UploadFile { file_id, .. } => file_id, _ => unreachable!() };
    album::add_to_album(&live, alice, album_id, file3_id).unwrap();
    album::trash_from_album(&live, alice, album_id, file3_id).unwrap();
    album::restore_from_trash(&live, alice, album_id, file3_id).unwrap();

    // A fourth file: added and trashed, left trashed — proves the trashed
    // *state itself* (not just presence/absence) survives replay.
    let file4_ev = file::upload_file(&live, &objects_dir, alice, b"dddd", "image/png".to_string(), None, file::FileMetadata::default()).unwrap();
    let file4_id = match file4_ev.action { server::Action::UploadFile { file_id, .. } => file_id, _ => unreachable!() };
    album::add_to_album(&live, alice, album_id, file4_id).unwrap();
    album::trash_from_album(&live, alice, album_id, file4_id).unwrap();

    // A second album, filled and then deleted: its rows must be gone both
    // live and after replay, while its file stays in alice's library.
    let doomed = match album::create_album(&live, alice, "Doomed", AlbumKind::User).unwrap().action {
        server::Action::CreateAlbum { album_id, .. } => album_id,
        _ => unreachable!(),
    };
    album::add_to_album(&live, alice, doomed, file3_id).unwrap();
    album::share_album(&live, alice, doomed, bob, AlbumRole::Reader).unwrap();
    album::delete_album(&live, alice, doomed).unwrap();

    // Bob drops the deduped file from his own library: alice keeps it.
    file::remove_from_library(&live, bob, file_id).unwrap();

    let live_state = snapshot(&live);

    // Rebuild a fresh database purely from the raw stored events, never by
    // re-invoking the commands (which would run against state the first
    // pass already mutated).
    let raw = chronicler::read_raw_events(dir.path().join("live.db")).unwrap();
    let replayed = EventLog::<Server>::open(dir.path().join("replayed.db"), node).unwrap();
    for row in raw {
        let action: server::Action = serde_json::from_str(&row.payload).unwrap();
        let actor: server::UserId = row.actor.parse().unwrap();
        replayed
            .ingest(server::Event {
                node_id: chronicler::NodeId(row.node_id),
                seq: row.seq,
                actor,
                at: row.at,
                action,
            })
            .unwrap();
    }

    assert_eq!(live_state, snapshot(&replayed));
}
