use chronicler::Event;
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;

use crate::action::Action;
use crate::error::Result;
use crate::ids::UserId;
use crate::schema::{file, file_hash, user_file};

pub fn apply(conn: &mut SqliteConnection, ev: &Event<UserId, Action>) -> Result<()> {
    match &ev.action {
        Action::UploadFile { file_id, hash, size, mime, width, height, duration, previous_version, source, name, taken_at } => {
            // `file` is pure content, keyed by the id the command resolved
            // (new, or the existing owner of this hash on dedup) — first
            // writer sets the metadata, later ones are the same bytes so
            // it must already agree.
            diesel::insert_into(file::table)
                .values((
                    file::id.eq(file_id.to_string()),
                    file::size.eq(size),
                    file::mime.eq(mime),
                    file::width.eq(width),
                    file::height.eq(height),
                    file::duration.eq(duration),
                    file::previous_version.eq(previous_version.map(|id| id.to_string())),
                    file::name.eq(name),
                    file::taken_at.eq(taken_at),
                ))
                .on_conflict(file::id)
                .do_nothing()
                .execute(conn)?;

            // N rows per file (core.md tagged hashes): a future algorithm
            // adds a row, never replaces one.
            diesel::insert_into(file_hash::table)
                .values((file_hash::file_id.eq(file_id.to_string()), file_hash::algo.eq(hash.algo()), file_hash::digest.eq(hash.digest())))
                .on_conflict((file_hash::algo, file_hash::digest))
                .do_nothing()
                .execute(conn)?;

            // The attribution record — "All Photos" is this table, not an
            // album row (server.md).
            diesel::insert_into(user_file::table)
                .values((
                    user_file::user_id.eq(ev.actor.to_string()),
                    user_file::file_id.eq(file_id.to_string()),
                    user_file::added_at.eq(ev.at),
                    user_file::source.eq(source.as_str()),
                ))
                .on_conflict((user_file::user_id, user_file::file_id))
                .do_nothing()
                .execute(conn)?;
        }

        Action::RemoveFromLibrary { file_id } => {
            diesel::delete(user_file::table.find((ev.actor.to_string(), file_id.to_string()))).execute(conn)?;
        }

        Action::RenameFile { file_id, name } => {
            diesel::update(file::table.find(file_id.to_string())).set(file::name.eq(name)).execute(conn)?;
        }

        _ => {}
    }

    Ok(())
}
