use std::{
    io::{self, BufRead, Write},
    thread,
};

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::{RpcError, RpcMessage, RpcObject};

/// Sets up bidirectional JSON-over-newline-delimited transport on stdin/stdout
/// (or any Read/Write pair). Spawns two threads:
/// - Writer thread: serializes outgoing RpcMessages as single-line JSON
/// - Reader thread: reads newline-delimited JSON and deserializes into RpcMessages
///
/// The two type parameter sets (Req1/Notif1/Resp1 vs Req2/Notif2/Resp2) allow
/// the reader and writer to use different message types, since UI->Proxy and
/// Proxy->UI have different request/notification schemas.
pub fn stdio_transport<W, R, Req1, Notif1, Resp1, Req2, Notif2, Resp2>(
    mut writer: W,
    writer_receiver: Receiver<RpcMessage<Req2, Notif2, Resp2>>,
    mut reader: R,
    reader_sender: Sender<RpcMessage<Req1, Notif1, Resp1>>,
) where
    W: 'static + Write + Send,
    R: 'static + BufRead + Send,
    Req1: 'static + Serialize + DeserializeOwned + Send + Sync,
    Notif1: 'static + Serialize + DeserializeOwned + Send + Sync,
    Resp1: 'static + Serialize + DeserializeOwned + Send + Sync,
    Req2: 'static + Serialize + DeserializeOwned + Send + Sync,
    Notif2: 'static + Serialize + DeserializeOwned + Send + Sync,
    Resp2: 'static + Serialize + DeserializeOwned + Send + Sync,
{
    thread::spawn(move || {
        for value in writer_receiver {
            if write_msg(&mut writer, value).is_err() {
                return;
            };
        }
    });
    thread::spawn(move || -> Result<()> {
        loop {
            if let Some(msg) = read_msg(&mut reader)? {
                reader_sender.send(msg)?;
            }
        }
    });
}

pub fn write_msg<W, Req, Notif, Resp>(
    out: &mut W,
    msg: RpcMessage<Req, Notif, Resp>,
) -> io::Result<()>
where
    W: Write,
    Req: Serialize,
    Notif: Serialize,
    Resp: Serialize,
{
    let value = match msg {
        RpcMessage::Request(id, req) => {
            let mut msg = serde_json::to_value(&req)?;
            msg.as_object_mut()
                .ok_or(io::ErrorKind::NotFound)?
                .insert("id".into(), id.into());
            msg
        }
        RpcMessage::Response(id, resp) => {
            json!({
                "id": id,
                "result": resp,
            })
        }
        RpcMessage::Notification(n) => serde_json::to_value(n)?,
        RpcMessage::Error(id, err) => {
            json!({
                "id": id,
                "error": err,
            })
        }
    };
    let msg = format!("{}\n", serde_json::to_string(&value)?);
    out.write_all(msg.as_bytes())?;
    out.flush()?;
    Ok(())
}

pub fn read_msg<R, Req, Notif, Resp>(
    inp: &mut R,
) -> io::Result<Option<RpcMessage<Req, Notif, Resp>>>
where
    R: BufRead,
    Req: DeserializeOwned,
    Notif: DeserializeOwned,
    Resp: DeserializeOwned,
{
    let mut buf = String::new();
    let _ = inp.read_line(&mut buf)?;
    let value: Value = serde_json::from_str(&buf)?;

    match parse_value(value) {
        Ok(msg) => Ok(Some(msg)),
        Err(e) => {
            tracing::error!("receive rpc from stdio error: {e:#}");
            Ok(None)
        }
    }
}

