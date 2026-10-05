use chronicler::Event;
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;

use crate::action::Action;
use crate::error::Result;
use crate::ids::UserId;
use crate::schema::{app_user, credential};

pub fn apply(conn: &mut SqliteConnection, ev: &Event<UserId, Action>) -> Result<()> {
    match &ev.action {
        Action::CreateUser { user_id, username, password_hash, is_admin, status } => {
            diesel::insert_into(app_user::table)
                .values((
                    app_user::user_id.eq(user_id.to_string()),
                    app_user::username.eq(username),
                    app_user::created_at.eq(ev.at),
                    app_user::is_admin.eq(is_admin),
                    app_user::status.eq(status.as_str()),
                ))
                .execute(conn)?;

            diesel::insert_into(credential::table)
                .values((credential::user_id.eq(user_id.to_string()), credential::password_hash.eq(password_hash)))
                .execute(conn)?;
        }

        Action::ApproveUser { user_id } => {
            diesel::update(app_user::table.find(user_id.to_string())).set(app_user::status.eq(crate::action::AccountStatus::Active.as_str())).execute(conn)?;
        }

        Action::ChangePassword { user_id, password_hash } => {
            diesel::update(credential::table.find(user_id.to_string())).set(credential::password_hash.eq(password_hash)).execute(conn)?;
        }

        _ => {}
    }
    Ok(())
}
