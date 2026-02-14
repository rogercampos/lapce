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
                    .unwrap();
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
