use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

use crate::error::Error;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Request {
    CreateSession { username: String },
    PostAuthMessageResponse { response: Option<String> },
    StartSession { cmd: Vec<String>, #[serde(default)] env: Vec<String> },
    CancelSession,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Response {
    Success,
    Error { error_type: ErrorType, description: String },
    AuthMessage { auth_message_type: AuthMessageType, auth_message: String },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorType {
    Error,
    AuthError,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMessageType {
    Visible,
    Secret,
    Info,
    Error,
}

impl Request {
    pub fn read_from(stream: &mut UnixStream) -> Result<Option<Self>, Error> {
        let mut len_buf = [0u8; 4];
        match stream.read_exact(&mut len_buf) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e.into()),
        }

        let len = u32::from_ne_bytes(len_buf) as usize;
        let mut buf = vec![0u8; len];
        stream.read_exact(&mut buf)?;

        let req = serde_json::from_slice(&buf)
            .map_err(|e| Error::Other(format!("failed to parse request: {e}")))?;
        Ok(Some(req))
    }
}

impl Response {
    pub fn write_to(&self, stream: &mut UnixStream) -> Result<(), Error> {
        let json = serde_json::to_vec(self)
            .map_err(|e| Error::Other(format!("failed to serialize response: {e}")))?;

        let len = (json.len() as u32).to_ne_bytes();
        stream.write_all(&len)?;
        stream.write_all(&json)?;
        Ok(())
    }

    pub fn error(msg: impl Into<String>) -> Self {
        Response::Error {
            error_type: ErrorType::Error,
            description: msg.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_create_session_serialization() {
        let req = Request::CreateSession { username: "bob".into() };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"type":"create_session","username":"bob"}"#);
    }

    #[test]
    fn request_post_auth_response_serialization() {
        let req = Request::PostAuthMessageResponse { response: Some("password".into()) };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"type":"post_auth_message_response","response":"password"}"#);
    }

    #[test]
    fn request_post_auth_response_none() {
        let req = Request::PostAuthMessageResponse { response: None };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"type":"post_auth_message_response","response":null}"#);
    }

    #[test]
    fn request_start_session_serialization() {
        let req = Request::StartSession {
            cmd: vec!["sway".into()],
            env: vec!["WAYLAND_DISPLAY=wayland-1".into()],
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"type":"start_session","cmd":["sway"],"env":["WAYLAND_DISPLAY=wayland-1"]}"#);
    }

    #[test]
    fn request_start_session_empty_env() {
        let req = Request::StartSession {
            cmd: vec!["sway".into()],
            env: vec![],
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"type":"start_session","cmd":["sway"],"env":[]}"#);
    }

    #[test]
    fn request_cancel_session_serialization() {
        let req = Request::CancelSession;
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"type":"cancel_session"}"#);
    }

    #[test]
    fn response_success_serialization() {
        let resp = Response::Success;
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(json, r#"{"type":"success"}"#);
    }

    #[test]
    fn response_error_serialization() {
        let resp = Response::Error {
            error_type: ErrorType::Error,
            description: "something went wrong".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(json, r#"{"type":"error","error_type":"error","description":"something went wrong"}"#);
    }

    #[test]
    fn response_auth_error_serialization() {
        let resp = Response::Error {
            error_type: ErrorType::AuthError,
            description: "bad password".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(json, r#"{"type":"error","error_type":"auth_error","description":"bad password"}"#);
    }

    #[test]
    fn response_auth_message_secret() {
        let resp = Response::AuthMessage {
            auth_message_type: AuthMessageType::Secret,
            auth_message: "Password:".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(json, r#"{"type":"auth_message","auth_message_type":"secret","auth_message":"Password:"}"#);
    }

    #[test]
    fn response_auth_message_visible() {
        let resp = Response::AuthMessage {
            auth_message_type: AuthMessageType::Visible,
            auth_message: "Username:".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(json, r#"{"type":"auth_message","auth_message_type":"visible","auth_message":"Username:"}"#);
    }

    #[test]
    fn response_auth_message_info() {
        let resp = Response::AuthMessage {
            auth_message_type: AuthMessageType::Info,
            auth_message: "Welcome!".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(json, r#"{"type":"auth_message","auth_message_type":"info","auth_message":"Welcome!"}"#);
    }

    #[test]
    fn response_auth_message_error() {
        let resp = Response::AuthMessage {
            auth_message_type: AuthMessageType::Error,
            auth_message: "Account locked".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(json, r#"{"type":"auth_message","auth_message_type":"error","auth_message":"Account locked"}"#);
    }

    #[test]
    fn request_deserialization() {
        let json = r#"{"type":"create_session","username":"alice"}"#;
        let req: Request = serde_json::from_str(json).unwrap();
        match req {
            Request::CreateSession { username } => assert_eq!(username, "alice"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn request_start_session_default_env() {
        // env should default to empty vec if not provided
        let json = r#"{"type":"start_session","cmd":["bash"]}"#;
        let req: Request = serde_json::from_str(json).unwrap();
        match req {
            Request::StartSession { cmd, env } => {
                assert_eq!(cmd, vec!["bash"]);
                assert!(env.is_empty());
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn response_error_helper() {
        let resp = Response::error("test error");
        match resp {
            Response::Error { error_type, description } => {
                assert!(matches!(error_type, ErrorType::Error));
                assert_eq!(description, "test error");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn roundtrip_via_stream() {
        use std::os::unix::net::UnixStream;

        let (mut client, mut server) = UnixStream::pair().unwrap();

        // Write response from server
        let resp = Response::AuthMessage {
            auth_message_type: AuthMessageType::Secret,
            auth_message: "Password:".into(),
        };
        resp.write_to(&mut server).unwrap();

        // Read from client side (simulating what a greeter would do)
        let mut len_buf = [0u8; 4];
        std::io::Read::read_exact(&mut client, &mut len_buf).unwrap();
        let len = u32::from_ne_bytes(len_buf) as usize;
        let mut buf = vec![0u8; len];
        std::io::Read::read_exact(&mut client, &mut buf).unwrap();

        let received: Response = serde_json::from_slice(&buf).unwrap();
        match received {
            Response::AuthMessage { auth_message_type, auth_message } => {
                assert!(matches!(auth_message_type, AuthMessageType::Secret));
                assert_eq!(auth_message, "Password:");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn request_read_from_stream() {
        use std::os::unix::net::UnixStream;

        let (mut client, mut server) = UnixStream::pair().unwrap();

        // Simulate greeter sending a request
        let req = Request::CreateSession { username: "testuser".into() };
        let json = serde_json::to_vec(&req).unwrap();
        let len = (json.len() as u32).to_ne_bytes();
        std::io::Write::write_all(&mut client, &len).unwrap();
        std::io::Write::write_all(&mut client, &json).unwrap();

        // Server reads request
        let received = Request::read_from(&mut server).unwrap().unwrap();
        match received {
            Request::CreateSession { username } => assert_eq!(username, "testuser"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn request_read_from_eof() {
        use std::os::unix::net::UnixStream;

        let (client, mut server) = UnixStream::pair().unwrap();
        drop(client); // Close the client side

        let result = Request::read_from(&mut server).unwrap();
        assert!(result.is_none());
    }
}
