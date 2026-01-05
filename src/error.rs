use std::ffi::NulError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("config error: {0}")]
    Config(String),

    #[error("auth error: {0}")]
    Auth(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("nix error: {0}")]
    Nix(#[from] nix::Error),

    #[error("nul error: {0}")]
    Nul(#[from] NulError),

    #[error("{0}")]
    Other(String),
}

impl From<&str> for Error {
    fn from(s: &str) -> Self {
        Error::Other(s.to_string())
    }
}

impl From<String> for Error {
    fn from(s: String) -> Self {
        Error::Other(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_str() {
        let err: Error = "test error".into();
        assert_eq!(err.to_string(), "test error");
    }

    #[test]
    fn from_string() {
        let err: Error = String::from("test error").into();
        assert_eq!(err.to_string(), "test error");
    }

    #[test]
    fn config_error_display() {
        let err = Error::Config("bad config".into());
        assert_eq!(err.to_string(), "config error: bad config");
    }

    #[test]
    fn auth_error_display() {
        let err = Error::Auth("bad password".into());
        assert_eq!(err.to_string(), "auth error: bad password");
    }

    #[test]
    fn from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err: Error = io_err.into();
        assert!(err.to_string().contains("file not found"));
    }

    #[test]
    fn from_nul_error() {
        let nul_err = std::ffi::CString::new("test\0string").unwrap_err();
        let err: Error = nul_err.into();
        assert!(err.to_string().contains("nul"));
    }
}
