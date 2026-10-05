use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use chronicler::{Event, EventLog};
use diesel::dsl::exists;
use diesel::prelude::*;
use sha2::{Digest, Sha256};

use crate::action::{Action, FileSource};
use crate::blob;
use crate::error::{Result, ServerError};
use crate::housekeeping;
use crate::ids::{FileId, UserId};
use crate::object_id::ObjectId;
use crate::projection::Server;
use crate::schema::{album_file, album_member, file_hash, user_file};
use crate::scrub;
use crate::thumbnail::{self, ThumbnailSize};
use crate::time::now_ms;
use crate::token::hex;

/// What size to serve a downloaded file at. `Small`/`Medium` are generated
/// (and cached) on first request — see `thumbnail::get_or_create` — for
/// images and videos (a video's is one extracted frame). When none can be
/// made, the original stands in if it's small (THUMBNAIL_FALLBACK_MAX_BYTES),
/// otherwise the request fails with `ServerError::NoThumbnail`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FileVariant {
    #[default]
    Original,
    Small,
    Medium,
}

/// Dimensions/duration are probed from the bytes by the caller (ffmpeg,
/// image decoders — I/O, so it can't happen in `apply()`) and passed in
/// already resolved.
#[derive(Debug, Clone, Default)]
pub struct FileMetadata {
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub duration: Option<i32>,
    pub previous_version: Option<FileId>,
    /// Capture time, ms since epoch (the device's "date taken").
    pub taken_at: Option<i64>,
}

/// Takes the actual bytes, not a caller-claimed hash — the server computes
/// the hash itself from what it actually received, so there's no way for
/// `file_hash` to disagree with what's on disk (core.md: "Hash match with
/// size mismatch is a hard error" — computing it here instead of trusting
/// a claim makes that class of error structurally impossible rather than
/// something to detect after the fact).
///
/// Write ordering follows server.md: the blob is written to disk *before*
/// the event is appended, so a crash in between leaves only a harmless
/// orphan file, never a DB row pointing at bytes that were never written.
pub fn upload_file(log: &EventLog<Server>, objects_dir: &Path, actor: UserId, data: &[u8], mime: String, name: Option<String>, meta: FileMetadata) -> Result<Event<UserId, Action>> {
    let hash = ObjectId::sha2_256(hex(&Sha256::digest(data)));

    // Held until the event references the blob: housekeeping must not
    // reclaim it in between.
    let _upload = housekeeping::upload_guard();
    // Idempotent (see blob::write's doc comment), so writing again even on
    // a dedup hit is harmless — simpler than branching on whether it exists.
    blob::write(objects_dir, &hash, data)?;

    record_upload(log, actor, hash, data.len() as i64, mime, name, meta)
}

/// The part every upload path shares, once the blob is on disk: reuse the
/// file_id if these bytes are already known (dedup), and append the event.
fn record_upload(log: &EventLog<Server>, actor: UserId, hash: ObjectId, size: i64, mime: String, name: Option<String>, meta: FileMetadata) -> Result<Event<UserId, Action>> {
    // The server just hashed these exact bytes: that's a verification, so
    // the periodic scrub (crate::scrub) can wait a full interval for it.
    // Best effort: a failure here only means it gets checked sooner.
    let _ = scrub::mark_verified(log, &hash);
    // The bytes are on disk again: any grace period before reclaiming them is over.
    housekeeping::mark_present(&mut *log.writable_connection()?, &hash)?;
    let existing: Option<String> = {
        let mut conn = log.connection()?;
        file_hash::table
            .filter(file_hash::algo.eq(hash.algo()))
            .filter(file_hash::digest.eq(hash.digest()))
            .select(file_hash::file_id)
            .first(&mut conn)
            .optional()?
    };

    let file_id = match existing {
        Some(id) => id.parse().expect("file_id column always holds a valid FileId"),
        None => FileId::new(),
    };

    Ok(log.append_and_apply(
        actor,
        Action::UploadFile {
            file_id,
            hash,
            size,
            mime,
            width: meta.width,
            height: meta.height,
            duration: meta.duration,
            previous_version: meta.previous_version,
            source: FileSource::Upload,
            name,
            taken_at: meta.taken_at,
        },
        now_ms(),
    )?)
}

// ---------------------------------------------------------- chunked uploads
//
// For files too large for one request (a video, a RAW photo). A partial
// upload is a plain file under `objects_dir/.uploads`, named by uploader and
// hash, so resuming needs no state beyond the filesystem: its length is how
// much has arrived. Keyed by uploader so two users never share one, and on
// the same filesystem as the blobs so committing is an atomic rename.
//
// One never committed is deleted by housekeeping after a while (see
// housekeeping::Settings::upload_expiry).

fn partial_path(objects_dir: &Path, actor: UserId, hash: &ObjectId) -> PathBuf {
    objects_dir.join(".uploads").join(format!("{actor}.{}.{}", hash.algo(), hash.digest()))
}

