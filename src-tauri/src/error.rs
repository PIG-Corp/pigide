use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("db: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("pool: {0}")]
    Pool(#[from] r2d2::Error),

    #[error("http: {0}")]
    Http(#[from] reqwest::Error),

    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    #[error("tauri: {0}")]
    Tauri(#[from] tauri::Error),

    #[error("uuid: {0}")]
    Uuid(#[from] uuid::Error),

    #[error("base64: {0}")]
    Base64(#[from] base64::DecodeError),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("invalid input: {0}")]
    Invalid(String),

    #[error("orchestrator: {0}")]
    Orchestrator(String),

    #[error("agent: {0}")]
    Agent(String),

    #[error("voice: {0}")]
    Voice(String),

    #[error("{0}")]
    Other(String),
}

impl From<anyhow::Error> for Error {
    fn from(e: anyhow::Error) -> Self {
        Error::Other(e.to_string())
    }
}

impl From<Error> for String {
    fn from(e: Error) -> Self {
        e.to_string()
    }
}

impl Error {
    /// A message safe to hand to a remote/untrusted caller (e.g. an MCP
    /// client). App-level variants carry intentional, useful text (a missing
    /// workspace id, a bad argument). Infrastructure variants (`Io`, `Db`,
    /// `Pool`, `Http`, `Json`, `Tauri`) can embed filesystem paths, SQL, or
    /// other internal detail, so those are collapsed to a generic string —
    /// the caller logs the real error separately for operators.
    pub fn client_safe_message(&self) -> String {
        match self {
            Error::NotFound(_)
            | Error::Invalid(_)
            | Error::Orchestrator(_)
            | Error::Agent(_)
            | Error::Voice(_) => self.to_string(),
            Error::Io(_)
            | Error::Db(_)
            | Error::Pool(_)
            | Error::Http(_)
            | Error::Json(_)
            | Error::Tauri(_)
            | Error::Uuid(_)
            | Error::Base64(_)
            | Error::Other(_) => "internal error".to_string(),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_level_errors_pass_through() {
        assert_eq!(
            Error::NotFound("workspace abc".into()).client_safe_message(),
            "not found: workspace abc"
        );
        assert_eq!(
            Error::Invalid("bad arg".into()).client_safe_message(),
            "invalid input: bad arg"
        );
    }

    #[test]
    fn infra_errors_are_redacted() {
        let io = Error::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "/home/secret/path/db.sqlite missing",
        ));
        assert_eq!(io.client_safe_message(), "internal error");
        assert_eq!(
            Error::Other("/etc/passwd detail".into()).client_safe_message(),
            "internal error"
        );
    }
}
