pub mod domain;
pub mod protocol;

mod client;
mod profile_lease;

pub use client::SidecarClient;
pub use domain::*;
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
        for (kind, expected_message) in [
            (SidecarErrorKind::SpawnFailed, "无法启动兼容组件"),
            (SidecarErrorKind::Timeout, "兼容组件响应超时"),
            (SidecarErrorKind::SidecarExited, "兼容组件已退出"),
            (SidecarErrorKind::ProtocolMismatch, "兼容协议版本不匹配"),
            (SidecarErrorKind::InvalidResponse, "兼容组件响应无效"),
            (SidecarErrorKind::FrameTooLarge, "兼容组件响应过大"),
            (SidecarErrorKind::Io, "兼容组件通信失败"),
            (SidecarErrorKind::ProfileInUse, "资料库正在被使用"),
            (SidecarErrorKind::ProfileLockRequired, "资料库写入租约无效"),
            (SidecarErrorKind::ProfileInvalid, "资料库路径无效"),
            (
                SidecarErrorKind::ProfileNotOwned,
                "资料库不属于 Joplin Lite",
            ),
            (SidecarErrorKind::ProfileAlreadyOpen, "资料库已经打开"),
            (SidecarErrorKind::ProfileNotOpen, "资料库尚未打开"),
            (SidecarErrorKind::ProfileOpenFailed, "无法打开资料库"),
            (SidecarErrorKind::StorageError, "无法保存资料库"),
            (SidecarErrorKind::NotFound, "项目不存在"),
            (SidecarErrorKind::ValidationFailed, "输入内容无效"),
            (SidecarErrorKind::Conflict, "项目已被其他操作修改"),
            (SidecarErrorKind::ImportInvalid, "JEX 文件无效"),
            (SidecarErrorKind::ImportBusy, "JEX 导入正在进行"),
            (SidecarErrorKind::ImportFailed, "JEX 导入失败"),
        ] {
            let error = SidecarError::new(kind, secret);
            assert_eq!(error.message(), expected_message);
            assert_eq!(error.to_string(), expected_message);
            assert!(!error.to_string().contains(secret));
            assert!(!error.message().contains(secret));
        }
    }
}
