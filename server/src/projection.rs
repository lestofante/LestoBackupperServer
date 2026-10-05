use chronicler::{Event, Projection};
use diesel::sqlite::SqliteConnection;
use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};

use crate::action::{self, Action};
use crate::domain::{album, file, user};
use crate::error::ServerError;
use crate::ids::UserId;

const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");

/// The chronicler [`Projection`] for this domain: files, albums, users.
/// Knows nothing about HTTP, blob storage on disk, or replication — those
/// are separate layers on top of the rebuildable state this produces.
pub struct Server;

impl Projection for Server {
    type Actor = UserId;
    type Action = Action;

    fn migrate(conn: &mut SqliteConnection) -> chronicler::Result<()> {
        conn.run_pending_migrations(MIGRATIONS)
            .map(|_| ())
            .map_err(chronicler::EventLogError::Migration)
    }

    fn apply(conn: &mut SqliteConnection, ev: &Event<Self::Actor, Self::Action>) -> chronicler::Result<()> {
        let result = match &ev.action {
            Action::CreateUser { .. } | Action::ApproveUser { .. } | Action::ChangePassword { .. } => user::apply(conn, ev),
            Action::UploadFile { .. } | Action::RenameFile { .. } | Action::RemoveFromLibrary { .. } => file::apply(conn, ev),
            Action::CreateAlbum { .. }
            | Action::AddToAlbum { .. }
            | Action::TrashFromAlbum { .. }
            | Action::RestoreFromTrash { .. }
            | Action::PurgeFromAlbum { .. }
            | Action::ShareAlbum { .. }
            | Action::UnshareAlbum { .. }
            | Action::RenameAlbum { .. }
            | Action::HideAlbum { .. }
            | Action::UnhideAlbum { .. }
            | Action::DeleteAlbum { .. } => album::apply(conn, ev),
        };
        result.map_err(to_chronicler_error)
    }

    fn action_type(action: &Self::Action) -> &'static str {
        action::action_type(action)
    }
}

// `Projection::apply` returns `chronicler::Result`, but the domain modules
// return `crate::error::Result` so they can also surface Diesel errors
// under our own error type when called from commands. Diesel errors are
// the only variant that can actually occur inside apply(); anything else
// here is a bug.
fn to_chronicler_error(e: ServerError) -> chronicler::EventLogError {
    match e {
        ServerError::EventLog(e) => e,
        ServerError::Diesel(e) => chronicler::EventLogError::Diesel(e),
        other => unreachable!("apply() cannot produce {other:?}"),
    }
}
