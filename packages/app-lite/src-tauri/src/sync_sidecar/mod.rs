pub mod protocol;

mod client;

pub use client::SidecarClient;
pub use protocol::{
    DEFAULT_REQUEST_TIMEOUT, MAX_FRAME_BYTES, PROTOCOL_VERSION, STARTUP_TIMEOUT, SidecarCommand,
    SidecarError, SidecarErrorKind, SidecarState,
};

#[cfg(test)]
mod protocol_tests {
    use super::protocol::*;

    #[test]
    fn response_success_and_failure_deserialize() {
        let success =
            decode_response(r#"{"id":"request-1","ok":true,"result":{"protocolVersion":1}}"#)
                .expect("success response");
        assert_eq!(success.id(), "request-1");
        let failure = decode_response(
            r#"{"id":"request-2","ok":false,"error":{"code":"INVALID_REQUEST","message":"请求格式无效"}}"#,
        )
        .expect("failure response");
        assert_eq!(failure.id(), "request-2");
    }

    #[test]
    fn mismatched_response_id_is_rejected() {
        let response =
            decode_response(r#"{"id":"other","ok":true,"result":{}}"#).expect("response parses");
        assert!(validate_response_id(&response, "request-1").is_err());
    }

    #[test]
    fn hello_protocol_version_mismatch_is_rejected() {
        let response =
            decode_response(r#"{"id":"request-1","ok":true,"result":{"protocolVersion":2}}"#)
                .expect("response parses");
        assert!(validate_hello(&response).is_err());
    }

    #[test]
    fn public_errors_are_fixed_and_redacted() {
        let secret = "supplied-secret-marker";
        for kind in [
            SidecarErrorKind::SpawnFailed,
            SidecarErrorKind::Timeout,
            SidecarErrorKind::SidecarExited,
            SidecarErrorKind::ProtocolMismatch,
            SidecarErrorKind::InvalidResponse,
            SidecarErrorKind::FrameTooLarge,
            SidecarErrorKind::Io,
        ] {
            let error = SidecarError::new(kind, secret);
            assert!(!error.to_string().contains(secret));
            assert!(!error.message().contains(secret));
        }
    }
}
