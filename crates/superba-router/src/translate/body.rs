//! Transport adapter: a selected native response decoded to IR and encoded
//! into the client's protocol. It is only constructed for cross-protocol legs.

use super::ir::Protocol;
use super::{Invalid, stream::Transcoder};
use crate::forward::{UpBody, UpResponse};
use crate::state::BoxError;
use bytes::Bytes;
use http::{Response, StatusCode};
use http_body::{Body, Frame};
use http_body_util::{BodyExt, Limited};
use serde_json::Value;
use std::pin::Pin;
use std::task::{Context, Poll};

const MAX_JSON: usize = 64 * 1024 * 1024;

struct TranslateBody {
    inner: UpBody,
    codec: Transcoder,
    done: bool,
}

impl Body for TranslateBody {
    type Data = Bytes;
    type Error = BoxError;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        if self.done {
            return Poll::Ready(None);
        }
        // Cooperatively yield after many empty/ping frames.
        for _ in 0..32 {
            let bytes = match Pin::new(&mut self.inner).poll_frame(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Err(e))) => {
                    self.done = true;
                    return Poll::Ready(Some(Err(e)));
                }
                Poll::Ready(Some(Ok(frame))) => match frame.into_data() {
                    Ok(bytes) => self.codec.push(&bytes),
                    Err(_) => continue,
                },
                Poll::Ready(None) => {
                    self.done = true;
                    self.codec.finish()
                }
            };
            match bytes {
                Ok(bytes) if !bytes.is_empty() => {
                    return Poll::Ready(Some(Ok(Frame::data(Bytes::from(bytes)))));
                }
                Ok(_) if self.done => return Poll::Ready(None),
                Ok(_) => {}
                Err(e) => {
                    self.done = true;
                    return Poll::Ready(Some(Err(Box::new(e))));
                }
            }
        }
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

/// Convert a successful response or a structured upstream error. Native
/// replies never call this function and keep their original bytes/headers.
pub(crate) async fn response(
    resp: UpResponse,
    source: Protocol,
    target: Protocol,
) -> Result<UpResponse, Invalid> {
    response_with_tools(resp, source, target, &[]).await
}

/// Restore the requesting client's native function identities and formats.
pub(crate) async fn response_with_tools(
    resp: UpResponse,
    source: Protocol,
    target: Protocol,
    tools: &[super::ir::Tool],
) -> Result<UpResponse, Invalid> {
    let (mut parts, body) = resp.into_parts();
    let sse = parts
        .headers
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| s.starts_with("text/event-stream"));
    if parts
        .headers
        .get(http::header::CONTENT_ENCODING)
        .is_some_and(|v| v != "identity")
    {
        return Err(Invalid::new(
            "encoded upstream response cannot be translated",
        ));
    }
    for key in [
        http::header::CONTENT_LENGTH,
        http::header::CONTENT_ENCODING,
        http::header::ETAG,
    ] {
        parts.headers.remove(key);
    }
    if sse && parts.status.is_success() {
        parts.headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("text/event-stream"),
        );
        return Ok(Response::from_parts(
            parts,
            UpBody::translated(
                TranslateBody {
                    inner: body,
                    codec: Transcoder::new(source, target).with_response_tools(tools),
                    done: false,
                }
                .boxed_unsync(),
            ),
        ));
    }
    let bytes = Limited::new(body, MAX_JSON)
        .collect()
        .await
        .map_err(|_| {
            Invalid::new("upstream JSON response could not be read within the translation limit")
        })?
        .to_bytes();
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| Invalid::new("upstream response is not JSON"))?;
    let mut converted = if parts.status.is_success() {
        super::encode_response(target, &super::decode_response(source, &value)?)?
    } else {
        super::encode_error(target, &super::decode_error(parts.status.as_u16(), &value))
    };
    if target == Protocol::Responses {
        super::responses::Bindings::new(tools).response(&mut converted)?;
    }
    parts.headers.insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static("application/json"),
    );
    Ok(Response::from_parts(
        parts,
        UpBody::replay(Bytes::from(converted.to_string())),
    ))
}

/// Local codec failures use the client's native error shape.
pub(crate) fn error(
    protocol: Protocol,
    status: StatusCode,
    error: &Invalid,
) -> Response<crate::state::RespBody> {
    let value = super::encode_error(
        protocol,
        &super::ir::ApiError {
            status: status.as_u16(),
            code: "protocol_conversion_error".into(),
            message: error.to_string(),
        },
    );
    let mut response = Response::new(crate::forward::full(Bytes::from(value.to_string())));
    *response.status_mut() = status;
    response.headers_mut().insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static("application/json"),
    );
    response
}

/// Encode one request for a selected upstream, preserving the client model in
/// attribution while using the resolved upstream model in the wire request.
pub(crate) fn request(
    source: Protocol,
    target: Protocol,
    value: &Value,
    model: &str,
) -> Result<Bytes, Invalid> {
    request_with_tools(source, target, value, model).map(|(bytes, _)| bytes)
}

/// Return the normalized tool definitions alongside the encoded request.
pub(crate) fn request_with_tools(
    source: Protocol,
    target: Protocol,
    value: &Value,
    model: &str,
) -> Result<(Bytes, Vec<super::ir::Tool>), Invalid> {
    let mut r = super::decode_request(source, value)?;
    r.model = model.to_owned();
    let v = super::encode_request(target, &r)?;
    Ok((Bytes::from(v.to_string()), r.tools))
}