fn parse_value<Req, Notif, Resp>(
    value: Value,
) -> io::Result<RpcMessage<Req, Notif, Resp>>
where
    Req: DeserializeOwned,
    Notif: DeserializeOwned,
    Resp: DeserializeOwned,
{
    let object = RpcObject(value);
    let is_response = object.is_response();
    let msg = if is_response {
        let id = object.get_id().ok_or(io::ErrorKind::NotFound)?;
        let resp = object
            .into_response()
            .map_err(|_| io::ErrorKind::NotFound)?;
        match resp {
            Ok(value) => {
                let resp: Resp = serde_json::from_value(value)?;
                RpcMessage::Response(id, resp)
            }
            Err(value) => {
                let err: RpcError = serde_json::from_value(value)?;
                RpcMessage::Error(id, err)
            }
        }
    } else {
        match object.get_id() {
            Some(id) => {
                let req: Req = serde_json::from_value(object.0)?;
                RpcMessage::Request(id, req)
            }
            None => {
                let notif: Notif = serde_json::from_value(object.0)?;
                RpcMessage::Notification(notif)
            }
        }
    };
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    use serde::{Deserialize, Serialize};

    // Simple types for testing round-trips
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct TestReq {
        method: String,
        params: String,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct TestNotif {
        method: String,
        data: String,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct TestResp {
        value: String,
    }

    fn write_and_read<Req, Notif, Resp>(
        msg: RpcMessage<Req, Notif, Resp>,
    ) -> RpcMessage<Req, Notif, Resp>
    where
        Req: Serialize + DeserializeOwned + std::fmt::Debug,
        Notif: Serialize + DeserializeOwned + std::fmt::Debug,
        Resp: Serialize + DeserializeOwned + std::fmt::Debug,
    {
        let mut buf = Vec::new();
        write_msg(&mut buf, msg).unwrap();
        let mut cursor = Cursor::new(buf);
        read_msg(&mut cursor).unwrap().unwrap()
    }

    #[test]
    fn roundtrip_request() {
        let req = TestReq {
            method: "test".into(),
            params: "hello".into(),
        };
        let msg: RpcMessage<TestReq, TestNotif, TestResp> =
            RpcMessage::Request(42, req.clone());

        match write_and_read(msg) {
            RpcMessage::Request(id, r) => {
                assert_eq!(id, 42);
                assert_eq!(r.method, req.method);
                assert_eq!(r.params, req.params);
            }
            other => panic!("Expected Request, got {:?}", other),
        }
    }

    #[test]
    fn roundtrip_notification() {
        let notif = TestNotif {
            method: "notify".into(),
            data: "world".into(),
        };
        let msg: RpcMessage<TestReq, TestNotif, TestResp> =
            RpcMessage::Notification(notif.clone());

        match write_and_read(msg) {
            RpcMessage::Notification(n) => {
                assert_eq!(n.method, notif.method);
                assert_eq!(n.data, notif.data);
            }
            other => panic!("Expected Notification, got {:?}", other),
        }
    }

    #[test]
    fn roundtrip_response() {
        let resp = TestResp {
            value: "result".into(),
        };
        let msg: RpcMessage<TestReq, TestNotif, TestResp> =
            RpcMessage::Response(7, resp.clone());

        match write_and_read(msg) {
            RpcMessage::Response(id, r) => {
                assert_eq!(id, 7);
                assert_eq!(r.value, resp.value);
            }
            other => panic!("Expected Response, got {:?}", other),
        }
    }

    #[test]
    fn roundtrip_error() {
        let err = RpcError::new("something went wrong");
        let msg: RpcMessage<TestReq, TestNotif, TestResp> =
            RpcMessage::Error(99, err);

        match write_and_read(msg) {
            RpcMessage::Error(id, e) => {
                assert_eq!(id, 99);
                assert_eq!(e.message, "something went wrong");
            }
            other => panic!("Expected Error, got {:?}", other),
        }
    }

    #[test]
    fn write_msg_produces_newline_terminated_json() {
        let msg: RpcMessage<TestReq, TestNotif, TestResp> =
            RpcMessage::Response(1, TestResp { value: "x".into() });
        let mut buf = Vec::new();
        write_msg(&mut buf, msg).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.ends_with('\n'));
        // Should be valid JSON without the trailing newline
        let _: Value = serde_json::from_str(s.trim()).unwrap();
    }

    #[test]
    fn read_msg_invalid_json_returns_err() {
        let input = b"not valid json\n";
        let mut cursor = Cursor::new(&input[..]);
        let result: io::Result<Option<RpcMessage<TestReq, TestNotif, TestResp>>> =
            read_msg(&mut cursor);
        assert!(result.is_err());
    }

    #[test]
    fn read_msg_empty_input_returns_err() {
        let input = b"";
        let mut cursor = Cursor::new(&input[..]);
        let result: io::Result<Option<RpcMessage<TestReq, TestNotif, TestResp>>> =
            read_msg(&mut cursor);
        // Empty string fails JSON parse
        assert!(result.is_err());
    }

    #[test]
    fn read_msg_malformed_rpc_returns_none() {
        // Valid JSON but not a valid RPC message: response-shaped (id, no method)
        // but missing both "result" and "error" fields → parse_value fails → Ok(None)
        let input = b"{\"id\": 1}\n";
        let mut cursor = Cursor::new(&input[..]);
        let result: io::Result<Option<RpcMessage<TestReq, TestNotif, TestResp>>> =
            read_msg(&mut cursor);
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn read_msg_response_with_both_result_and_error_returns_none() {
        // Response with both "result" and "error" → into_response returns Err → Ok(None)
        let input = b"{\"id\": 1, \"result\": \"ok\", \"error\": {\"code\": 0, \"message\": \"fail\"}}\n";
        let mut cursor = Cursor::new(&input[..]);
        let result: io::Result<Option<RpcMessage<TestReq, TestNotif, TestResp>>> =
            read_msg(&mut cursor);
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn read_msg_request_with_wrong_schema_returns_none() {
        // Has "id" and "method" (so it's a request), but fields don't match TestReq
        let input = b"{\"id\": 1, \"method\": \"test\", \"wrong_field\": true}\n";
        let mut cursor = Cursor::new(&input[..]);
        let result: io::Result<Option<RpcMessage<TestReq, TestNotif, TestResp>>> =
            read_msg(&mut cursor);
        // serde deserialization into TestReq fails because "params" is missing
        // parse_value returns Err → read_msg returns Ok(None)
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn parse_value_response_without_id_returns_err() {
        // Response-shaped JSON: no "method", has "result", but no "id"
        // Actually this won't be classified as response since is_response checks
        // id.is_some, so it becomes a notification parse attempt
        let value = json!({"result": "hello"});
        let result: io::Result<RpcMessage<TestReq, TestNotif, TestResp>> =
            parse_value(value);
        // No id + no method-like fields = tries to deserialize as Notif, fails
        assert!(result.is_err());
    }
}
