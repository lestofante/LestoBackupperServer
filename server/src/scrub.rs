// Periodic integrity check of the blobs on disk ("scrubbing"): read each
// blob back, re-hash it, and compare hash and size with what the DB says
// (core.md: verify, and a mismatch is surfaced to a human, never silently
// "fixed"). Only reads, so it doesn't wear an SSD (wear comes from writes);
// the real cost is disk bandwidth, hence `Throttle`.
//
// Each blob is due `interval` after it was last verified. An upload counts
// as a verification (`mark_verified`): the server hashed those exact bytes
// as they arrived. Blobs from before this table existed have no row and are
// due immediately, once.
//
// When a blob was last verified is per-node state (a mirror has its own
// disk), so `blob_check` is written directly, never through the event log.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use diesel::dsl::{exists, not};
use diesel::prelude::*;
use sha2::{Digest, Sha256};

use crate::blob;
use crate::error::Result;
use crate::object_id::ObjectId;
use crate::projection::Server;
use crate::schema::{blob_check, blob_gc, file, file_hash};
use crate::time::now_ms;
use crate::token::hex;
use chronicler::EventLog;

/// A blob whose last verification is older than the interval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DueBlob {
    pub hash: ObjectId,
    /// What the DB says the file's size is.
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobStatus {
    Ok,
    Missing,
    Corrupt,
}

impl BlobStatus {
    fn as_str(self) -> &'static str {
        match self {
            BlobStatus::Ok => "ok",
            BlobStatus::Missing => "missing",
            BlobStatus::Corrupt => "corrupt",
        }
    }
}

/// A blob that failed its check. `detail` is for the log: what was wrong,
/// and for a corrupt one, where it was moved.
#[derive(Debug, Clone)]
pub struct Problem {
    pub hash: ObjectId,
    pub status: BlobStatus,
    pub detail: String,
}

/// Caps the read rate across a whole run, so a scrub never hogs the disk.
pub struct Throttle {
    started: Instant,
    bytes: u64,
    max_bytes_per_sec: u64,
}

impl Throttle {
    pub fn new(max_bytes_per_sec: u64) -> Self {
        Throttle { started: Instant::now(), bytes: 0, max_bytes_per_sec: max_bytes_per_sec.max(1) }
    }

    /// Records `n` more bytes read, sleeping if that's ahead of the cap.
    fn consumed(&mut self, n: usize) {
        self.bytes += n as u64;
        let allowed_at = Duration::from_secs_f64(self.bytes as f64 / self.max_bytes_per_sec as f64);
        if let Some(wait) = allowed_at.checked_sub(self.started.elapsed()) {
            thread::sleep(wait);
        }
    }
}

/// Records that `hash`'s blob was just verified as fine. Called on upload.
pub fn mark_verified(log: &EventLog<Server>, hash: &ObjectId) -> Result<()> {
    record(log, hash, BlobStatus::Ok)
}

fn record(log: &EventLog<Server>, hash: &ObjectId, status: BlobStatus) -> Result<()> {
    let mut conn = log.writable_connection()?;
    diesel::replace_into(blob_check::table)
        .values((
            blob_check::algo.eq(hash.algo()),
            blob_check::digest.eq(hash.digest()),
            blob_check::checked_at.eq(now_ms()),
            blob_check::status.eq(status.as_str()),
        ))
        .execute(&mut conn)?;
    Ok(())
}

/// Every blob not verified within `interval`, least recently checked first
/// (never-checked ones before all others).
pub fn due(log: &EventLog<Server>, interval: Duration) -> Result<Vec<DueBlob>> {
    let cutoff = now_ms() - i64::try_from(interval.as_millis()).unwrap_or(i64::MAX);
    let mut conn = log.connection()?;
    let rows: Vec<(String, String, i64, Option<i64>)> = file_hash::table
        .inner_join(file::table.on(file::id.eq(file_hash::file_id)))
        .left_join(blob_check::table.on(blob_check::algo.eq(file_hash::algo).and(blob_check::digest.eq(file_hash::digest))))
        .filter(blob_check::checked_at.is_null().or(blob_check::checked_at.le(cutoff)))
        // Reclaimed by housekeeping: gone on purpose, not missing.
        .filter(not(exists(
            blob_gc::table.filter(blob_gc::algo.eq(file_hash::algo)).filter(blob_gc::digest.eq(file_hash::digest)).filter(blob_gc::reclaimed_at.is_not_null()),
        )))
        .order(blob_check::checked_at.asc())
        .select((file_hash::algo, file_hash::digest, file::size, blob_check::checked_at.nullable()))
        .load(&mut conn)?;

    Ok(rows
        .into_iter()
        .map(|(algo, digest, size, _)| DueBlob { hash: ObjectId::new(algo, digest), size: u64::try_from(size).unwrap_or(0) })
        .collect())
}

/// Reads one blob back and records the verdict. `Ok(None)` if it's fine;
/// a corrupt blob is moved to `objects/.corrupt/` (see `blob::quarantine`).
/// `Err` only for trouble that says nothing about the blob (the DB, a read
/// error other than "not found"): it stays due and is retried next run.
pub fn verify_and_record(log: &EventLog<Server>, objects_dir: &Path, blob: &DueBlob, throttle: &mut Throttle) -> Result<Option<Problem>> {
    if blob.hash.algo() != "sha2-256" {
        return Ok(None); // nothing to check it with yet
    }

    let mut file = match File::open(blob::path(objects_dir, &blob.hash)) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            record(log, &blob.hash, BlobStatus::Missing)?;
            return Ok(Some(Problem { hash: blob.hash.clone(), status: BlobStatus::Missing, detail: "blob file is missing".to_string() }));
        }
        Err(e) => return Err(e.into()),
    };

    let (digest, size) = hash_throttled(&mut file, throttle)?;
    if digest == blob.hash.digest() && size == blob.size {
        record(log, &blob.hash, BlobStatus::Ok)?;
        return Ok(None);
    }

    let found = format!("read back {size} bytes hashing to sha2-256:{digest}, expected {} bytes", blob.size);
    let moved_to = blob::quarantine(objects_dir, &blob.hash, now_ms())?;
    record(log, &blob.hash, BlobStatus::Corrupt)?;
    Ok(Some(Problem { hash: blob.hash.clone(), status: BlobStatus::Corrupt, detail: format!("{found}; moved to {}", moved_to.display()) }))
}

fn hash_throttled(reader: &mut impl Read, throttle: &mut Throttle) -> io::Result<(String, u64)> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut size = 0u64;
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        size += n as u64;
        throttle.consumed(n);
    }
    Ok((hex(&hasher.finalize()), size))
}
