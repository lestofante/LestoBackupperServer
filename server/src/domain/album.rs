use chronicler::Event;
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;

use crate::action::{Action, AlbumRole};
use crate::error::Result;
use crate::ids::UserId;
use crate::schema::{album, album_file, album_member};

pub fn apply(conn: &mut SqliteConnection, ev: &Event<UserId, Action>) -> Result<()> {
    match &ev.action {
        Action::CreateAlbum { album_id, owner, name, kind } => {
            diesel::insert_into(album::table)
                .values((album::album_id.eq(album_id.to_string()), album::name.eq(name), album::kind.eq(kind.as_str())))
                .execute(conn)?;

            // Exactly one owner, plus admin for membership management
            // (server.md) — creation grants the owner role directly.
            diesel::insert_into(album_member::table)
                .values((
                    album_member::album_id.eq(album_id.to_string()),
                    album_member::user_id.eq(owner.to_string()),
                    album_member::role.eq(AlbumRole::Owner.as_str()),
                ))
                .execute(conn)?;
        }

        Action::AddToAlbum { album_id, file_id, added_by } => {
            diesel::insert_into(album_file::table)
                .values((
                    album_file::album_id.eq(album_id.to_string()),
                    album_file::file_id.eq(file_id.to_string()),
                    album_file::added_at.eq(ev.at),
                    album_file::added_by.eq(added_by.to_string()),
                ))
                .on_conflict((album_file::album_id, album_file::file_id))
                .do_nothing()
                .execute(conn)?;
        }

        Action::TrashFromAlbum { album_id, file_id } => {
            diesel::update(album_file::table.find((album_id.to_string(), file_id.to_string())))
                .set((album_file::trashed_at.eq(Some(ev.at)), album_file::trashed_by.eq(Some(ev.actor.to_string()))))
                .execute(conn)?;
        }

        Action::RestoreFromTrash { album_id, file_id } => {
            diesel::update(album_file::table.find((album_id.to_string(), file_id.to_string())))
                .set((album_file::trashed_at.eq(None::<i64>), album_file::trashed_by.eq(None::<String>)))
                .execute(conn)?;
        }

        Action::PurgeFromAlbum { album_id, file_id } => {
            diesel::delete(album_file::table.find((album_id.to_string(), file_id.to_string()))).execute(conn)?;
        }

        // Sharing is inserting a row; re-sharing an existing member updates
        // their role in place rather than requiring a separate action
        // (server.md: "sharing is inserting a row, unsharing is deleting
        // one" — no is_shared flag, no branching).
        Action::ShareAlbum { album_id, user_id, role } => {
            diesel::insert_into(album_member::table)
                .values((
                    album_member::album_id.eq(album_id.to_string()),
                    album_member::user_id.eq(user_id.to_string()),
                    album_member::role.eq(role.as_str()),
                ))
                .on_conflict((album_member::album_id, album_member::user_id))
                .do_update()
                .set(album_member::role.eq(role.as_str()))
                .execute(conn)?;
        }

        Action::UnshareAlbum { album_id, user_id } => {
            diesel::delete(album_member::table.find((album_id.to_string(), user_id.to_string()))).execute(conn)?;
        }

        Action::RenameAlbum { album_id, name } => {
            diesel::update(album::table.find(album_id.to_string())).set(album::name.eq(name)).execute(conn)?;
        }

        Action::HideAlbum { album_id } => {
            diesel::update(album_member::table.find((album_id.to_string(), ev.actor.to_string()))).set(album_member::hidden.eq(true)).execute(conn)?;
        }

        Action::UnhideAlbum { album_id } => {
            diesel::update(album_member::table.find((album_id.to_string(), ev.actor.to_string()))).set(album_member::hidden.eq(false)).execute(conn)?;
        }

        Action::DeleteAlbum { album_id } => {
            let id = album_id.to_string();
            diesel::delete(album_file::table.filter(album_file::album_id.eq(&id))).execute(conn)?;
            diesel::delete(album_member::table.filter(album_member::album_id.eq(&id))).execute(conn)?;
            diesel::delete(album::table.find(&id)).execute(conn)?;
        }
        Action::CreateUser { .. }
        | Action::ApproveUser { .. }
        | Action::ChangePassword { .. }
        | Action::UploadFile { .. }
        | Action::RenameFile { .. }
        | Action::RemoveFromLibrary { .. } => {}
    }
    Ok(())
}
