// Disk housekeeping for objects_dir: the "separate, optional pass" that
// server.md leaves out of the request path. Four kinds of leftovers:
//
// - partial chunked uploads nobody resumed (`.uploads/`), after
//   `Settings::upload_expiry`;
// - temp files from a write that crashed halfway (`.tmp-*`);
// - orphan blobs: bytes written by an upload that crashed before its event
//   was appended (server.md: "a crash between 2 and 3 leaves an orphan
//   blob"), so no `file_hash` row names them;
// - unreferenced blobs: files that every library and album dropped
//   (server.md's "Blob lifetime"). Album trash still counts as a reference,
//   since the owner can restore from it. These are reclaimed only after
//   staying unreferenced for `Settings::reclaim_after`: until then the bytes
//   are still there, and uploading the same file again ends the wait.
//
// Never touches `.corrupt/` (kept for a human) or names it doesn't know.
//
// Uploads hold `upload_guard()` from writing the blob until the event is
// appended; deleting a blob takes the same lock exclusively and re-checks
// the database under it, so a blob can't be deleted between an upload
// writing it and the upload's event referencing it.

use std::fs;
use std::io;
use std::path::Path;
use std::sync::{RwLock, RwLockReadGuard};
use std::time::{Duration, UNIX_EPOCH};

use chronicler::EventLog;
use diesel::dsl::exists;
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;

use crate::blob;
use crate::error::Result;
use crate::object_id::ObjectId;
use crate::projection::Server;
use crate::schema::{blob_gc, file_hash};
use crate::time::now_ms;

static UPLOAD_LOCK: RwLock<()> = RwLock::new(());

/// Held by an upload from writing its blob until its event is appended.
pub(crate) fn upload_guard() -> RwLockReadGuard<'static, ()> {
    UPLOAD_LOCK.read().unwrap_or_else(|e| e.into_inner())
}

/// Leftover temp files and orphan blobs are only touched once they're this
/// old: anything younger might belong to a write still in progress.
const LEFTOVER_MIN_AGE: Duration = Duration::from_secs(24 * 3600);

#[derive(Debug, Clone, Copy)]
pub struct Settings {
    /// A partial upload not written to for this long is deleted.
    pub upload_expiry: Duration,
    /// A blob unreferenced for this long is deleted. `None`: never.
    pub reclaim_after: Option<Duration>,
    /// Minimum age of temp files and orphan blobs (tests set it to zero).
    pub leftover_min_age: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            upload_expiry: Duration::from_secs(7 * 24 * 3600),
            reclaim_after: Some(Duration::from_secs(30 * 24 * 3600)),
            leftover_min_age: LEFTOVER_MIN_AGE,
        }
    }
}

/// What one pass removed, for the log.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub expired_uploads: usize,
    pub temp_files: usize,
    pub orphan_blobs: usize,
    /// Blobs that became unreferenced since the last pass (grace period starts).
    pub newly_unreferenced: usize,
    pub reclaimed_blobs: usize,
    /// Every byte freed, thumbnails included.
    pub freed_bytes: u64,
    /// Reclaimed blobs found referenced again: their bytes are gone until a
    /// device uploads them (the scrub reports them missing). Shouldn't happen.
    pub lost: Vec<ObjectId>,
}

pub fn run(log: &EventLog<Server>, objects_dir: &Path, settings: Settings) -> Result<Report> {
    let mut report = Report::default();
    expire_uploads(objects_dir, settings.upload_expiry, &mut report)?;
    remove_leftovers(log, objects_dir, settings.leftover_min_age, &mut report)?;
    track_unreferenced(log, &mut report)?;
    if let Some(grace) = settings.reclaim_after {
        reclaim(log, objects_dir, grace, &mut report)?;
    }
    Ok(report)
}

/// Called when an upload (re)writes a blob: it's referenced again, so any
/// grace period or reclaimed state is over.
pub(crate) fn mark_present(conn: &mut SqliteConnection, hash: &ObjectId) -> Result<()> {
    diesel::delete(blob_gc::table.filter(blob_gc::algo.eq(hash.algo())).filter(blob_gc::digest.eq(hash.digest()))).execute(conn)?;
    Ok(())
}

