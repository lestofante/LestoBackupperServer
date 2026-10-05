// On-disk thumbnail cache: two fixed sizes, generated on first request and
// then cached forever under the same objects_dir as the originals (blob.rs)
// — content-addressed by the *original's* hash plus a size suffix, so an
// identical upload (same hash) never gets thumbnailed twice, and the cache
// entry outlives whatever `file` row references the original (matches
// core.md's content-addressing invariant: same bytes in, same bytes out).
//
// `Small` is sized for a phone-width grid tile or a progressive-loading
// placeholder; `Medium` for a desktop grid tile or album preview. Neither
// is meant to replace the original for a full single-photo view — that
// still downloads the untouched bytes.
//
// Still images the `image` crate can decode are resized in-process, upright
// per their EXIF orientation. Everything else that's an image or a video
// (HEIC, every video) goes through `ffmpeg` (server.md: "Video: single
// extracted frame only... shells out to ffmpeg"); without ffmpeg installed
// those simply have no thumbnail.

use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, ImageDecoder, ImageReader, ImageResult};

use crate::blob;
use crate::object_id::ObjectId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbnailSize {
    Small,
    Medium,
}

impl ThumbnailSize {
    fn longest_edge(self) -> u32 {
        match self {
            ThumbnailSize::Small => 240,
            ThumbnailSize::Medium => 960,
        }
    }

    fn suffix(self) -> &'static str {
        match self {
            ThumbnailSize::Small => "small",
            ThumbnailSize::Medium => "medium",
        }
    }
}

/// A broken file can make ffmpeg spin; it must not hold a server thread forever.
const FFMPEG_TIMEOUT: Duration = Duration::from_secs(30);

fn cache_path(objects_dir: &Path, hash: &ObjectId, size: ThumbnailSize) -> PathBuf {
    objects_dir.join(format!("{hash}.thumb-{}.jpg", size.suffix()))
}

/// The cached thumbnail, creating it on first request. `Ok(None)` when the
/// file isn't something a thumbnail can be made of (not an image or video,
/// a format nothing here decodes, ffmpeg missing): not an error, the caller
/// decides what to send instead. A cache hit doesn't read the original.
pub fn get_or_create(objects_dir: &Path, hash: &ObjectId, mime: &str, size: ThumbnailSize) -> std::io::Result<Option<Vec<u8>>> {
    let cache = cache_path(objects_dir, hash, size);
    if let Ok(bytes) = std::fs::read(&cache) {
        return Ok(Some(bytes));
    }

    let is_video = mime.starts_with("video/");
    if !is_video && !mime.starts_with("image/") {
        return Ok(None);
    }
    let source = blob::path(objects_dir, hash);
    let resized = if is_video {
        None
    } else {
        decode_and_resize(&std::fs::read(&source)?, size)
    };
    let Some(resized) = resized.or_else(|| ffmpeg_frame(&source, is_video, size)) else { return Ok(None) };

    // Same atomic temp-then-rename discipline as blob::write. The temp name
    // is unique per call: two requests for the same thumbnail at once must
    // not write into one temp file.
    static NEXT_TMP: AtomicU64 = AtomicU64::new(0);
    let tmp = objects_dir.join(format!(".tmp-thumb-{}-{}-{}-{}", hash.digest(), size.suffix(), std::process::id(), NEXT_TMP.fetch_add(1, Ordering::Relaxed)));
    std::fs::write(&tmp, &resized)?;
    std::fs::rename(&tmp, &cache)?;

    Ok(Some(resized))
}

fn decode_and_resize(original: &[u8], size: ThumbnailSize) -> Option<Vec<u8>> {
    let mut decoder = ImageReader::new(Cursor::new(original)).with_guessed_format().ok()?.into_decoder().ok()?;
    // Phones often store the pixels sideways plus an EXIF "rotate me" flag.
    let orientation = decoder.orientation().ok();
    let mut img = DynamicImage::from_decoder(decoder).ok()?;
    if let Some(orientation) = orientation {
        img.apply_orientation(orientation);
    }

    let edge = size.longest_edge();
    // Fits within an `edge`x`edge` box, preserving aspect ratio — not a
    // crop, so portrait and landscape originals both end up correctly
    // proportioned rather than square.
    let resized = img.resize(edge, edge, image::imageops::FilterType::Triangle);

    let mut out = Vec::new();
    let encode: ImageResult<()> = {
        let encoder = JpegEncoder::new_with_quality(&mut out, 82);
        resized.write_with_encoder(encoder)
    };
    encode.ok()?;
    Some(out)
}

/// One JPEG frame, fitted in the size's box, from anything ffmpeg can read.
/// ffmpeg applies a video's rotation metadata itself. For videos the
/// `thumbnail` filter picks a representative frame among the first few, so a
/// fade-in doesn't give a black thumbnail.
fn ffmpeg_frame(source: &Path, is_video: bool, size: ThumbnailSize) -> Option<Vec<u8>> {
    let edge = size.longest_edge();
    let scale = format!("scale={edge}:{edge}:force_original_aspect_ratio=decrease");
    let filter = if is_video { format!("thumbnail=30,{scale}") } else { scale };

    let mut child = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(source)
        .args(["-frames:v", "1", "-vf", &filter, "-q:v", "4", "-f", "image2pipe", "-c:v", "mjpeg", "pipe:1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?; // not installed: no thumbnail

    // Drain stdout on its own thread (a full pipe would stall ffmpeg) while
    // this one enforces the timeout.
    let mut stdout = child.stdout.take()?;
    let reader = thread::spawn(move || {
        let mut out = Vec::new();
        stdout.read_to_end(&mut out).map(|_| out)
    });

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < FFMPEG_TIMEOUT => thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let out = reader.join().ok()?.ok()?;
    (status.success() && !out.is_empty()).then_some(out)
}
