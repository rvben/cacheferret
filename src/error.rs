use std::path::PathBuf;

use thiserror::Error;

/// Stable error kinds and exit-code mapping exposed through clispec.
#[derive(Debug, Error)]
pub enum Error {
    #[error("{message}")]
    Usage { message: String },

    #[error("invalid input: {message}")]
    InvalidInput { message: String },

    #[error("path is not a readable directory: {path}")]
    InvalidPath { path: PathBuf },

    #[error("confirmation is required before cleaning {count} cache directories")]
    ConfirmationRequired { count: usize },

    #[error("cleanup conflicted with filesystem changes: {message}")]
    Conflict { message: String },

    #[error("filesystem operation failed for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl Error {
    pub fn kind(&self) -> &'static str {
        match self {
            Error::Usage { .. } => "usage",
            Error::InvalidInput { .. } | Error::InvalidPath { .. } => "invalid_input",
            Error::ConfirmationRequired { .. } => "confirmation_required",
            Error::Conflict { .. } => "conflict",
            Error::Io { .. } => "io",
        }
    }

    pub fn hint(&self) -> Option<&'static str> {
        match self {
            Error::Usage { .. } => Some("see `cacheferret --help` or `cacheferret schema`"),
            Error::InvalidInput { .. } => Some("check the accepted values in `cacheferret schema`"),
            Error::InvalidPath { .. } => Some("pass an existing directory with --root"),
            Error::ConfirmationRequired { .. } => Some("re-run with --yes to confirm cleanup"),
            Error::Conflict { .. } => Some("scan again and review the changed targets"),
            Error::Io { .. } => {
                Some("check permissions and whether another process is using the path")
            }
        }
    }

    pub fn exit_code(&self) -> u8 {
        match self {
            Error::InvalidInput { .. } | Error::InvalidPath { .. } => 2,
            Error::Usage { .. } => 3,
            Error::Io { .. } => 4,
            Error::Conflict { .. } => 5,
            Error::ConfirmationRequired { .. } => 6,
        }
    }
}
