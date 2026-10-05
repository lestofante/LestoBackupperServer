// Diesel table definitions for the domain projection tables created by
// migrations/00000000000000_initial. Distinct from chronicler's own
// `event`/`seq_counter` tables, which it keeps private to itself.

diesel::table! {
    app_user (user_id) {
        user_id -> Text,
        username -> Text,
        created_at -> BigInt,
        is_admin -> Bool,
        status -> Text,
    }
}

diesel::table! {
    credential (user_id) {
        user_id -> Text,
        password_hash -> Text,
        otp_secret -> Nullable<Text>,
    }
}

diesel::table! {
    session (token) {
        token -> Text,
        user_id -> Text,
        issued_at -> BigInt,
        expires_at -> Nullable<BigInt>,
        device_label -> Text,
    }
}

diesel::table! {
    file (id) {
        id -> Text,
        size -> BigInt,
        mime -> Text,
        width -> Nullable<Integer>,
        height -> Nullable<Integer>,
        duration -> Nullable<Integer>,
        previous_version -> Nullable<Text>,
        name -> Nullable<Text>,
        taken_at -> Nullable<BigInt>,
    }
}

diesel::table! {
    file_hash (algo, digest) {
        file_id -> Text,
        algo -> Text,
        digest -> Text,
    }
}

diesel::table! {
    user_file (user_id, file_id) {
        user_id -> Text,
        file_id -> Text,
        added_at -> BigInt,
        source -> Text,
    }
}

diesel::table! {
    album (album_id) {
        album_id -> Text,
        name -> Text,
        kind -> Text,
    }
}

diesel::table! {
    album_member (album_id, user_id) {
        album_id -> Text,
        user_id -> Text,
        role -> Text,
        hidden -> Bool,
    }
}

diesel::table! {
    album_file (album_id, file_id) {
        album_id -> Text,
        file_id -> Text,
        added_at -> BigInt,
        added_by -> Text,
        trashed_at -> Nullable<BigInt>,
        trashed_by -> Nullable<Text>,
    }
}

// Only pairs actually joined in commands/ need to be listed here.
diesel::table! {
    blob_check (algo, digest) {
        algo -> Text,
        digest -> Text,
        checked_at -> BigInt,
        status -> Text,
    }
}

diesel::table! {
    blob_gc (algo, digest) {
        algo -> Text,
        digest -> Text,
        unreferenced_since -> BigInt,
        reclaimed_at -> Nullable<BigInt>,
    }
}

diesel::allow_tables_to_appear_in_same_query!(app_user, credential);
diesel::allow_tables_to_appear_in_same_query!(blob_gc, file_hash);
diesel::allow_tables_to_appear_in_same_query!(blob_gc, file);
diesel::allow_tables_to_appear_in_same_query!(blob_gc, blob_check);
diesel::allow_tables_to_appear_in_same_query!(blob_check, file_hash);
diesel::allow_tables_to_appear_in_same_query!(blob_check, file);
diesel::allow_tables_to_appear_in_same_query!(album, album_member);
diesel::allow_tables_to_appear_in_same_query!(album_file, album_member);
diesel::allow_tables_to_appear_in_same_query!(album_member, app_user);
diesel::allow_tables_to_appear_in_same_query!(album, album_file);
diesel::allow_tables_to_appear_in_same_query!(file, file_hash);
diesel::allow_tables_to_appear_in_same_query!(file_hash, user_file);
diesel::allow_tables_to_appear_in_same_query!(file, user_file);
diesel::allow_tables_to_appear_in_same_query!(file, album_file);
