// FlatBuffers encode/decode, shared by both transports (src/tcp.rs frames
// these bytes with a length prefix; src/ws.rs uses one WebSocket binary
// frame per message — either way, this is the actual message).

use lestobackupper_schema::protocol::{Request, RequestBody, RequestRef, Response, ResponseBody};
use planus::ReadAsRoot;

use crate::error::ApiError;

/// Returns `(request_id, token, body)` — the caller correlates the
/// `request_id` back onto the eventual `Response`, and `token` (if
/// present) is what `dispatch` authenticates against.
pub fn decode_request(bytes: &[u8]) -> Result<(u64, Option<String>, RequestBody), ApiError> {
    let req_ref = RequestRef::read_as_root(bytes)?;
    let request_id = req_ref.request_id()?;
    let token = req_ref.token()?.map(str::to_string);
    let body_ref = req_ref.body()?;
    let request: Request = Request { request_id, token, body: body_ref.try_into()? };
    Ok((request.request_id, request.token, request.body))
}

pub fn encode_response(request_id: u64, body: ResponseBody) -> Vec<u8> {
    let mut builder = planus::Builder::new();
    let response = Response { request_id, body };
    builder.finish(&response, None).to_vec()
}

pub fn encode_error(request_id: u64, err: &ApiError) -> Vec<u8> {
    let (code, message) = err.to_wire();
    encode_response(request_id, ResponseBody::Error(Box::new(lestobackupper_schema::protocol::Error { code, message })))
}
