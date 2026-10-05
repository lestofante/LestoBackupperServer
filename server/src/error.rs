use thiserror::Error;

#[derive(Debug, Error)]
pub enum ServerError {
    #[error(transparent)]
    EventLog(#[from] chronicler::EventLogError),

    #[error("database error: {0}")]
    Diesel(#[from] diesel::result::Error),

    #[error("connection pool error: {0}")]
    Pool(#[from] r2d2::Error),

    #[error("failed to hash password: {0}")]
    PasswordHash(String),

    #[error("username {0:?} is already taken")]
    UsernameTaken(String),

    #[error("owner already has an album named {0:?}")]
    AlbumNameTaken(String),

    #[error("album not found")]
    AlbumNotFound,

    /// No user by that name or id (or, unsharing, not a member of the album).
    #[error("no such user")]
    UserNotFound,

    #[error("file is not in the trash for this album")]
    NotTrashed,

    #[error("user does not have the {0:?} role or higher on this album")]
    PermissionDenied(&'static str),

    #[error("invalid username or password")]
    InvalidCredentials,

    #[error("invalid or expired session token")]
    InvalidToken,

    #[error("this account requires a one-time code")]
    OtpRequired,

    #[error("invalid or expired one-time code")]
    InvalidOtp,

    #[error("TOTP error: {0}")]
    Otp(String),

    #[error("too many login attempts, try again in a moment")]
    RateLimited,

    #[error("this account is pending admin approval")]
    AccountPending,

    #[error("admin privileges required")]
    AdminRequired,

    #[error("blob storage error: {0}")]
    Blob(#[from] std::io::Error),

    #[error("file not found, or you don't have access to it")]
    FileNotFound,

    /// A chunk that doesn't continue the upload where the server has it
    /// (a stale client, or two devices uploading the same file at once).
    /// The client should ask again with BeginUpload and resume from there.
    #[error("upload is at byte {expected}, but the chunk starts at {got}")]
    WrongOffset { expected: u64, got: u64 },

    /// The received bytes aren't the file the client said it was sending
    /// (it changed on the client mid-upload). The partial upload is dropped.
    #[error("received bytes hash to {actual}, not the declared {declared}")]
    HashMismatch { declared: String, actual: String },

    #[error("invalid upload: {0}")]
    InvalidUpload(String),

    /// Too large to send in one message; download it in ranges instead.
    #[error("file is {size} bytes, too large to send whole: download it in ranges")]
    TooLarge { size: u64 },

    /// A thumbnail was asked for, but none can be made of this file and the
    /// original is too large to stand in for one.
    #[error("no thumbnail available for this file")]
    NoThumbnail,

    /// A request argument that doesn't make sense (a malformed page cursor, ...).
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
}

pub type Result<T> = std::result::Result<T, ServerError>;
