use serde::{Deserialize, Serialize};

use crate::ids::{AlbumId, FileId, UserId};
use crate::object_id::ObjectId;

/// Everything this domain considers "a thing that happened". One typed
/// enum, one `match` in `apply()` (eventlog.md) — an unhandled new variant
/// is a compile error, not a silent gap.
///
/// Old variants stay replayable forever: never change what one means, add
/// a new one instead (e.g. `UploadFileV2`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Action {
    /// `is_admin`/`status` default (via serde) to `false`/`Active` when
    /// absent — the correct reading of any event recorded before this
    /// field existed, when every created account was immediately usable
    /// and none was an admin. New events set both explicitly: ordinary
    /// self-registration is `is_admin: false, status: Pending`; the
    /// bootstrap admin (`ensure_bootstrap_admin`) is the one case that
    /// sets `is_admin: true, status: Active` directly, skipping approval.
    CreateUser {
        user_id: UserId,
        username: String,
        password_hash: String,
        #[serde(default)]
        is_admin: bool,
        #[serde(default = "AccountStatus::default_for_missing_field")]
        status: AccountStatus,
    },
    /// Admin-only (checked in the command, not here): flips a pending
    /// account to active. No reject action exists yet — an admin simply
    /// never approves an account they don't want, and it stays pending.
    ApproveUser {
        user_id: UserId,
    },
    /// Old password verified in the command, same as `CreateUser`'s
    /// hashing — `apply()` only ever sees the resulting hash.
    ChangePassword {
        user_id: UserId,
        password_hash: String,
    },
    /// `file_id` and `hash` are both resolved by the command: if the hash
    /// is already known, `file_id` is the existing file's id (dedup); the
    /// event is self-sufficient either way and needs no lookup to apply.
    UploadFile {
        file_id: FileId,
        hash: ObjectId,
        size: i64,
        mime: String,
        width: Option<i32>,
        height: Option<i32>,
        duration: Option<i32>,
        previous_version: Option<FileId>,
        source: FileSource,
        /// Defaults to the original filename the client uploaded it as
        /// (e.g. a browser `File`'s `.name`); absent for historical events
        /// (`#[serde(default)]` — every account's uploads before this
        /// field existed just have no name, same as a dedup hit that never
        /// resolved one). A name is a property of the file itself, not of
        /// any one album it's filed into — the same photo shouldn't have a
        /// different name depending which album you're looking at it from.
        #[serde(default)]
        name: Option<String>,
        /// When it was taken (ms since epoch), as the uploading device knew
        /// it. Absent for events from before this field existed
        /// (`#[serde(default)]`), and for content with no known capture time.
        #[serde(default)]
        taken_at: Option<i64>,
    },
    /// Anyone who can already see the file (owns it, or is a member of an
    /// album containing it — the same visibility `download_file` checks)
    /// can rename it, same as `AddToAlbum`'s reasoning: nothing here is
    /// destructive or shared-trust-sensitive enough to need a stronger
    /// gate than "you can already see this exists".
    RenameFile {
        file_id: FileId,
        name: String,
    },
    CreateAlbum {
        album_id: AlbumId,
        owner: UserId,
        name: String,
        kind: AlbumKind,
    },
    AddToAlbum {
        album_id: AlbumId,
        file_id: FileId,
        added_by: UserId,
    },
    /// Reversible hide, not a delete: the `album_file` row survives (with
    /// `trashed_at`/`trashed_by` set), so the file stays referenced and
    /// nothing is lost until an explicit `PurgeFromAlbum`. Same permission
    /// level as `AddToAlbum` precisely because it's undoable.
    TrashFromAlbum {
        album_id: AlbumId,
        file_id: FileId,
    },
    /// Owner-only: undoes `TrashFromAlbum`.
    RestoreFromTrash {
        album_id: AlbumId,
        file_id: FileId,
    },
    /// Owner-only, and only once already trashed (command-layer check, not
    /// enforced here): the actual, irreversible delete of the `album_file`
    /// row. This is the only one of the three that a replay can't undo.
    PurgeFromAlbum {
        album_id: AlbumId,
        file_id: FileId,
    },
    /// Sharing is inserting a row; the same variant upserts a role change
    /// for an existing member (server.md: no `is_shared` flag, no
    /// branching).
    ShareAlbum {
        album_id: AlbumId,
        user_id: UserId,
        role: AlbumRole,
    },
    /// Unsharing is deleting the row.
    UnshareAlbum {
        album_id: AlbumId,
        user_id: UserId,
    },
    /// Owner-only (checked in the command): changes `album.name`. Subject
    /// to the same per-owner uniqueness rule as `CreateAlbum`.
    RenameAlbum {
        album_id: AlbumId,
        name: String,
    },
    /// Removes this album from `actor`'s own album list without touching
    /// anyone else's — a per-membership flag (`album_member.hidden`), not
    /// an album-wide property, since decluttering your own view shouldn't
    /// affect what other members see. Reversible via `UnhideAlbum`; unlike
    /// `TrashFromAlbum`/`PurgeFromAlbum` there's no owner-only step here
    /// because nothing shared is ever at stake, only the caller's own
    /// list. `actor` comes from the wrapping `Event`, same as
    /// `TrashFromAlbum`'s `trashed_by`.
    HideAlbum {
        album_id: AlbumId,
    },
    /// Undoes `HideAlbum`.
    UnhideAlbum {
        album_id: AlbumId,
    },
    /// Owner-only. The album, its memberships and its file links go; the
    /// files themselves stay in every library that has them (server.md:
    /// "Removing ... deletes only that row").
    DeleteAlbum {
        album_id: AlbumId,
    },
    /// The actor's own library entry for a file goes (their "All Photos").
    /// The content survives while any library or album references it, and
    /// stays visible to the actor through albums they're a member of.
    RemoveFromLibrary {
        file_id: FileId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccountStatus {
    Pending,
    Active,
}

impl AccountStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            AccountStatus::Pending => "pending",
            AccountStatus::Active => "active",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(AccountStatus::Pending),
            "active" => Some(AccountStatus::Active),
            _ => None,
        }
    }

    // Named for what it's for, not what it returns — the value only makes
    // sense in light of *why* CreateUser defaults here (see that variant's
    // doc comment), not as a general-purpose "the default status is Active".
    fn default_for_missing_field() -> Self {
        AccountStatus::Active
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileSource {
    Upload,
    SavedFromAlbum,
}

impl FileSource {
    pub fn as_str(self) -> &'static str {
        match self {
            FileSource::Upload => "upload",
            FileSource::SavedFromAlbum => "saved_from_album",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AlbumKind {
    User,
    System,
}

impl AlbumKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AlbumKind::User => "user",
            AlbumKind::System => "system",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "user" => Some(AlbumKind::User),
            "system" => Some(AlbumKind::System),
            _ => None,
        }
    }
}

