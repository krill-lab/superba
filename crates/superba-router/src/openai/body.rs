//! Decode incoming request bodies before inspecting or shaping them. Both
//! OpenAI protocols send identity bodies upstream, so no encoding can bypass
//! private-field removal or leave a stale Content-Encoding header behind.

use std::io::Read;

use bytes::Bytes;
use http::header::{CONTENT_ENCODING, CONTENT_LENGTH};
use http::{HeaderMap, Response, StatusCode};

use crate::forward::router_error;
use crate::state::RespBody;

/// Decode one supported content coding, bounded by the request body's limit.
/// Unknown, stacked and non-text encodings are refused before forwarding.
pub(crate) fn decode(
    headers: &mut HeaderMap,
    body: Bytes,
    limit: usize,
) -> Result<Bytes, Box<Response<RespBody>>> {
    let unsupported = || {
        Box::new(router_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "bad_request",
            "request bodies require a single content encoding: identity, gzip, deflate, br or zstd",
        ))
    };
    let mut values = headers.get_all(CONTENT_ENCODING).iter();
    let encoding = match values.next() {
        None => "identity".to_owned(),
        Some(v) => v
            .to_str()
            .map_err(|_| unsupported())?
            .trim()
            .to_ascii_lowercase(),
    };
    if values.next().is_some() {
        return Err(unsupported());
    }
    let bad_encoding = || {
        Box::new(router_error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            &format!("the {encoding} request body could not be decoded"),
        ))
    };
    let decoded = match encoding.as_str() {
        "identity" => body,
        "gzip" | "deflate" | "br" | "zstd" => {
            let decoder: Box<dyn Read + '_> = match encoding.as_str() {
                "gzip" => Box::new(flate2::read::MultiGzDecoder::new(body.as_ref())),
                "deflate" => Box::new(flate2::read::ZlibDecoder::new(body.as_ref())),
                "br" => Box::new(brotli::Decompressor::new(body.as_ref(), 4096)),
                _ => Box::new(
                    zstd::stream::read::Decoder::new(body.as_ref()).map_err(|_| bad_encoding())?,
                ),
            };
            let mut out = Vec::new();
            decoder
                .take(u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1))
                .read_to_end(&mut out)
                .map_err(|_| bad_encoding())?;
            Bytes::from(out)
        }
        _ => return Err(unsupported()),
    };
    if decoded.len() > limit {
        return Err(Box::new(router_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request_too_large",
            &format!("the decoded request body exceeds {limit} bytes"),
        )));
    }
    headers.remove(CONTENT_ENCODING);
    headers.remove(CONTENT_LENGTH);
    Ok(decoded)
}
