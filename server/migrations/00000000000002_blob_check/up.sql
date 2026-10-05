-- When each blob on this node's disk was last read back and verified
-- (src/scrub.rs). Local state, not domain history: another mirror has its
-- own disk and its own answer. So, like `session`, it's written directly
-- through EventLog::writable_connection(), never through the event log.
--
-- status: 'ok' | 'missing' | 'corrupt'. An upload records 'ok', since the
-- server hashed those exact bytes as they arrived.
CREATE TABLE blob_check (
    algo       TEXT NOT NULL,
    digest     TEXT NOT NULL,
    checked_at INTEGER NOT NULL,
    status     TEXT NOT NULL,
    PRIMARY KEY (algo, digest)
);
