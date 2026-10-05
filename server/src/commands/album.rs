use chronicler::{Event, EventLog};
use diesel::dsl::exists;
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;

use crate::action::{Action, AlbumKind, AlbumRole};
use crate::error::{Result, ServerError};
use crate::ids::{AlbumId, FileId, UserId};
use crate::projection::Server;
use crate::schema::{album, album_file, album_member, app_user};
use crate::time::now_ms;

pub fn create_album(log: &EventLog<Server>, owner: UserId, name: &str, kind: AlbumKind) -> Result<Event<UserId, Action>> {
    // Name uniqueness is scoped per owner (server.md), so this only checks
    // albums this user owns, not the whole table.
    let taken: bool = {
        let mut conn = log.connection()?;
        diesel::select(exists(
            album::table
                .inner_join(album_member::table.on(album_member::album_id.eq(album::album_id)))
                .filter(album_member::user_id.eq(owner.to_string()))
                .filter(album_member::role.eq(AlbumRole::Owner.as_str()))
                .filter(album::name.eq(name)),
        ))
        .get_result(&mut conn)?
    };
    if taken {
        return Err(ServerError::AlbumNameTaken(name.to_string()));
    }

    Ok(log.append_and_apply(
        owner,
        Action::CreateAlbum {
            album_id: AlbumId::new(),
            owner,
            name: name.to_string(),
            kind,
        },
        now_ms(),
    )?)
}

/// The actor must be able to see the file themselves (their library, or an
/// album they're in): album write access isn't a way to reach other files.
/// Re-adding a file that's in this album's trash restores it, which only
/// the owner may do (as `restore_from_trash`).
pub fn add_to_album(log: &EventLog<Server>, actor: UserId, album_id: AlbumId, file_id: FileId) -> Result<Event<UserId, Action>> {
    let in_trash = {
        let mut conn = log.connection()?;
        require_role(&mut conn, album_id, actor, AlbumRole::Writer)?;
        if !crate::commands::file::can_see_file(&mut conn, actor, file_id)? {
            return Err(ServerError::FileNotFound);
        }
        let trashed_at: Option<Option<i64>> = album_file::table
            .find((album_id.to_string(), file_id.to_string()))
            .select(album_file::trashed_at)
            .first(&mut conn)
            .optional()?;
        matches!(trashed_at, Some(Some(_)))
    };
    if in_trash {
        return restore_from_trash(log, actor, album_id, file_id);
    }
    Ok(log.append_and_apply(actor, Action::AddToAlbum { album_id, file_id, added_by: actor }, now_ms())?)
}

/// Owner-only: the album and its file links go, the files stay in libraries.
pub fn delete_album(log: &EventLog<Server>, actor: UserId, album_id: AlbumId) -> Result<Event<UserId, Action>> {
    {
        let mut conn = log.connection()?;
        require_role(&mut conn, album_id, actor, AlbumRole::Owner)?;
    }
    Ok(log.append_and_apply(actor, Action::DeleteAlbum { album_id }, now_ms())?)
}

/// Reversible hide — same permission level as `add_to_album`, since nothing
/// is actually lost yet. Use `purge_from_album` (owner-only) to actually
/// delete it.
pub fn trash_from_album(log: &EventLog<Server>, actor: UserId, album_id: AlbumId, file_id: FileId) -> Result<Event<UserId, Action>> {
    {
        let mut conn = log.connection()?;
        require_role(&mut conn, album_id, actor, AlbumRole::Writer)?;
    }

    Ok(log.append_and_apply(actor, Action::TrashFromAlbum { album_id, file_id }, now_ms())?)
}

/// Owner-only: the trash bin is only visible to (and reversible by) the
/// owner, not any writer or admin who could have trashed the file.
pub fn restore_from_trash(log: &EventLog<Server>, actor: UserId, album_id: AlbumId, file_id: FileId) -> Result<Event<UserId, Action>> {
    {
        let mut conn = log.connection()?;
        require_role(&mut conn, album_id, actor, AlbumRole::Owner)?;
    }

    Ok(log.append_and_apply(actor, Action::RestoreFromTrash { album_id, file_id }, now_ms())?)
}

