-- Blobs no library or album references any more (src/housekeeping.rs):
-- since when, and whether this node has deleted the bytes. Local state like
-- `blob_check`: written directly, never through the event log, since each
-- node reclaims its own disk on its own schedule.
--
-- A row is removed as soon as the blob is referenced again (an upload of the
-- same bytes), which also restarts the grace period if it's dropped later.
CREATE TABLE blob_gc (
    algo               TEXT NOT NULL,
    digest             TEXT NOT NULL,
    unreferenced_since INTEGER NOT NULL,
    reclaimed_at       INTEGER,
    PRIMARY KEY (algo, digest)
);
