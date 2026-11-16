//! Error type for the backup/restore engine.

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database error: {}", db_detail(.0))]
    Db(#[from] tokio_postgres::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("connection failed: {0}")]
    Connect(String),

    #[error("{0}")]
    Msg(String),

    /// The free-space watch stopped the restore (crate::space): a disk it
    /// writes to fell under the safe level. The message names the disk.
    #[error("{0}")]
    SpaceLow(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn msg(m: impl Into<String>) -> Self {
        Error::Msg(m.into())
    }
}

/// PostgreSQL's own message (tokio_postgres says only "db error"), with its
/// detail and hint when it gives them.
fn db_detail(e: &tokio_postgres::Error) -> String {
    match e.as_db_error() {
        Some(d) => {
            let mut s = d.message().to_string();
            if let Some(x) = d.detail() {
                s.push_str(&format!(" ({x})"));
            }
            if let Some(h) = d.hint() {
                s.push_str(&format!(" Hint: {h}"));
            }
            s
        }
        None => e.to_string(),
    }
}
