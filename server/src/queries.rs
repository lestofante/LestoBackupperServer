// Read-only, permission-checked lookups against the projection tables.
// Distinct from `commands`: nothing here appends an event — these never
// change state, only report it.

use std::collections::HashMap;
use std::path::Path;

use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;
use serde::Serialize;

use crate::action::{AccountStatus, AlbumKind, AlbumRole};
use crate::blob;
use crate::commands::album::require_role;
use crate::commands::user::is_admin;
use crate::error::{Result, ServerError};
use crate::ids::{AlbumId, FileId, UserId};
use crate::object_id::ObjectId;
use crate::projection::Server;
use crate::schema::{album, album_file, album_member, app_user, file, file_hash, user_file};
use chronicler::EventLog;

#[derive(Debug, Clone, Serialize)]
pub struct AlbumSummary {
    pub album_id: AlbumId,
    pub name: String,
    pub kind: AlbumKind,
    pub role: AlbumRole,
    /// Active (non-trashed) files.
    pub file_count: i64,
    /// The newest active file by capture time, to show as the album's cover.
    pub cover_file_id: Option<FileId>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PendingUser {
    pub user_id: UserId,
    pub username: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AlbumMember {
    pub user_id: UserId,
    pub username: String,
    pub role: AlbumRole,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrashedFile {
    pub file_id: FileId,
    pub trashed_at: i64,
    pub trashed_by: UserId,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileSummary {
    pub file_id: FileId,
    /// Absent for a file that's never been named (no filename came with
    /// the upload, and nobody's renamed it since).
    pub name: Option<String>,
}

/// Every album `actor` is a member of, with their role on each — excluding
/// ones they've hidden from their own list (`commands::album::hide_album`).
/// Use `list_hidden_albums` to see (and unhide) those.
pub fn list_my_albums(log: &EventLog<Server>, actor: UserId) -> Result<Vec<AlbumSummary>> {
    let mut conn = log.connection()?;
    let rows: Vec<(String, String, String, String)> = album_member::table
        .inner_join(album::table.on(album::album_id.eq(album_member::album_id)))
        .filter(album_member::user_id.eq(actor.to_string()))
        .filter(album_member::hidden.eq(false))
        .select((album::album_id, album::name, album::kind, album_member::role))
        .load(&mut conn)?;

    rows.into_iter().map(|row| row_to_album_summary(&mut conn, row)).collect()
}

/// The albums `actor` has hidden from their own list — the only way back
/// to `unhide_album`, since a hidden album no longer shows up anywhere
/// else in their UI.
pub fn list_hidden_albums(log: &EventLog<Server>, actor: UserId) -> Result<Vec<AlbumSummary>> {
    let mut conn = log.connection()?;
    let rows: Vec<(String, String, String, String)> = album_member::table
        .inner_join(album::table.on(album::album_id.eq(album_member::album_id)))
        .filter(album_member::user_id.eq(actor.to_string()))
        .filter(album_member::hidden.eq(true))
        .select((album::album_id, album::name, album::kind, album_member::role))
        .load(&mut conn)?;

    rows.into_iter().map(|row| row_to_album_summary(&mut conn, row)).collect()
}

fn row_to_album_summary(conn: &mut SqliteConnection, (album_id, name, kind, role): (String, String, String, String)) -> Result<AlbumSummary> {
    let (file_count, cover_file_id) = album_stats(conn, &album_id)?;
    Ok(AlbumSummary {
        album_id: album_id.parse().expect("album_id column always holds a valid AlbumId"),
        name,
        kind: AlbumKind::parse(&kind).expect("kind column always holds a valid AlbumKind"),
        role: AlbumRole::parse(&role).expect("role column always holds a valid AlbumRole"),
        file_count,
        cover_file_id,
    })
}

#[derive(QueryableByName)]
struct CoverRow {
    #[diesel(sql_type = diesel::sql_types::Text)]
    file_id: String,
}

/// How many active files an album has, and its cover: the newest by
/// capture time (falling back to when it was added), as listings sort.
fn album_stats(conn: &mut SqliteConnection, album_id: &str) -> Result<(i64, Option<FileId>)> {
    let count: i64 = album_file::table
        .filter(album_file::album_id.eq(album_id))
        .filter(album_file::trashed_at.is_null())
        .count()
        .get_result(conn)?;
    let cover: Option<CoverRow> = diesel::sql_query(
        "SELECT af.file_id FROM album_file af JOIN file f ON f.id = af.file_id
         WHERE af.album_id = ? AND af.trashed_at IS NULL
         ORDER BY COALESCE(f.taken_at, af.added_at) DESC, f.id DESC LIMIT 1",
    )
    .bind::<diesel::sql_types::Text, _>(album_id)
    .get_result(conn)
    .optional()?;
    Ok((count, cover.map(|c| c.file_id.parse().expect("file_id column always holds a valid FileId"))))
}

/// Active (non-trashed) files in an album. Requires at least Reader —
/// membership is what gates visibility, same as every other album read.
pub fn list_album_files(log: &EventLog<Server>, actor: UserId, album_id: AlbumId) -> Result<Vec<FileSummary>> {
    let mut conn = log.connection()?;
    require_role(&mut conn, album_id, actor, AlbumRole::Reader)?;

    let rows: Vec<(String, Option<String>)> = album_file::table
        .inner_join(file::table.on(file::id.eq(album_file::file_id)))
        .filter(album_file::album_id.eq(album_id.to_string()))
        .filter(album_file::trashed_at.is_null())
        .select((album_file::file_id, file::name))
        .load(&mut conn)?;

    Ok(rows.into_iter().map(row_to_file_summary).collect())
}

/// Which of `actor`'s albums each of `file_ids` currently sits in
/// (non-trashed) — one batched query rather than one per file, since a
/// grid showing this for every tile at once (Library, say) would
/// otherwise mean one round-trip per photo. A file with no album
/// membership at all simply has no entry in the result, rather than an
/// entry with an empty `Vec` — same "absence means none" convention as
/// `FileSummary::name`.
pub fn list_albums_for_files(log: &EventLog<Server>, actor: UserId, file_ids: &[FileId]) -> Result<HashMap<FileId, Vec<AlbumSummary>>> {
    let mut conn = log.connection()?;
    let id_strings: Vec<String> = file_ids.iter().map(|id| id.to_string()).collect();

    let rows: Vec<(String, String, String, String, String)> = album_file::table
        .inner_join(album_member::table.on(album_member::album_id.eq(album_file::album_id)))
        .inner_join(album::table.on(album::album_id.eq(album_file::album_id)))
        .filter(album_member::user_id.eq(actor.to_string()))
        .filter(album_file::file_id.eq_any(&id_strings))
        .filter(album_file::trashed_at.is_null())
        .select((album_file::file_id, album::album_id, album::name, album::kind, album_member::role))
        .load(&mut conn)?;

    let mut result: HashMap<FileId, Vec<AlbumSummary>> = HashMap::new();
    for (file_id, album_id, name, kind, role) in rows {
        let file_id: FileId = file_id.parse().expect("file_id column always holds a valid FileId");
        let summary = row_to_album_summary(&mut conn, (album_id, name, kind, role))?;
        result.entry(file_id).or_default().push(summary);
    }
    Ok(result)
}

/// `actor`'s own library ("All Photos") — always self-scoped, so there's no
/// target user_id to check: the query itself is the permission boundary.
pub fn list_library(log: &EventLog<Server>, actor: UserId) -> Result<Vec<FileSummary>> {
    let mut conn = log.connection()?;
    let rows: Vec<(String, Option<String>)> = user_file::table
        .inner_join(file::table.on(file::id.eq(user_file::file_id)))
        .filter(user_file::user_id.eq(actor.to_string()))
        .select((user_file::file_id, file::name))
        .load(&mut conn)?;

    Ok(rows.into_iter().map(row_to_file_summary).collect())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HashMatch {
    pub hash: ObjectId,
    pub file_id: FileId,
}

/// `find_library_files_by_hash`'s answer. `missing_blobs` are in the
/// caller's library but their bytes are gone from disk: they're reported as
/// absent (not in `matches`) so a client that still has the file uploads it
/// again, and `upload_file` rewrites the blob — the store heals from any
/// device that kept a copy. Callers should still surface them: it's data loss.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HashLookup {
    pub matches: Vec<HashMatch>,
    pub missing_blobs: Vec<ObjectId>,
}

/// Well under SQLite's bound-parameter limit, whatever it was compiled
/// with — a phone asking about its whole camera roll in one request just
/// costs a few more statements.
const HASH_LOOKUP_CHUNK: usize = 500;

/// Which of `hashes` are already in `actor`'s own library, and under which
/// `file_id` — lets a backup client skip re-sending bytes it already
/// uploaded (a re-upload would dedup on disk, but still append another
/// `UploadFile` event), and rebuild its local "what's backed up" state from
/// the server alone after losing it.
///
/// Scoped to the caller's library on purpose: a hash that exists on the
/// server only in someone else's library is reported exactly like one that
/// doesn't exist at all, so this can't be used to learn that another user
/// holds those exact bytes (server.md's reason `file` has no owner column).
/// Unknown hashes are simply absent from the result, not an error.
pub fn find_library_files_by_hash(log: &EventLog<Server>, objects_dir: &Path, actor: UserId, hashes: &[ObjectId]) -> Result<HashLookup> {
    let mut conn = log.connection()?;

    let mut by_algo: HashMap<&str, Vec<&str>> = HashMap::new();
    for hash in hashes {
        by_algo.entry(hash.algo()).or_default().push(hash.digest());
    }

    let mut result = HashLookup::default();
    for (algo, digests) in by_algo {
        for chunk in digests.chunks(HASH_LOOKUP_CHUNK) {
            let rows: Vec<(String, String)> = file_hash::table
                .inner_join(user_file::table.on(user_file::file_id.eq(file_hash::file_id)))
                .filter(user_file::user_id.eq(actor.to_string()))
                .filter(file_hash::algo.eq(algo))
                .filter(file_hash::digest.eq_any(chunk))
                .select((file_hash::digest, file_hash::file_id))
                .load(&mut conn)?;

            for (digest, file_id) in rows {
                let hash = ObjectId::new(algo, digest);
                if !blob::exists(objects_dir, &hash) {
                    result.missing_blobs.push(hash);
                    continue;
                }
                result.matches.push(HashMatch { hash, file_id: file_id.parse().expect("file_id column always holds a valid FileId") });
            }
        }
    }
    Ok(result)
}

fn row_to_file_summary((file_id, name): (String, Option<String>)) -> FileSummary {
    FileSummary { file_id: file_id.parse().expect("file_id column always holds a valid FileId"), name }
}

/// One file in a library page: what a gallery needs to show and sort it,
/// plus the hash, so a device can tell which of its own files these are.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, QueryableByName)]
pub struct LibraryFile {
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub file_id: String,
    /// Tagged, "sha2-256:<hex>".
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub hash: String,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    pub name: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub mime: String,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    pub size: i64,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Integer>)]
    pub width: Option<i32>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Integer>)]
    pub height: Option<i32>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Integer>)]
    pub duration: Option<i32>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::BigInt>)]
    pub taken_at: Option<i64>,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    pub added_at: i64,
    /// The sort key: `taken_at`, or `added_at` when the capture time is unknown.
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    pub sort_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LibraryPage {
    pub files: Vec<LibraryFile>,
    /// Pass back as `after` for the next page; None on the last one.
    pub next: Option<String>,
}

