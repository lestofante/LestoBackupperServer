use lestobackupper_schema::protocol::ErrorCode;
use server::ServerError;

/// Everything that can go wrong turning a wire `Request` into a `Response`.
/// Distinct from `server::ServerError`: that's a domain-layer error and
/// knows nothing about wire formats. `InvalidRequest` in particular is a
/// concern that only exists at this boundary — a JSON API had serde parse
/// `UserId`/`ObjectId`/etc. for free; FlatBuffers strings are just strings,
/// so parsing (and rejecting malformed ones) is this crate's job.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error(transparent)]
    Server(#[from] ServerError),

    #[error("malformed {field}: {reason}")]
    InvalidRequest { field: &'static str, reason: String },

    #[error("malformed flatbuffers message: {0}")]
    Planus(#[from] planus::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl ApiError {
    pub fn invalid(field: &'static str, reason: impl ToString) -> Self {
        ApiError::InvalidRequest { field, reason: reason.to_string() }
    }

    /// The `(ErrorCode, message)` pair `dispatch` puts on the wire.
    /// `server::ServerError`'s internal-only variants (Diesel/Pool/
    /// EventLog/PasswordHash) all collapse to `Internal` — a client has no
    /// useful decision to make on those beyond "something broke", same
    /// bucket the earlier JSON API's `http_status` used for its 500s.
    pub fn to_wire(&self) -> (ErrorCode, String) {
        let code = match self {
            ApiError::Server(e) => match e {
                ServerError::InvalidCredentials => ErrorCode::InvalidCredentials,
                ServerError::InvalidToken => ErrorCode::InvalidToken,
                ServerError::PermissionDenied(_) => ErrorCode::PermissionDenied,
                ServerError::AlbumNotFound => ErrorCode::AlbumNotFound,
                ServerError::UserNotFound => ErrorCode::UserNotFound,
                ServerError::UsernameTaken(_) => ErrorCode::UsernameTaken,
                ServerError::AlbumNameTaken(_) => ErrorCode::AlbumNameTaken,
                ServerError::RateLimited => ErrorCode::RateLimited,
                ServerError::OtpRequired => ErrorCode::OtpRequired,
                ServerError::InvalidOtp => ErrorCode::InvalidOtp,
                ServerError::NotTrashed => ErrorCode::NotTrashed,
                ServerError::AccountPending => ErrorCode::AccountPending,
                ServerError::AdminRequired => ErrorCode::AdminRequired,
                ServerError::FileNotFound => ErrorCode::FileNotFound,
                ServerError::WrongOffset { .. } => ErrorCode::WrongOffset,
                ServerError::HashMismatch { .. } => ErrorCode::HashMismatch,
                ServerError::NoThumbnail => ErrorCode::NoThumbnail,
                ServerError::TooLarge { .. } => ErrorCode::TooLarge,
                ServerError::InvalidUpload(_) | ServerError::InvalidArgument(_) => ErrorCode::InvalidRequest,
                ServerError::EventLog(_) | ServerError::Diesel(_) | ServerError::Pool(_) | ServerError::PasswordHash(_) | ServerError::Otp(_) | ServerError::Blob(_) => ErrorCode::Internal,
            },
            ApiError::InvalidRequest { .. } => ErrorCode::InvalidRequest,
            ApiError::Planus(_) | ApiError::Io(_) => ErrorCode::Internal,
        };
        (code, self.to_string())
    }
}