/// Owner-only, and only for a file already sitting in the trash — you can't
/// skip straight past the review step by purging something still active.
pub fn purge_from_album(log: &EventLog<Server>, actor: UserId, album_id: AlbumId, file_id: FileId) -> Result<Event<UserId, Action>> {
    {
        let mut conn = log.connection()?;
        require_role(&mut conn, album_id, actor, AlbumRole::Owner)?;

        let trashed_at: Option<i64> = album_file::table
            .find((album_id.to_string(), file_id.to_string()))
            .select(album_file::trashed_at)
            .first(&mut conn)
            .optional()?
            .flatten();
        if trashed_at.is_none() {
            return Err(ServerError::NotTrashed);
        }
    }

    Ok(log.append_and_apply(actor, Action::PurgeFromAlbum { album_id, file_id }, now_ms())?)
}

/// Gives `user_id` a role on the album, or changes the one they have.
/// Admins and owners can share; only an owner can make someone an owner or
/// change an owner's role, and the last owner can't be demoted.
///
/// Ownership transfer is just `share_album` with `role: Owner` — an
/// explicit logged action, per server.md, rather than a dedicated
/// transfer-ownership variant; the old owner then steps down with
/// `unshare_album` or a role change.
pub fn share_album(log: &EventLog<Server>, actor: UserId, album_id: AlbumId, user_id: UserId, role: AlbumRole) -> Result<Event<UserId, Action>> {
    {
        let mut conn = log.connection()?;
        let actor_role = role_in(&mut conn, album_id, actor)?.ok_or(ServerError::AlbumNotFound)?;
        if !actor_role.can(AlbumRole::Admin) {
            return Err(ServerError::PermissionDenied(AlbumRole::Admin.as_str()));
        }
        let exists: bool = diesel::select(exists(app_user::table.find(user_id.to_string()))).get_result(&mut conn)?;
        if !exists {
            return Err(ServerError::UserNotFound);
        }
        let current = role_in(&mut conn, album_id, user_id)?;
        if (role == AlbumRole::Owner || current == Some(AlbumRole::Owner)) && actor_role != AlbumRole::Owner {
            return Err(ServerError::PermissionDenied(AlbumRole::Owner.as_str()));
        }
        if current == Some(AlbumRole::Owner) && role != AlbumRole::Owner && owner_count(&mut conn, album_id)? == 1 {
            return Err(ServerError::InvalidArgument("an album needs an owner: make someone else owner first".to_string()));
        }
    }

    Ok(log.append_and_apply(actor, Action::ShareAlbum { album_id, user_id, role }, now_ms())?)
}

/// `share_album`, naming the user by username (what people know each other by).
pub fn share_album_with(log: &EventLog<Server>, actor: UserId, album_id: AlbumId, username: &str, role: AlbumRole) -> Result<Event<UserId, Action>> {
    let user_id: Option<String> = {
        let mut conn = log.connection()?;
        app_user::table.filter(app_user::username.eq(username)).select(app_user::user_id).first(&mut conn).optional()?
    };
    let user_id = user_id.ok_or(ServerError::UserNotFound)?.parse().expect("user_id column always holds a valid UserId");
    share_album(log, actor, album_id, user_id, role)
}

/// Takes `user_id` out of the album. Admins and owners can remove others
/// (only an owner can remove an owner); any member can leave. The last owner
/// can do neither: the album would have no one able to manage it (delete it
/// instead).
pub fn unshare_album(log: &EventLog<Server>, actor: UserId, album_id: AlbumId, user_id: UserId) -> Result<Event<UserId, Action>> {
    {
        let mut conn = log.connection()?;
        let actor_role = role_in(&mut conn, album_id, actor)?.ok_or(ServerError::AlbumNotFound)?;
        if user_id != actor && !actor_role.can(AlbumRole::Admin) {
            return Err(ServerError::PermissionDenied(AlbumRole::Admin.as_str()));
        }
        let current = role_in(&mut conn, album_id, user_id)?.ok_or(ServerError::UserNotFound)?;
        if current == AlbumRole::Owner {
            if actor_role != AlbumRole::Owner {
                return Err(ServerError::PermissionDenied(AlbumRole::Owner.as_str()));
            }
            if owner_count(&mut conn, album_id)? == 1 {
                return Err(ServerError::InvalidArgument("an album needs an owner: make someone else owner first, or delete the album".to_string()));
            }
        }
    }

    Ok(log.append_and_apply(actor, Action::UnshareAlbum { album_id, user_id }, now_ms())?)
}