fn age(meta: &fs::Metadata) -> Duration {
    let modified_ms = meta.modified().ok().and_then(|m| m.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64);
    modified_ms.map(|m| Duration::from_millis(u64::try_from(now_ms() - m).unwrap_or(0))).unwrap_or(Duration::ZERO)
}

/// Removes a file, counting its size; one that's already gone is fine.
fn remove(path: &Path, report_bytes: &mut u64) -> io::Result<bool> {
    let size = match fs::metadata(path) {
        Ok(m) => m.len(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    match fs::remove_file(path) {
        Ok(()) => {
            *report_bytes += size;
            Ok(true)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

fn expire_uploads(objects_dir: &Path, expiry: Duration, report: &mut Report) -> Result<()> {
    let entries = match fs::read_dir(objects_dir.join(".uploads")) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let entry = entry?;
        let meta = entry.metadata()?;
        if meta.is_file() && age(&meta) >= expiry && remove(&entry.path(), &mut report.freed_bytes)? {
            report.expired_uploads += 1;
        }
    }
    Ok(())
}

/// What a name in objects_dir is: a blob, a cached thumbnail of one, or a temp file.
enum Entry {
    Blob(ObjectId),
    Thumbnail(ObjectId),
    Temp,
}

fn classify(name: &str) -> Option<Entry> {
    if name.starts_with(".tmp-") {
        return Some(Entry::Temp);
    }
    if name.starts_with('.') {
        return None; // .uploads, .corrupt
    }
    let (hash, thumbnail) = match name.split_once(".thumb-") {
        Some((hash, _)) => (hash, true),
        None => (name, false),
    };
    let hash: ObjectId = hash.parse().ok()?;
    Some(if thumbnail { Entry::Thumbnail(hash) } else { Entry::Blob(hash) })
}

fn known(conn: &mut SqliteConnection, hash: &ObjectId) -> Result<bool> {
    Ok(diesel::select(exists(file_hash::table.filter(file_hash::algo.eq(hash.algo())).filter(file_hash::digest.eq(hash.digest())))).get_result(conn)?)
}

/// Temp files, orphan blobs, and thumbnails whose blob is unknown.
fn remove_leftovers(log: &EventLog<Server>, objects_dir: &Path, min_age: Duration, report: &mut Report) -> Result<()> {
    let entries = match fs::read_dir(objects_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let entry = entry?;
        let meta = entry.metadata()?;
        if !meta.is_file() || age(&meta) < min_age {
            continue;
        }
        let (hash, is_blob) = match classify(&entry.file_name().to_string_lossy()) {
            None => continue,
            Some(Entry::Temp) => {
                if remove(&entry.path(), &mut report.freed_bytes)? {
                    report.temp_files += 1;
                }
                continue;
            }
            Some(Entry::Blob(hash)) => (hash, true),
            Some(Entry::Thumbnail(hash)) => (hash, false),
        };
        let _exclusive = UPLOAD_LOCK.write().unwrap_or_else(|e| e.into_inner());
        let mut conn = log.connection()?;
        if !known(&mut conn, &hash)? && remove(&entry.path(), &mut report.freed_bytes)? && is_blob {
            report.orphan_blobs += 1;
        }
    }
    Ok(())
}

#[derive(QueryableByName)]
struct HashRow {
    #[diesel(sql_type = diesel::sql_types::Text)]
    algo: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    digest: String,
}

#[derive(QueryableByName)]
struct Found {
    #[diesel(sql_type = diesel::sql_types::Bool)]
    found: bool,
}

/// Whether any library or album (trash included) still holds a file with this hash.
fn referenced(conn: &mut SqliteConnection, hash: &ObjectId) -> Result<bool> {
    let row: Found = diesel::sql_query(
        "SELECT EXISTS (SELECT 1 FROM file_hash h WHERE h.algo = ? AND h.digest = ? AND ( \
             EXISTS (SELECT 1 FROM user_file u WHERE u.file_id = h.file_id) \
             OR EXISTS (SELECT 1 FROM album_file a WHERE a.file_id = h.file_id))) AS found",
    )
    .bind::<diesel::sql_types::Text, _>(hash.algo())
    .bind::<diesel::sql_types::Text, _>(hash.digest())
    .get_result(conn)?;
    Ok(row.found)
}

/// Starts the grace period for blobs that just lost their last reference,
/// and ends it for ones referenced again.
fn track_unreferenced(log: &EventLog<Server>, report: &mut Report) -> Result<()> {
    let now = now_ms();
    let mut conn = log.writable_connection()?;
    let unreferenced: Vec<HashRow> = diesel::sql_query(
        "SELECT h.algo, h.digest FROM file_hash h \
         WHERE NOT EXISTS (SELECT 1 FROM user_file u WHERE u.file_id = h.file_id) \
           AND NOT EXISTS (SELECT 1 FROM album_file a WHERE a.file_id = h.file_id) \
           AND NOT EXISTS (SELECT 1 FROM blob_gc g WHERE g.algo = h.algo AND g.digest = h.digest)",
    )
    .load(&mut conn)?;
    for row in unreferenced {
        diesel::insert_into(blob_gc::table)
            .values((blob_gc::algo.eq(row.algo), blob_gc::digest.eq(row.digest), blob_gc::unreferenced_since.eq(now)))
            .execute(&mut conn)?;
        report.newly_unreferenced += 1;
    }

    let tracked: Vec<(String, String, Option<i64>)> = blob_gc::table.select((blob_gc::algo, blob_gc::digest, blob_gc::reclaimed_at)).load(&mut conn)?;
    for (algo, digest, reclaimed_at) in tracked {
        let hash = ObjectId::new(algo, digest);
        if referenced(&mut conn, &hash)? {
            if reclaimed_at.is_some() {
                // Every way back into a library uploads the bytes again, which
                // clears the row; reaching here means something else did.
                report.lost.push(hash.clone());
            }
            mark_present(&mut conn, &hash)?;
        }
    }
    Ok(())
}

/// Deletes blobs (and their thumbnails) unreferenced for longer than `grace`.
fn reclaim(log: &EventLog<Server>, objects_dir: &Path, grace: Duration, report: &mut Report) -> Result<()> {
    let cutoff = now_ms() - i64::try_from(grace.as_millis()).unwrap_or(i64::MAX);
    let due: Vec<(String, String)> = {
        let mut conn = log.connection()?;
        blob_gc::table
            .filter(blob_gc::reclaimed_at.is_null())
            .filter(blob_gc::unreferenced_since.le(cutoff))
            .select((blob_gc::algo, blob_gc::digest))
            .load(&mut conn)?
    };
    for (algo, digest) in due {
        let hash = ObjectId::new(algo, digest);
        let _exclusive = UPLOAD_LOCK.write().unwrap_or_else(|e| e.into_inner());
        let mut conn = log.writable_connection()?;
        let still_due: bool = diesel::select(exists(
            blob_gc::table.filter(blob_gc::algo.eq(hash.algo())).filter(blob_gc::digest.eq(hash.digest())).filter(blob_gc::reclaimed_at.is_null()),
        ))
        .get_result(&mut conn)?;
        if !still_due || referenced(&mut conn, &hash)? {
            continue; // uploaded again since the list was read
        }
        let mut freed = 0;
        remove(&blob::path(objects_dir, &hash), &mut freed)?;
        for size in ["small", "medium"] {
            remove(&objects_dir.join(format!("{hash}.thumb-{size}.jpg")), &mut freed)?;
        }
        diesel::update(blob_gc::table.filter(blob_gc::algo.eq(hash.algo())).filter(blob_gc::digest.eq(hash.digest())))
            .set(blob_gc::reclaimed_at.eq(now_ms()))
            .execute(&mut conn)?;
        report.reclaimed_blobs += 1;
        report.freed_bytes += freed;
    }
    Ok(())
}
