use anyhow::{Result, anyhow};
use serde::de::DeserializeOwned;
use serde_json::Value;

/// A thin wrapper around a serde_json::Value that provides RPC-aware parsing.
/// Distinguishes between requests (have "id" + "method"), responses (have "id"
/// but no "method"), and notifications (have "method" but no "id") based on
/// which JSON fields are present.
#[derive(Debug, Clone)]
pub struct RpcObject(pub Value);

pub type RequestId = u64;

#[derive(Debug, Clone, PartialEq, Eq)]
/// An RPC call, which may be either a notification or a request.
pub enum Call<N, R> {
    /// An id and an RPC Request
    Request(RequestId, R),
    /// An RPC Notification
    Notification(N),
}

impl RpcObject {
    pub fn get_id(&self) -> Option<RequestId> {
        self.0.get("id").and_then(Value::as_u64)
    }

    pub fn is_response(&self) -> bool {
        self.0.get("id").is_some() && self.0.get("method").is_none()
    }

    pub fn into_rpc<N, R>(self) -> Result<Call<N, R>>
    where
        N: DeserializeOwned,
        R: DeserializeOwned,
    {
        let id = self.get_id();
        match id {
            Some(id) => match serde_json::from_value::<R>(self.0) {
                Ok(resp) => Ok(Call::Request(id, resp)),
                Err(err) => Err(anyhow!(err)),
            },
            None => {
                let result = serde_json::from_value::<N>(self.0)?;
                Ok(Call::Notification(result))
            }
        }
    }

    /// Parses a response message, returning Ok(Ok(result)) for success or
    /// Ok(Err(error)) for an error response. Returns Err(String) if the
    /// message is malformed (missing id, or has both/neither result and error).
    pub fn into_response(mut self) -> Result<Result<Value, Value>, String> {
        let _ = self
            .get_id()
            .ok_or_else(|| "Response requires 'id' field.".to_string())?;

        if self.0.get("result").is_some() == self.0.get("error").is_some() {
            return Err("RPC response must contain exactly one of\
                        'error' or 'result' fields."
                .into());
        }
        let result = self.0.as_object_mut().and_then(|obj| obj.remove("result"));

        match result {
            Some(r) => Ok(Ok(r)),
            None => {
                let error = self
                    .0
                    .as_object_mut()
                    .and_then(|obj| obj.remove("error"))
                    .ok_or_else(|| {
                        "RPC response has no 'result' or 'error' field.".to_string()
                    })?;
                Ok(Err(error))
            }
        }
    }
}

impl From<Value> for RpcObject {
    fn from(v: Value) -> RpcObject {
        RpcObject(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn get_id_returns_some_when_present() {
        let obj = RpcObject(json!({"id": 42, "method": "foo"}));
        assert_eq!(obj.get_id(), Some(42));
    }

    #[test]
    fn get_id_returns_none_when_absent() {
        let obj = RpcObject(json!({"method": "foo"}));
        assert_eq!(obj.get_id(), None);
    }

    #[test]
    fn get_id_returns_none_for_non_u64() {
        let obj = RpcObject(json!({"id": "not_a_number"}));
        assert_eq!(obj.get_id(), None);
    }

    #[test]
    fn is_response_true_when_id_without_method() {
        let obj = RpcObject(json!({"id": 1, "result": "ok"}));
        assert!(obj.is_response());
    }

    #[test]
    fn is_response_false_when_id_with_method() {
        let obj = RpcObject(json!({"id": 1, "method": "foo"}));
        assert!(!obj.is_response());
    }

    #[test]
    fn is_response_false_when_no_id() {
        let obj = RpcObject(json!({"method": "foo"}));
        assert!(!obj.is_response());
    }

    #[test]
    fn into_rpc_request_with_id() {
        let obj = RpcObject(json!({"id": 5, "method": "test"}));
        let result: Result<Call<Value, Value>> = obj.into_rpc();
        match result.unwrap() {
            Call::Request(id, _val) => assert_eq!(id, 5),
            Call::Notification(_) => panic!("Expected Request, got Notification"),
        }
    }

    #[test]
    fn into_rpc_notification_without_id() {
        let obj = RpcObject(json!({"method": "test", "params": []}));
        let result: Result<Call<Value, Value>> = obj.into_rpc();
        match result.unwrap() {
            Call::Notification(_) => {} // expected
            Call::Request(_, _) => panic!("Expected Notification, got Request"),
        }
    }

    #[test]
    fn into_response_success() {
        let obj = RpcObject(json!({"id": 1, "result": "hello"}));
        let resp = obj.into_response().unwrap();
        assert_eq!(resp.unwrap(), json!("hello"));
    }

    #[test]
    fn into_response_error() {
        let obj =
            RpcObject(json!({"id": 1, "error": {"code": -1, "message": "fail"}}));
        let resp = obj.into_response().unwrap();
        assert_eq!(resp.unwrap_err(), json!({"code": -1, "message": "fail"}));
    }

    #[test]
    fn into_response_missing_id_is_err() {
        let obj = RpcObject(json!({"result": "hello"}));
        let resp = obj.into_response();
        assert!(resp.is_err());
    }

    #[test]
    fn into_response_both_result_and_error_is_err() {
        let obj = RpcObject(json!({"id": 1, "result": "ok", "error": "bad"}));
        let resp = obj.into_response();
        assert!(resp.is_err());
    }

    #[test]
    fn into_response_neither_result_nor_error_is_err() {
        let obj = RpcObject(json!({"id": 1}));
        let resp = obj.into_response();
        assert!(resp.is_err());
    }

    #[test]
    fn from_value() {
        let val = json!({"id": 1});
        let obj: RpcObject = val.clone().into();
        assert_eq!(obj.0, val);
    }
}