/// Ordered weakest to strongest; see [`AlbumRole::can`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AlbumRole {
    Reader,
    Writer,
    Admin,
    Owner,
}

impl AlbumRole {
    pub fn as_str(self) -> &'static str {
        match self {
            AlbumRole::Reader => "reader",
            AlbumRole::Writer => "writer",
            AlbumRole::Admin => "admin",
            AlbumRole::Owner => "owner",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "reader" => Some(AlbumRole::Reader),
            "writer" => Some(AlbumRole::Writer),
            "admin" => Some(AlbumRole::Admin),
            "owner" => Some(AlbumRole::Owner),
            _ => None,
        }
    }

    /// Whether this role meets or exceeds `required`.
    pub fn can(self, required: AlbumRole) -> bool {
        self >= required
    }
}

pub fn action_type(action: &Action) -> &'static str {
    match action {
        Action::CreateUser { .. } => "user.create",
        Action::ApproveUser { .. } => "user.approve",
        Action::ChangePassword { .. } => "user.change_password",
        Action::UploadFile { .. } => "file.upload",
        Action::RenameFile { .. } => "file.rename",
        Action::CreateAlbum { .. } => "album.create",
        Action::AddToAlbum { .. } => "album.add_file",
        Action::TrashFromAlbum { .. } => "album.trash_file",
        Action::RestoreFromTrash { .. } => "album.restore_file",
        Action::PurgeFromAlbum { .. } => "album.purge_file",
        Action::ShareAlbum { .. } => "album.share",
        Action::UnshareAlbum { .. } => "album.unshare",
        Action::RenameAlbum { .. } => "album.rename",
        Action::HideAlbum { .. } => "album.hide",
        Action::UnhideAlbum { .. } => "album.unhide",
        Action::DeleteAlbum { .. } => "album.delete",
        Action::RemoveFromLibrary { .. } => "file.remove_from_library",
    }
}
