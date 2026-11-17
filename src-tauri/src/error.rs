use serde::Serialize;

/// Error sent to the UI. `code` is stable and translated by the UI (errors.<code>);
/// `message` is the raw technical text (server message, OS error) shown as-is.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppError {
    pub code: &'static str,
    pub message: String,
    /// MariaDB error number (e.g. 1045) when the server answered.
    pub server_code: Option<u16>,
    /// Extra data for the UI, e.g. the SSH host key fingerprint to confirm.
    pub detail: Option<String>,
}

pub type AppResult<T> = Result<T, AppError>;

impl AppError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), server_code: None, detail: None }
    }
}

impl From<mysql_async::Error> for AppError {
    fn from(e: mysql_async::Error) -> Self {
        use mysql_async::{DriverError, Error, IoError};
        let message = e.to_string();
        match e {
            Error::Server(s) => {
                let code = match s.code {
                    1045 | 1044 | 1698 => "DB_ACCESS_DENIED",
                    1049 => "DB_UNKNOWN_DATABASE",
                    1317 => "QUERY_CANCELLED",
                    // ALGORITHM=INSTANT / LOCK=NONE not possible for this change.
                    1845 | 1846 => "ALTER_ALGORITHM",
                    _ => "DB_SERVER",
                };
                AppError { code, message: s.message, server_code: Some(s.code), detail: None }
            }
            Error::Io(IoError::Tls(_)) => AppError::new("TLS", message),
            // rustls handshake failures (bad certificate…) arrive as InvalidData I/O errors.
            Error::Io(IoError::Io(io)) if io.kind() == std::io::ErrorKind::InvalidData => {
                AppError::new("TLS", message)
            }
            Error::Io(IoError::Io(_)) => AppError::new("DB_UNREACHABLE", message),
            Error::Driver(DriverError::NoClientSslFlagFromServer) => AppError::new("TLS_NOT_SUPPORTED", message),
            Error::Driver(DriverError::UnknownAuthPlugin { .. }) => AppError::new("AUTH_PLUGIN", message),
            _ => AppError::new("DB", message),
        }
    }
}

impl From<keyring::Error> for AppError {
    fn from(e: keyring::Error) -> Self {
        AppError::new("KEYCHAIN", e.to_string())
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError::new("IO", e.to_string())
    }
}