/// Starts, or resumes, a chunked upload of the file with this `hash` and
/// `size`. Returns how many bytes the server already has: 0 for a new
/// upload, more to resume, `size` if everything arrived and only
/// `commit_upload` is missing.
pub fn begin_upload(objects_dir: &Path, actor: UserId, hash: &ObjectId, size: u64) -> Result<u64> {
    if hash.algo() != "sha2-256" {
        return Err(ServerError::InvalidUpload(format!("unsupported hash algorithm {:?}", hash.algo())));
    }
    let path = partial_path(objects_dir, actor, hash);
    std::fs::create_dir_all(path.parent().expect("partial_path is always inside .uploads"))?;

    let have = match std::fs::metadata(&path) {
        Ok(m) => m.len(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(e) => return Err(e.into()),
    };
    if have > size {
        // Can't be the same file: start over rather than trust any of it.
        std::fs::remove_file(&path)?;
    }
    if have == 0 || have > size {
        File::create(&path)?;
        return Ok(0);
    }
    Ok(have)
}

/// Appends `data` at `offset`, which must be exactly where the upload is
/// (`ServerError::WrongOffset` otherwise, so the client re-asks
/// `begin_upload`). Not fsynced here: `commit_upload` does, once.
pub fn upload_chunk(objects_dir: &Path, actor: UserId, hash: &ObjectId, offset: u64, data: &[u8]) -> Result<()> {
    let path = partial_path(objects_dir, actor, hash);
    let mut file = match OpenOptions::new().append(true).open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ServerError::InvalidUpload("no upload in progress for this hash: call BeginUpload first".to_string()));
        }
        Err(e) => return Err(e.into()),
    };
    let have = file.metadata()?.len();
    if have != offset {
        return Err(ServerError::WrongOffset { expected: have, got: offset });
    }
    file.write_all(data)?;
    Ok(())
}

/// Finishes a chunked upload: hashes what actually arrived, and only if it
/// matches the declared `hash` moves it into place as the blob and appends
/// the event (blob first, event last, as `upload_file`). On a mismatch the
/// partial is dropped and `ServerError::HashMismatch` returned.
pub fn commit_upload(log: &EventLog<Server>, objects_dir: &Path, actor: UserId, hash: &ObjectId, mime: String, name: Option<String>, meta: FileMetadata) -> Result<Event<UserId, Action>> {
    let path = partial_path(objects_dir, actor, hash);
    let (actual, size) = match File::open(&path) {
        Ok(mut file) => hash_reader(&mut file)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ServerError::InvalidUpload("no upload in progress for this hash: call BeginUpload first".to_string()));
        }
        Err(e) => return Err(e.into()),
    };
    if &actual != hash {
        std::fs::remove_file(&path)?;
        return Err(ServerError::HashMismatch { declared: hash.to_string(), actual: actual.to_string() });
    }

    let _upload = housekeeping::upload_guard();
    blob::adopt(objects_dir, hash, &path)?;
    record_upload(log, actor, actual, size as i64, mime, name, meta)
}

fn hash_reader(reader: &mut impl Read) -> Result<(ObjectId, u64)> {
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
    }
    Ok((ObjectId::sha2_256(hex(&hasher.finalize())), size))
}

/// Takes the file out of the actor's own library ("All Photos"). Nothing is
/// deleted: the content stays while any library or album references it.
/// `FileNotFound` if it isn't in their library.
pub fn remove_from_library(log: &EventLog<Server>, actor: UserId, file_id: FileId) -> Result<Event<UserId, Action>> {
    let in_library: bool = {
        let mut conn = log.connection()?;
        diesel::select(exists(
            user_file::table.filter(user_file::user_id.eq(actor.to_string())).filter(user_file::file_id.eq(file_id.to_string())),
        ))
        .get_result(&mut conn)?
    };
    if !in_library {
        return Err(ServerError::FileNotFound);
    }
    Ok(log.append_and_apply(actor, Action::RemoveFromLibrary { file_id }, now_ms())?)
}

/// Same visibility `download_file` and `rename_file` both gate on: `actor`
/// either owns the file (a `user_file` row) or is a member of some album —
/// any role, `Reader` included — that the file is currently (non-trashed)
/// in. No separate "can you rename/download this" permission exists beyond
/// "can you already see this file's existence".
pub(crate) fn can_see_file(conn: &mut diesel::sqlite::SqliteConnection, actor: UserId, file_id: FileId) -> Result<bool> {
    let owns_it: bool = diesel::select(exists(
        user_file::table.filter(user_file::user_id.eq(actor.to_string())).filter(user_file::file_id.eq(file_id.to_string())),
    ))
    .get_result(conn)?;
    if owns_it {
        return Ok(true);
    }

    let visible_via_album: bool = diesel::select(exists(
        album_file::table
            .inner_join(album_member::table.on(album_member::album_id.eq(album_file::album_id)))
            .filter(album_member::user_id.eq(actor.to_string()))
            .filter(album_file::file_id.eq(file_id.to_string()))
            .filter(album_file::trashed_at.is_null()),
    ))
    .get_result(conn)?;
    Ok(visible_via_album)
}

