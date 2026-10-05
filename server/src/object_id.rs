use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

/// A self-describing content hash: `sha2-256:e3b0c442...` (core.md). Wrapped
/// so an untagged digest can't be constructed — the tag is what keeps old
/// events identifiable across a future hash-algorithm migration.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ObjectId {
    algo: String,
    digest: String,
}

impl ObjectId {
    pub fn new(algo: impl Into<String>, digest: impl Into<String>) -> Self {
        Self {
            algo: algo.into(),
            digest: digest.into(),
        }
    }

    /// The hash function this project currently ingests with (core.md).
    pub fn sha2_256(digest_hex: impl Into<String>) -> Self {
        Self::new("sha2-256", digest_hex)
    }

    pub fn algo(&self) -> &str {
        &self.algo
    }

    pub fn digest(&self) -> &str {
        &self.digest
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.algo, self.digest)
    }
}

#[derive(Debug, Error)]
pub enum ObjectIdParseError {
    #[error("missing \"algo:digest\" tag in {0:?}")]
    MissingTag(String),
}

impl FromStr for ObjectId {
    type Err = ObjectIdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (algo, digest) = s.split_once(':').ok_or_else(|| ObjectIdParseError::MissingTag(s.to_string()))?;
        Ok(Self::new(algo, digest))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_display_and_from_str() {
        let id = ObjectId::sha2_256("e3b0c44298fc1c14".to_string());
        let parsed: ObjectId = id.to_string().parse().unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn rejects_an_untagged_digest() {
        assert!("e3b0c44298fc1c14".parse::<ObjectId>().is_err());
    }
}
