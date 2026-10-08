use serde::{Deserialize, Serialize};
use std::io::{Read, Write};

pub const REQUEST_LIMIT: usize = 64 * 1024;
pub const OUTPUT_LIMIT: usize = 1024 * 1024;
pub const STDERR_LIMIT: usize = 64 * 1024;

#[derive(Debug, Serialize, Deserialize)]
pub enum PickerRequest {
    Open {
        files: bool,
        directories: bool,
        multiple: bool,
        prompt: Option<String>,
    },
    Save {
        directory: Vec<u8>,
        suggested_name: Option<String>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub enum PickerEvent {
    Ready,
    Selected(Vec<Vec<u8>>),
    Cancelled,
    Failed(String),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RequestEnvelope<T> {
    pub version: u32,
    pub request: T,
}

pub(super) fn encode_request(request: &PickerRequest) -> Result<Vec<u8>, String> {
    let bytes = serde_json::to_vec(&RequestEnvelope {
        version: 1,
        request,
    })
    .map_err(|error| format!("picker request protocol: {error}"))?;
    if bytes.len() > REQUEST_LIMIT {
        return Err("picker request limit exceeded".into());
    }
    Ok(bytes)
}

pub(super) fn read_request(input: impl Read) -> Result<PickerRequest, String> {
    let mut bytes = Vec::new();
    input
        .take(REQUEST_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("picker request read: {error}"))?;
    if bytes.len() > REQUEST_LIMIT {
        return Err("picker request limit exceeded".into());
    }
    let envelope: RequestEnvelope<PickerRequest> = serde_json::from_slice(&bytes)
        .map_err(|error| format!("picker request protocol: {error}"))?;
    if envelope.version != 1 {
        return Err("picker request version unsupported".into());
    }
    match &envelope.request {
        PickerRequest::Open {
            files,
            directories,
            prompt,
            ..
        } => {
            if !files && !directories {
                return Err("picker options: nothing is selectable".into());
            }
            if prompt.as_ref().is_some_and(|text| text.contains('\0')) {
                return Err("picker options: invalid prompt".into());
            }
        }
        PickerRequest::Save {
            directory,
            suggested_name,
        } => {
            if directory.first() != Some(&b'/') || directory.contains(&0) {
                return Err("picker directory must be an absolute non-NUL path".into());
            }
            if suggested_name
                .as_ref()
                .is_some_and(|name| name.contains('\0') || name.contains('/'))
            {
                return Err("picker options: invalid suggested name".into());
            }
        }
    }
    Ok(envelope.request)
}

pub(super) fn emit_event(event: &PickerEvent) -> Result<(), String> {
    let mut bytes =
        serde_json::to_vec(event).map_err(|error| format!("picker event protocol: {error}"))?;
    bytes.push(b'\n');
    if bytes.len() > OUTPUT_LIMIT {
        return Err("picker stdout limit exceeded".into());
    }
    let mut output = std::io::stdout().lock();
    output
        .write_all(&bytes)
        .and_then(|_| output.flush())
        .map_err(|error| format!("picker event write: {error}"))
}