fn role_in(conn: &mut SqliteConnection, album_id: AlbumId, user_id: UserId) -> Result<Option<AlbumRole>> {
    let role: Option<String> = album_member::table.find((album_id.to_string(), user_id.to_string())).select(album_member::role).first(conn).optional()?;
    Ok(role.as_deref().and_then(AlbumRole::parse))
}

fn owner_count(conn: &mut SqliteConnection, album_id: AlbumId) -> Result<i64> {
    Ok(album_member::table
        .filter(album_member::album_id.eq(album_id.to_string()))
        .filter(album_member::role.eq(AlbumRole::Owner.as_str()))
        .count()
        .get_result(conn)?)
}

/// Owner-only: renaming is identity-changing, and keeping it owner-only
/// sidesteps having to decide whose per-owner uniqueness namespace an
/// Admin (who isn't the owner) would be renaming into.
pub fn rename_album(log: &EventLog<Server>, actor: UserId, album_id: AlbumId, name: &str) -> Result<Event<UserId, Action>> {
    {
        let mut conn = log.connection()?;
        require_role(&mut conn, album_id, actor, AlbumRole::Owner)?;

        let taken: bool = diesel::select(exists(
            album::table
                .inner_join(album_member::table.on(album_member::album_id.eq(album::album_id)))
                .filter(album_member::user_id.eq(actor.to_string()))
                .filter(album_member::role.eq(AlbumRole::Owner.as_str()))
                .filter(album::name.eq(name))
                .filter(album::album_id.ne(album_id.to_string())),
        ))
        .get_result(&mut conn)?;
        if taken {
            return Err(ServerError::AlbumNameTaken(name.to_string()));
        }
    }

    Ok(log.append_and_apply(actor, Action::RenameAlbum { album_id, name: name.to_string() }, now_ms())?)
}

/// Any member (Reader included) can hide/unhide an album from their own
/// list — this only ever touches the caller's own `album_member` row, so
/// there's nothing here for a role check to protect.
pub fn hide_album(log: &EventLog<Server>, actor: UserId, album_id: AlbumId) -> Result<Event<UserId, Action>> {
    {
        let mut conn = log.connection()?;
        require_role(&mut conn, album_id, actor, AlbumRole::Reader)?;
    }
    Ok(log.append_and_apply(actor, Action::HideAlbum { album_id }, now_ms())?)
}

pub fn unhide_album(log: &EventLog<Server>, actor: UserId, album_id: AlbumId) -> Result<Event<UserId, Action>> {
    {
        let mut conn = log.connection()?;
        require_role(&mut conn, album_id, actor, AlbumRole::Reader)?;
    }
    Ok(log.append_and_apply(actor, Action::UnhideAlbum { album_id }, now_ms())?)
}

pub fn role_of(log: &EventLog<Server>, album_id: AlbumId, user_id: UserId) -> Result<Option<AlbumRole>> {
    let mut conn = log.connection()?;
    let role: Option<String> = album_member::table
        .find((album_id.to_string(), user_id.to_string()))
        .select(album_member::role)
        .first(&mut conn)
        .optional()?;
    Ok(role.as_deref().and_then(AlbumRole::parse))
}

pub(crate) fn require_role(conn: &mut SqliteConnection, album_id: AlbumId, user_id: UserId, required: AlbumRole) -> Result<()> {
    let role: Option<String> = album_member::table
        .find((album_id.to_string(), user_id.to_string()))
        .select(album_member::role)
        .first(conn)
        .optional()?;

    let role = role.as_deref().and_then(AlbumRole::parse).ok_or(ServerError::AlbumNotFound)?;

    if role.can(required) {
        Ok(())
    } else {
        Err(ServerError::PermissionDenied(required.as_str()))
    }
}