pub const MAX_LIBRARY_PAGE: u32 = 1000;

/// `actor`'s library, newest first by when each file was taken (falling
/// back to when it was added), `limit` files at a time. Keyset-paged: `after`
/// is the previous page's `next`, naming the last file seen rather than an
/// offset, so files added while someone scrolls don't shift or repeat pages.
pub fn list_library_page(log: &EventLog<Server>, actor: UserId, after: Option<&str>, limit: u32) -> Result<LibraryPage> {
    let limit = limit.clamp(1, MAX_LIBRARY_PAGE);
    let cursor = after.map(parse_library_cursor).transpose()?;
    let has_cursor = cursor.is_some();
    let (after_at, after_id) = cursor.unwrap_or((i64::MAX, String::new()));

    let mut conn = log.connection()?;
    let mut files: Vec<LibraryFile> = diesel::sql_query(
        "SELECT f.id AS file_id, h.algo || ':' || h.digest AS hash, f.name, f.mime, f.size,
                f.width, f.height, f.duration, f.taken_at, uf.added_at,
                COALESCE(f.taken_at, uf.added_at) AS sort_at
         FROM user_file uf
         JOIN file f ON f.id = uf.file_id
         JOIN file_hash h ON h.file_id = f.id AND h.algo = 'sha2-256'
         WHERE uf.user_id = ?
           AND (COALESCE(f.taken_at, uf.added_at) < ?
                OR (COALESCE(f.taken_at, uf.added_at) = ? AND f.id < ?)
                OR ? = 0)
         ORDER BY sort_at DESC, f.id DESC
         LIMIT ?",
    )
    .bind::<diesel::sql_types::Text, _>(actor.to_string())
    .bind::<diesel::sql_types::BigInt, _>(after_at)
    .bind::<diesel::sql_types::BigInt, _>(after_at)
    .bind::<diesel::sql_types::Text, _>(&after_id)
    .bind::<diesel::sql_types::Integer, _>(i32::from(has_cursor))
    .bind::<diesel::sql_types::BigInt, _>(i64::from(limit) + 1)
    .load(&mut conn)?;

    // One extra row tells whether another page exists without a COUNT.
    let next = if files.len() > limit as usize {
        files.truncate(limit as usize);
        files.last().map(|f| format!("{}:{}", f.sort_at, f.file_id))
    } else {
        None
    };
    Ok(LibraryPage { files, next })
}

