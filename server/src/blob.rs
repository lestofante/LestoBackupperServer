// On-disk blob storage (server.md's "Blob storage" section): flat
// directory, content-addressed by the tagged hash, one file per object.
// Write ordering matters — server.md: "Create the orphan-able thing first;
// commit the reference last." `commands::file::upload_file` writes the
// blob (this module) *before* appending the UploadFile event; a crash
// between the two leaves a harmless orphan file, never a DB row pointing
// at bytes that don't exist.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::object_id::ObjectId;

fn path_for(objects_dir: &Path, hash: &ObjectId) -> PathBuf {
    objects_dir.join(hash.to_string())
}

/// Idempotent by construction, not by an existence check: the temp-file-
/// then-rename is safe to repeat even if the target already exists,
/// because identical hashes mean identical bytes (core.md's content-
/// addressing invariant) — a "duplicate" write just overwrites the file
/// with the bytes it already had.
pub fn write(objects_dir: &Path, hash: &ObjectId, bytes: &[u8]) -> io::Result<()> {
    fs::create_dir_all(objects_dir)?;

    let tmp_path = objects_dir.join(format!(".tmp-{}", hash.digest()));
    {
        let mut tmp = File::create(&tmp_path)?;
        tmp.write_all(bytes)?;
        tmp.sync_all()?;
    }
    fs::rename(&tmp_path, path_for(objects_dir, hash))
}

/// Whether the bytes for `hash` are actually on disk. A DB row for a hash
/// whose blob is gone means the store lost data (a hand-deleted objects dir,
/// a failed restore).
pub fn exists(objects_dir: &Path, hash: &ObjectId) -> bool {
    path_for(objects_dir, hash).is_file()
}

/// Moves an already fully written file into place as `hash`'s blob, with
/// the same fsync-then-rename ordering as `write`. `file` must be on the
/// same filesystem as `objects_dir` for the rename to be atomic.
pub fn adopt(objects_dir: &Path, hash: &ObjectId, file: &Path) -> io::Result<()> {
    File::open(file)?.sync_all()?;
    fs::rename(file, path_for(objects_dir, hash))
}

pub(crate) fn path(objects_dir: &Path, hash: &ObjectId) -> PathBuf {
    path_for(objects_dir, hash)
}

/// Moves a damaged blob out of the way, into `objects_dir/.corrupt/`, rather
/// than deleting it: with the blob gone the library reports the file as
/// missing, so a device that still has it re-uploads it, while the damaged
/// copy stays around for a human to inspect.
pub(crate) fn quarantine(objects_dir: &Path, hash: &ObjectId, now_ms: i64) -> io::Result<PathBuf> {
    let dir = objects_dir.join(".corrupt");
    fs::create_dir_all(&dir)?;
    let target = dir.join(format!("{hash}.{now_ms}"));
    fs::rename(path_for(objects_dir, hash), &target)?;
    Ok(target)
}

pub fn read(objects_dir: &Path, hash: &ObjectId) -> io::Result<Vec<u8>> {
    fs::read(path_for(objects_dir, hash))
}
