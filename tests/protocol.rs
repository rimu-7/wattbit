use berry::config::ConfigOption;
use berry::protocol::{
    decode_request, encode_line, Call, ErrorKind, Outcome, Reply, Request, Response, RpcError,
    METHODS,
};
use serde_json::{json, Value};

fn all_calls() -> Vec<Call> {
    vec![
        Call::Status,
        Call::Doctor,
        Call::SetLimit {
            upper: 80,
            lower: 75,
        },
        Call::DisableLimit,
        Call::SetAdapter { enabled: false },
        Call::SetOption {
            option: ConfigOption::MagsafeLed,
            enabled: true,
        },
    ]
}

#[test]
fn requests_round_trip_through_the_wire_format() {
    for (id, call) in all_calls().into_iter().enumerate() {
        let request = Request::new(id as u64, call);
        let line = encode_line(&request).unwrap();
        assert_eq!(line.last(), Some(&b'\n'));
        assert_eq!(line.iter().filter(|b| **b == b'\n').count(), 1);
        let decoded = decode_request(&line[..line.len() - 1]).unwrap();
        assert_eq!(decoded, request);
    }
}

#[test]
fn every_call_has_a_registered_method() {
    for call in all_calls() {
        assert!(METHODS.contains(&call.method()), "{}", call.method());
        let encoded = serde_json::to_value(Request::new(1, call.clone())).unwrap();
        assert_eq!(encoded["method"], call.method());
    }
}

#[test]
fn wire_format_is_json_rpc() {
    let value = serde_json::to_value(Request::new(
        7,
        Call::SetLimit {
            upper: 80,
            lower: 75,
        },
    ))
    .unwrap();
    assert_eq!(
        value,
        json!({"jsonrpc": "2.0", "id": 7, "method": "set_limit", "params": {"upper": 80, "lower": 75}})
    );

    let value = serde_json::to_value(Request::new(1, Call::Status)).unwrap();
    assert_eq!(
        value,
        json!({"jsonrpc": "2.0", "id": 1, "method": "status"})
    );

    let value = serde_json::to_value(Request::new(
        2,
        Call::SetOption {
            option: ConfigOption::PreventSleepAdapterDisabled,
            enabled: false,
        },
    ))
    .unwrap();
    assert_eq!(value["params"]["option"], "prevent-sleep-adapter-disabled");
}

#[test]
fn responses_round_trip() {
    let ok = Response::result(
        3,
        Reply::Ack {
            message: "done".into(),
        },
    );
    let value = serde_json::to_value(&ok).unwrap();
    assert_eq!(
        value,
        json!({"jsonrpc": "2.0", "id": 3, "result": {"kind": "ack", "message": "done"}})
    );
    assert_eq!(serde_json::from_value::<Response>(value).unwrap(), ok);

    let err = Response::error(Some(4), RpcError::new(ErrorKind::Unsupported, "no CHTE"));
    let value = serde_json::to_value(&err).unwrap();
    assert_eq!(
        value,
        json!({"jsonrpc": "2.0", "id": 4, "error": {"code": -32002, "message": "no CHTE", "data": {"kind": "unsupported"}}})
    );
    assert_eq!(serde_json::from_value::<Response>(value).unwrap(), err);
}

fn decode_error(line: &str) -> (Option<u64>, ErrorKind) {
    let response = decode_request(line.as_bytes()).unwrap_err();
    match response.outcome {
        Outcome::Error(e) => (response.id, e.kind()),
        Outcome::Result(_) => panic!("expected an error"),
    }
}

#[test]
fn malformed_requests_map_to_json_rpc_errors() {
    assert_eq!(decode_error("{nope"), (None, ErrorKind::ParseError));
    assert_eq!(decode_error("[1,2]"), (None, ErrorKind::InvalidRequest));
    assert_eq!(
        decode_error(r#"{"jsonrpc":"1.0","id":1,"method":"status"}"#),
        (Some(1), ErrorKind::InvalidRequest)
    );
    assert_eq!(
        decode_error(r#"{"jsonrpc":"2.0","method":"status"}"#),
        (None, ErrorKind::InvalidRequest)
    );
    assert_eq!(
        decode_error(r#"{"jsonrpc":"2.0","id":-5,"method":"status"}"#),
        (None, ErrorKind::InvalidRequest)
    );
    assert_eq!(
        decode_error(r#"{"jsonrpc":"2.0","id":2,"method":"reboot"}"#),
        (Some(2), ErrorKind::MethodNotFound)
    );
    assert_eq!(
        decode_error(r#"{"jsonrpc":"2.0","id":3,"method":"set_limit","params":{"upper":"high"}}"#),
        (Some(3), ErrorKind::InvalidParams)
    );
    assert_eq!(
        decode_error(r#"{"jsonrpc":"2.0","id":4,"method":"set_adapter"}"#),
        (Some(4), ErrorKind::InvalidParams)
    );
}

#[test]
fn only_read_calls_are_unprivileged() {
    for call in all_calls() {
        let read_only = matches!(call, Call::Status | Call::Doctor);
        assert_eq!(call.requires_root(), !read_only, "{}", call.method());
    }
}

#[test]
fn error_codes_follow_json_rpc_ranges() {
    use ErrorKind::*;
    for kind in [
        ParseError,
        InvalidRequest,
        MethodNotFound,
        InvalidParams,
        Internal,
    ] {
        assert!((-32768..=-32600).contains(&kind.code()), "{kind:?}");
    }
    let application = [Unauthorized, Unsupported, Hardware, Timeout, Conflict];
    for kind in application {
        assert!((-32099..=-32000).contains(&kind.code()), "{kind:?}");
    }
    let mut codes: Vec<i32> = application.iter().map(|k| k.code()).collect();
    codes.dedup();
    assert_eq!(
        codes.len(),
        application.len(),
        "application codes are distinct"
    );
    assert_eq!(Conflict.code(), -32005);
    let parsed: Value = serde_json::to_value(RpcError::new(Hardware, "x")).unwrap();
    assert_eq!(parsed["data"]["kind"], "hardware");
}