fn parse_library_cursor(cursor: &str) -> Result<(i64, String)> {
    let invalid = || ServerError::InvalidArgument(format!("invalid library page cursor {cursor:?}"));
    let (at, id) = cursor.split_once(':').ok_or_else(invalid)?;
    Ok((at.parse().map_err(|_| invalid())?, id.to_string()))
}

/// An album's active files, newest first by capture time (falling back to
/// when each was added to the album), paged like `list_library_page`.
/// Requires at least Reader. `added_at` is when the file joined the album.
pub fn list_album_page(log: &EventLog<Server>, actor: UserId, album_id: AlbumId, after: Option<&str>, limit: u32) -> Result<LibraryPage> {
    let limit = limit.clamp(1, MAX_LIBRARY_PAGE);
    let cursor = after.map(parse_library_cursor).transpose()?;
    let has_cursor = cursor.is_some();
    let (after_at, after_id) = cursor.unwrap_or((i64::MAX, String::new()));

    let mut conn = log.connection()?;
    require_role(&mut conn, album_id, actor, AlbumRole::Reader)?;
    let mut files: Vec<LibraryFile> = diesel::sql_query(
        "SELECT f.id AS file_id, h.algo || ':' || h.digest AS hash, f.name, f.mime, f.size,
                f.width, f.height, f.duration, f.taken_at, af.added_at,
                COALESCE(f.taken_at, af.added_at) AS sort_at
         FROM album_file af
         JOIN file f ON f.id = af.file_id
         JOIN file_hash h ON h.file_id = f.id AND h.algo = 'sha2-256'
         WHERE af.album_id = ? AND af.trashed_at IS NULL
           AND (COALESCE(f.taken_at, af.added_at) < ?
                OR (COALESCE(f.taken_at, af.added_at) = ? AND f.id < ?)
                OR ? = 0)
         ORDER BY sort_at DESC, f.id DESC
         LIMIT ?",
    )
    .bind::<diesel::sql_types::Text, _>(album_id.to_string())
    .bind::<diesel::sql_types::BigInt, _>(after_at)
    .bind::<diesel::sql_types::BigInt, _>(after_at)
    .bind::<diesel::sql_types::Text, _>(&after_id)
    .bind::<diesel::sql_types::Integer, _>(i32::from(has_cursor))
    .bind::<diesel::sql_types::BigInt, _>(i64::from(limit) + 1)
    .load(&mut conn)?;

    let next = if files.len() > limit as usize {
        files.truncate(limit as usize);
        files.last().map(|f| format!("{}:{}", f.sort_at, f.file_id))
    } else {
        None
    };
    Ok(LibraryPage { files, next })
}

