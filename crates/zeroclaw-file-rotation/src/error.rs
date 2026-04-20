use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum RotationError {
    #[error("I/O error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("write channel closed")]
    ChannelClosed,

    #[error("shutdown timeout")]
    ShutdownTimeout,
}

impl RotationError {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

pub type Result<T> = std::result::Result<T, RotationError>;
