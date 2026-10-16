//! IPC protocol between `berry` and the daemon.
//!
//! JSON-RPC 2.0 over a Unix stream socket. The client writes one request as
//! a single line of JSON terminated by `\n`; the daemon answers with one
//! response line and closes the connection.
//!
//! ```text
//! -> {"jsonrpc":"2.0","id":1,"method":"set_limit","params":{"upper":80,"lower":75}}
//! <- {"jsonrpc":"2.0","id":1,"result":{"kind":"ack","message":"..."}}
//! ```

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::battery::BatteryTelemetry;
use crate::capabilities::{Capabilities, ChargeControlMode};
use crate::config::{Config, ConfigOption};
use crate::policy::ChargeState;

pub const JSONRPC_VERSION: &str = "2.0";

/// Upper bound on a single request or response line.
pub const MAX_MESSAGE_BYTES: usize = 256 * 1024;

/// Total time the client waits for a reply.
pub const CLIENT_TIMEOUT: Duration = Duration::from_secs(15);

/// Time the daemon waits for a client to send its request.
pub const SERVER_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Time the daemon waits for a client to accept its reply.
pub const SERVER_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Every method the daemon implements.
pub const METHODS: &[&str] = &[
    "status",
    "doctor",
    "set_limit",
    "disable_limit",
    "set_adapter",
    "set_option",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Call {
    Status,
    Doctor,
    SetLimit { upper: u8, lower: u8 },
    DisableLimit,
    SetAdapter { enabled: bool },
    SetOption { option: ConfigOption, enabled: bool },
}

impl Call {
    pub fn method(&self) -> &'static str {
        match self {
            Call::Status => "status",
            Call::Doctor => "doctor",
            Call::SetLimit { .. } => "set_limit",
            Call::DisableLimit => "disable_limit",
            Call::SetAdapter { .. } => "set_adapter",
            Call::SetOption { .. } => "set_option",
        }
    }

    /// Whether the call changes hardware or configuration and therefore
    /// requires a root peer.
    pub fn requires_root(&self) -> bool {
        !matches!(self, Call::Status | Call::Doctor)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    pub id: u64,
    #[serde(flatten)]
    pub call: Call,
}

impl Request {
    pub fn new(id: u64, call: Call) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id,
            call,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    /// `None` only when the request was too malformed to recover its id.
    pub id: Option<u64>,
    #[serde(flatten)]
    pub outcome: Outcome,
}

impl Response {
    pub fn result(id: u64, reply: Reply) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id: Some(id),
            outcome: Outcome::Result(reply),
        }
    }

    pub fn error(id: Option<u64>, error: RpcError) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id,
            outcome: Outcome::Error(error),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Result(Reply),
    Error(RpcError),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reply {
    Status(Box<StatusReport>),
    Diagnostics(Box<DaemonDiagnostics>),
    Ack { message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    ParseError,
    InvalidRequest,
    MethodNotFound,
    InvalidParams,
    Internal,
    /// The peer lacks the privileges the call requires.
    Unauthorized,
    /// The hardware cannot perform the requested operation.
    Unsupported,
    /// A hardware operation failed.
    Hardware,
    Timeout,
}

impl ErrorKind {
    /// JSON-RPC error code. Standard codes for protocol errors, and the
    /// implementation-defined range -32000..=-32099 for application errors.
    pub fn code(self) -> i32 {
        match self {
            ErrorKind::ParseError => -32700,
            ErrorKind::InvalidRequest => -32600,
            ErrorKind::MethodNotFound => -32601,
            ErrorKind::InvalidParams => -32602,
            ErrorKind::Internal => -32603,
            ErrorKind::Unauthorized => -32001,
            ErrorKind::Unsupported => -32002,
            ErrorKind::Hardware => -32003,
            ErrorKind::Timeout => -32004,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorData {
    pub kind: ErrorKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    pub data: ErrorData,
}

impl RpcError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            code: kind.code(),
            message: message.into(),
            data: ErrorData { kind },
        }
    }

    pub fn kind(&self) -> ErrorKind {
        self.data.kind
    }
}

/// Decodes one request line. On failure, returns the error response to
/// send back, carrying the request id when it can be recovered.
pub fn decode_request(line: &[u8]) -> Result<Request, Box<Response>> {
    let fail =
        |id, kind, message: String| Box::new(Response::error(id, RpcError::new(kind, message)));

    let value: Value = serde_json::from_slice(line)
        .map_err(|e| fail(None, ErrorKind::ParseError, format!("invalid JSON: {e}")))?;
    let Some(object) = value.as_object() else {
        return Err(fail(
            None,
            ErrorKind::InvalidRequest,
            "request must be a JSON object".into(),
        ));
    };
    let id = object.get("id").and_then(Value::as_u64);
    if object.get("jsonrpc").and_then(Value::as_str) != Some(JSONRPC_VERSION) {
        return Err(fail(
            id,
            ErrorKind::InvalidRequest,
            "\"jsonrpc\" must be \"2.0\"".into(),
        ));
    }
    if id.is_none() {
        return Err(fail(
            None,
            ErrorKind::InvalidRequest,
            "\"id\" must be a non-negative integer".into(),
        ));
    }
    match object.get("method").and_then(Value::as_str) {
        None => {
            return Err(fail(
                id,
                ErrorKind::InvalidRequest,
                "\"method\" must be a string".into(),
            ))
        }
        Some(method) if !METHODS.contains(&method) => {
            return Err(fail(
                id,
                ErrorKind::MethodNotFound,
                format!("unknown method {method:?}"),
            ))
        }
        Some(_) => {}
    }
    serde_json::from_value(value)
        .map_err(|e| fail(id, ErrorKind::InvalidParams, format!("invalid params: {e}")))
}

/// Encodes a message as a single newline-terminated JSON line.
pub fn encode_line<T: Serialize>(message: &T) -> serde_json::Result<Vec<u8>> {
    let mut line = serde_json::to_vec(message)?;
    line.push(b'\n');
    Ok(line)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChargingStatus {
    /// Whether the battery may charge, from hardware read-back where
    /// possible. `None` when berry cannot control or observe it.
    pub charging_allowed: Option<bool>,
    /// Hysteresis state; `None` when no limit is configured.
    pub policy_state: Option<ChargeState>,
    /// Whether the adapter is in use, from hardware read-back. `None` when
    /// adapter control is unsupported.
    pub adapter_enabled: Option<bool>,
    /// Most recent hardware error, if the last evaluation hit one.
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusReport {
    pub version: String,
    pub battery: Option<BatteryTelemetry>,
    pub battery_error: Option<String>,
    pub charging: ChargingStatus,
    pub config: Config,
    /// Set when `/etc/berry.json` could not be loaded and defaults are in use.
    pub config_warning: Option<String>,
    pub mode: ChargeControlMode,
    pub capabilities: Capabilities,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaemonDiagnostics {
    pub version: String,
    pub pid: u32,
    pub euid: u32,
    pub socket_path: String,
    pub config_path: String,
    pub config_warning: Option<String>,
    pub mode: ChargeControlMode,
    pub capabilities: Capabilities,
    pub last_error: Option<String>,
}