/// A name is global to the file (see `Action::RenameFile`'s doc comment),
/// so renaming it from within one album changes what it's called
/// everywhere else it's visible too — Library, any other album, a direct
/// link — not just the one you renamed it from.
pub fn rename_file(log: &EventLog<Server>, actor: UserId, file_id: FileId, name: &str) -> Result<Event<UserId, Action>> {
    {
        let mut conn = log.connection()?;
        if !can_see_file(&mut conn, actor, file_id)? {
            return Err(ServerError::FileNotFound);
        }
    }

    Ok(log.append_and_apply(actor, Action::RenameFile { file_id, name: name.to_string() }, now_ms())?)
}

/// `(bytes, mime, name)`. Gated on `can_see_file` — the same visibility a
/// client already has through `queries::list_library`/`list_album_files`.
/// `name` rides along on every variant (not just `Original`) so a caller
/// showing a thumbnail already has it without a second round-trip.
pub fn download_file(log: &EventLog<Server>, objects_dir: &Path, actor: UserId, file_id: FileId, variant: FileVariant) -> Result<(Vec<u8>, String, Option<String>)> {
    let file = visible_file(log, actor, file_id)?;

    let size = match variant {
        FileVariant::Original => {
            if file.size > MAX_WHOLE_DOWNLOAD_BYTES {
                return Err(ServerError::TooLarge { size: file.size });
            }
            return Ok((blob::read(objects_dir, &file.hash)?, file.mime, file.name));
        }
        FileVariant::Small => ThumbnailSize::Small,
        FileVariant::Medium => ThumbnailSize::Medium,
    };
    match thumbnail::get_or_create(objects_dir, &file.hash, &file.mime, size)? {
        Some(thumb) => Ok((thumb, "image/jpeg".to_string(), file.name)),
        // No thumbnail possible: the original stands in only while that's
        // cheap, so a gallery on metered data never pulls a whole video.
        None if file.size <= THUMBNAIL_FALLBACK_MAX_BYTES => Ok((blob::read(objects_dir, &file.hash)?, file.mime, file.name)),
        None => Err(ServerError::NoThumbnail),
    }
}

/// Largest file `download_file` sends whole: what fits one server-api frame
/// (16 MiB) with room for the envelope. Bigger ones go through `download_range`.
pub const MAX_WHOLE_DOWNLOAD_BYTES: u64 = 15 * 1024 * 1024;

/// A thumbnail request for a file nothing can thumbnail gets the original
/// only up to this size; beyond it, `ServerError::NoThumbnail`.
pub const THUMBNAIL_FALLBACK_MAX_BYTES: u64 = 2 * 1024 * 1024;

/// Largest piece `download_range` returns at once.
pub const MAX_DOWNLOAD_CHUNK_BYTES: u32 = 8 * 1024 * 1024;

/// A piece of a file, for downloads too large for one message (videos).
#[derive(Debug, Clone)]
pub struct DownloadedRange {
    pub data: Vec<u8>,
    /// The whole file's size, so the client knows when it's done.
    pub total_size: u64,
    pub mime: String,
    pub name: Option<String>,
}

/// Up to `length` bytes (capped at MAX_DOWNLOAD_CHUNK_BYTES) of the original
/// from `offset`; fewer at the end of the file, none at exactly its end. Same
/// visibility rule as `download_file`.
pub fn download_range(log: &EventLog<Server>, objects_dir: &Path, actor: UserId, file_id: FileId, offset: u64, length: u32) -> Result<DownloadedRange> {
    let file = visible_file(log, actor, file_id)?;
    if offset > file.size {
        return Err(ServerError::InvalidArgument(format!("offset {offset} is past the end of a {}-byte file", file.size)));
    }
    let want = u64::from(length.min(MAX_DOWNLOAD_CHUNK_BYTES)).min(file.size - offset);

    let mut blob_file = File::open(blob::path(objects_dir, &file.hash))?;
    blob_file.seek(SeekFrom::Start(offset))?;
    let mut data = Vec::with_capacity(want as usize);
    blob_file.take(want).read_to_end(&mut data)?;

    Ok(DownloadedRange { data, total_size: file.size, mime: file.mime, name: file.name })
}

struct VisibleFile {
    hash: ObjectId,
    mime: String,
    name: Option<String>,
    size: u64,
}

/// The file's metadata, if `actor` may see it (`can_see_file`); FileNotFound
/// otherwise, indistinguishable from a file that doesn't exist.
fn visible_file(log: &EventLog<Server>, actor: UserId, file_id: FileId) -> Result<VisibleFile> {
    let mut conn = log.connection()?;
    if !can_see_file(&mut conn, actor, file_id)? {
        return Err(ServerError::FileNotFound);
    }

    let (hash_algo, hash_digest, mime, name, size): (String, String, String, Option<String>, i64) = {
        use crate::schema::file;
        file::table
            .inner_join(file_hash::table.on(file_hash::file_id.eq(file::id)))
            .filter(file::id.eq(file_id.to_string()))
            .select((file_hash::algo, file_hash::digest, file::mime, file::name, file::size))
            .first(&mut conn)
            .optional()?
            .ok_or(ServerError::FileNotFound)?
    };
    Ok(VisibleFile { hash: ObjectId::new(hash_algo, hash_digest), mime, name, size: u64::try_from(size).unwrap_or(0) })
}
