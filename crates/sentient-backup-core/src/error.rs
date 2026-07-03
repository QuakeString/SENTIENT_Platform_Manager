//! Error type for the backup/restore engine.

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database error: {0}")]
    Db(#[from] tokio_postgres::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("connection failed: {0}")]
    Connect(String),

    #[error("{0}")]
    Msg(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn msg(m: impl Into<String>) -> Self {
        Error::Msg(m.into())
    }
}