/// Who the album is shared with, owners first, then by username. Any member
/// can see this: it's who sees what they add.
pub fn list_album_members(log: &EventLog<Server>, actor: UserId, album_id: AlbumId) -> Result<Vec<AlbumMember>> {
    let mut conn = log.connection()?;
    require_role(&mut conn, album_id, actor, AlbumRole::Reader)?;
    let rows: Vec<(String, String, String)> = album_member::table
        .inner_join(app_user::table.on(app_user::user_id.eq(album_member::user_id)))
        .filter(album_member::album_id.eq(album_id.to_string()))
        .select((album_member::user_id, app_user::username, album_member::role))
        .load(&mut conn)?;

    let mut members: Vec<AlbumMember> = rows
        .into_iter()
        .filter_map(|(user_id, username, role)| {
            Some(AlbumMember { user_id: user_id.parse().expect("user_id column always holds a valid UserId"), username, role: AlbumRole::parse(&role)? })
        })
        .collect();
    members.sort_by(|a, b| b.role.cmp(&a.role).then_with(|| a.username.cmp(&b.username)));
    Ok(members)
}

/// Admin-only: the accounts waiting on `commands::user::approve_user`.
pub fn list_pending_users(log: &EventLog<Server>, actor: UserId) -> Result<Vec<PendingUser>> {
    if !is_admin(log, actor)? {
        return Err(ServerError::AdminRequired);
    }

    let mut conn = log.connection()?;
    let rows: Vec<(String, String, i64)> = app_user::table
        .filter(app_user::status.eq(AccountStatus::Pending.as_str()))
        .select((app_user::user_id, app_user::username, app_user::created_at))
        .load(&mut conn)?;

    Ok(rows
        .into_iter()
        .map(|(user_id, username, created_at)| PendingUser { user_id: user_id.parse().expect("user_id column always holds a valid UserId"), username, created_at })
        .collect())
}

/// The trash bin — owner-only, per the trash/restore/purge design: a writer
/// can hide a file, but only the owner can see the hidden pile and decide
/// whether to restore or purge it.
pub fn list_trash(log: &EventLog<Server>, actor: UserId, album_id: AlbumId) -> Result<Vec<TrashedFile>> {
    let mut conn = log.connection()?;
    require_role(&mut conn, album_id, actor, AlbumRole::Owner)?;

    let rows: Vec<(String, Option<i64>, Option<String>)> = album_file::table
        .filter(album_file::album_id.eq(album_id.to_string()))
        .filter(album_file::trashed_at.is_not_null())
        .select((album_file::file_id, album_file::trashed_at, album_file::trashed_by))
        .load(&mut conn)?;

    Ok(rows
        .into_iter()
        .map(|(file_id, trashed_at, trashed_by)| TrashedFile {
            file_id: file_id.parse().expect("file_id column always holds a valid FileId"),
            trashed_at: trashed_at.expect("filtered to trashed_at IS NOT NULL"),
            trashed_by: trashed_by.expect("trashed_at set implies trashed_by is too").parse().expect("trashed_by column always holds a valid UserId"),
        })
        .collect())
}
