-- Domain projection tables. Every row here is rebuildable by replaying the
-- chronicler event log from scratch (see src/projection.rs) — this schema
-- has no independent authority.

-- status: 'pending' | 'active'. New self-registrations start pending and
-- need an admin's ApproveUser event before login succeeds (session::login).
-- is_admin: system-wide, unrelated to any one album's ACL — see
-- Action::CreateUser's doc comment for why a new account defaults to
-- neither (is_admin=0, status='pending'), while a historical event that
-- predates this column defaults to status='active' via serde, since every
-- account created before this feature existed was already usable.
CREATE TABLE app_user (
    user_id    TEXT PRIMARY KEY,
    username   TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    is_admin   BOOLEAN NOT NULL DEFAULT 0,
    status     TEXT NOT NULL DEFAULT 'pending'
);

-- One password_hash column: Argon2id encodes salt, algorithm and cost
-- inside the string. A separate salt column would signal a hand-rolled
-- scheme (see server.md).
--
-- otp_secret is NULL until TOTP is enrolled. Unlike password_hash, this is
-- a *symmetric* secret — reading it lets you generate valid codes forever,
-- so (like `session` below) this table is written directly, never through
-- the event log: an OTP secret must never sit in the immutable, forever-
-- replayable, chronicler-dump-able event history.
CREATE TABLE credential (
    user_id       TEXT PRIMARY KEY REFERENCES app_user(user_id),
    password_hash TEXT NOT NULL,
    otp_secret    TEXT
);

-- Not event-sourced (server.md lists this alongside `credential` under
-- "Auth", separate from the domain schema) — written directly via
-- EventLog::writable_connection(). A session isn't domain history, and a
-- revoked token must actually disappear, which an immutable log can't do.
--
-- `token` stores sha256(actual token), never the raw token — so a DB leak
-- doesn't hand out live sessions. The raw token is returned to the client
-- exactly once, at login.
CREATE TABLE session (
    token        TEXT PRIMARY KEY,
    user_id      TEXT NOT NULL REFERENCES app_user(user_id),
    issued_at    INTEGER NOT NULL,
    expires_at   INTEGER,
    device_label TEXT NOT NULL
);
CREATE INDEX session_user_id_idx ON session(user_id);

-- Pure content: no creator, no owner column (server.md). With dedup, the
-- same bytes are one row; an owner column would misattribute a second
-- uploader's file.
CREATE TABLE file (
    id                TEXT PRIMARY KEY,
    size              INTEGER NOT NULL,
    mime              TEXT NOT NULL,
    width             INTEGER,
    height            INTEGER,
    duration          INTEGER,
    previous_version  TEXT REFERENCES file(id),
    -- A property of the file itself, not of any one album it's filed
    -- into (see Action::UploadFile's doc comment) — defaults to the
    -- original filename on upload, changeable via Action::RenameFile.
    name              TEXT
);

-- N rows per file: migration to a new hash algorithm adds a row rather
-- than replacing one, so no historical event is ever reinterpreted.
CREATE TABLE file_hash (
    file_id TEXT NOT NULL REFERENCES file(id),
    algo    TEXT NOT NULL,
    digest  TEXT NOT NULL,
    PRIMARY KEY (algo, digest)
);
CREATE INDEX file_hash_file_id_idx ON file_hash(file_id);

-- A user's library and the attribution record. "All Photos" is this
-- table, not an album row.
CREATE TABLE user_file (
    user_id  TEXT NOT NULL REFERENCES app_user(user_id),
    file_id  TEXT NOT NULL REFERENCES file(id),
    added_at INTEGER NOT NULL,
    source   TEXT NOT NULL,
    PRIMARY KEY (user_id, file_id)
);

CREATE TABLE album (
    album_id TEXT PRIMARY KEY,
    name     TEXT NOT NULL,
    kind     TEXT NOT NULL
);

-- ACL, not owner+group: sharing is inserting a row, unsharing is deleting
-- one. No is_shared flag, no branching (server.md).
--
-- hidden is per-membership, not per-album: decluttering your own album
-- list (HideAlbum/UnhideAlbum) shouldn't affect what any other member
-- sees, so it lives on the (album_id, user_id) row, not on `album` itself.
CREATE TABLE album_member (
    album_id TEXT NOT NULL REFERENCES album(album_id),
    user_id  TEXT NOT NULL REFERENCES app_user(user_id),
    role     TEXT NOT NULL,
    hidden   BOOLEAN NOT NULL DEFAULT 0,
    PRIMARY KEY (album_id, user_id)
);

-- Name uniqueness is scoped per owner ("owner" = the album_member row with
-- role = 'owner'), so two users' "Vacation" coexist. Enforced in the
-- create_album command, not by a DB constraint, since the owner is a
-- separate table.
CREATE INDEX album_name_idx ON album(name);

-- trashed_at NULL = active. Non-NULL = hidden pending an owner's decision to
-- restore or purge. The row itself is never deleted by trashing, only by
-- purging — so a trashed file still counts as a reference for the (not yet
-- implemented) GC pass over unreferenced blobs (server.md).
CREATE TABLE album_file (
    album_id   TEXT NOT NULL REFERENCES album(album_id),
    file_id    TEXT NOT NULL REFERENCES file(id),
    added_at   INTEGER NOT NULL,
    added_by   TEXT NOT NULL REFERENCES app_user(user_id),
    trashed_at INTEGER,
    trashed_by TEXT REFERENCES app_user(user_id),
    PRIMARY KEY (album_id, file_id)
);
