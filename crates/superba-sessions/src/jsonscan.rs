//! Byte-exact locator for the `session_meta` provider field of a rollout line.

use std::ops::Range;

/// What [`inspect_session_meta`] found in one JSONL line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MetaLine {
    /// Decoded `payload.id`, when it is a JSON string.
    pub id: Option<String>,
    /// Decoded `payload.timestamp`, when it is a JSON string.
    pub timestamp: Option<String>,
    /// Decoded `payload.model_provider`, when it is a JSON string.
    pub provider: Option<String>,
    /// Byte range (within the line) of the effective `payload.model_provider`
    /// string token, quotes included. `None` when the key is absent or its value is
    /// not a string.
    pub provider_span: Option<Range<usize>>,
    /// The payload has a `model_provider` member (any value type).
    pub provider_present: bool,
    /// Byte range of the effective `model_provider` value when it is a scalar
    /// (string, number, `true`/`false`, `null`); `None` when it is absent or an
    /// object/array. Lets a `null` provider be replaced instead of duplicated.
    pub provider_scalar_span: Option<Range<usize>>,
    /// Byte index (within the line) of the closing `}` of the payload object.
    pub payload_end: usize,
    /// True when the payload object has at least one member.
    pub payload_nonempty: bool,
}

/// Inspect one JSONL line (without its `\n`; a trailing `\r` is allowed).
///
/// The cursor only advances, and skipped values are validated without retaining
/// them. The explicit stack limits nesting to 512 open objects/arrays, including
/// the root object. Numbers are checked against the JSON grammar, without
/// imposing a machine numeric type's range on values that we do not use.
pub(crate) fn inspect_session_meta(line: &[u8]) -> Option<MetaLine> {
    let mut scan = Scanner { line, pos: 0 };
    scan.whitespace();
    scan.expect(b'{')?;
    let mut stack = vec![Frame {
        state: State::ObjectFirst,
        scope: Scope::Root,
    }];
    let mut session_meta = false;
    let mut payload: Option<MetaLine> = None;

    while let Some(frame) = stack.last().copied() {
        scan.whitespace();
        let target = match (frame.state, scan.peek()?) {
            (State::ObjectFirst | State::ObjectAfter, b'}')
            | (State::ArrayFirst | State::ArrayAfter, b']') => {
                if frame.scope == Scope::Payload {
                    payload.as_mut()?.payload_end = scan.pos;
                }
                scan.pos += 1;
                stack.pop();
                continue;
            }
            (State::ObjectAfter, b',') => {
                scan.pos += 1;
                stack.last_mut()?.state = State::ObjectKey;
                continue;
            }
            (State::ArrayAfter, b',') => {
                scan.pos += 1;
                stack.last_mut()?.state = State::ArrayValue;
                continue;
            }
            (State::ObjectAfter | State::ArrayAfter, _) => return None,
            (State::ObjectFirst | State::ObjectKey, _) => {
                let key = scan.string(frame.scope != Scope::Ignore)?;
                scan.whitespace();
                scan.expect(b':')?;
                stack.last_mut()?.state = State::ObjectAfter;
                if frame.scope == Scope::Payload {
                    payload.as_mut()?.payload_nonempty = true;
                }
                match (frame.scope, key.as_deref()) {
                    (Scope::Root, Some("type")) => Target::Type,
                    (Scope::Root, Some("payload")) => Target::Payload,
                    (Scope::Payload, Some("id")) => Target::Id,
                    (Scope::Payload, Some("timestamp")) => Target::Timestamp,
                    (Scope::Payload, Some("model_provider")) => Target::Provider,
                    _ => Target::Ignore,
                }
            }
            (State::ArrayFirst | State::ArrayValue, _) => {
                stack.last_mut()?.state = State::ArrayAfter;
                Target::Ignore
            }
        };

        scan.whitespace();
        let value_start = scan.pos;
        let value = scan.value(matches!(
            target,
            Target::Type | Target::Id | Target::Timestamp | Target::Provider
        ))?;
        let mut scope = Scope::Ignore;
        match target {
            Target::Type => session_meta = value.string.as_deref() == Some("session_meta"),
            Target::Payload => {
                // Replacing the entire result also discards fields from an
                // earlier payload when the last payload is empty or non-object.
                payload = if matches!(value.container, Some(State::ObjectFirst)) {
                    scope = Scope::Payload;
                    Some(MetaLine {
                        id: None,
                        timestamp: None,
                        provider: None,
                        provider_span: None,
                        provider_present: false,
                        provider_scalar_span: None,
                        payload_end: 0,
                        payload_nonempty: false,
                    })
                } else {
                    None
                };
            }
            Target::Id => payload.as_mut()?.id = value.string,
            Target::Timestamp => payload.as_mut()?.timestamp = value.string,
            Target::Provider => {
                let meta = payload.as_mut()?;
                meta.provider = value.string;
                meta.provider_span = value.span;
                meta.provider_present = true;
                meta.provider_scalar_span =
                    value.container.is_none().then_some(value_start..scan.pos);
            }
            Target::Ignore => {}
        }
        if let Some(state) = value.container {
            if stack.len() >= 512 {
                return None;
            }
            stack.push(Frame { state, scope });
        }
    }

    scan.whitespace();
    if session_meta && scan.pos == line.len() {
        payload
    } else {
        None
    }
}

/// Encode `s` as a JSON string token (quotes included).
pub(crate) fn json_string_token(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0'..='\u{1f}' => {
                out.push_str("\\u00");
                let byte = ch as u8;
                for digit in [byte >> 4, byte & 15] {
                    out.push(char::from(if digit < 10 {
                        b'0' + digit
                    } else {
                        b'a' + digit - 10
                    }));
                }
            }
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

#[derive(Clone, Copy)]
struct Frame {
    state: State,
    scope: Scope,
}

#[derive(Clone, Copy)]
enum State {
    ObjectFirst,
    ObjectKey,
    ObjectAfter,
    ArrayFirst,
    ArrayValue,
    ArrayAfter,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Root,
    Payload,
    Ignore,
}

enum Target {
    Type,
    Payload,
    Id,
    Timestamp,
    Provider,
    Ignore,
}

#[derive(Default)]
struct ValueStart {
    string: Option<String>,
    span: Option<Range<usize>>,
    container: Option<State>,
}

struct Scanner<'a> {
    line: &'a [u8],
    pos: usize,
}

impl Scanner<'_> {
    fn peek(&self) -> Option<u8> {
        self.line.get(self.pos).copied()
    }

    fn next(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.pos += 1;
        Some(byte)
    }

    fn expect(&mut self, expected: u8) -> Option<()> {
        (self.next()? == expected).then_some(())
    }

    fn whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.pos += 1;
        }
    }

    fn value(&mut self, capture: bool) -> Option<ValueStart> {
        let mut value = ValueStart::default();
        match self.peek()? {
            b'"' => {
                let start = self.pos;
                value.string = self.string(capture)?;
                if capture {
                    value.span = Some(start..self.pos);
                }
            }
            b'{' => {
                self.pos += 1;
                value.container = Some(State::ObjectFirst);
            }
            b'[' => {
                self.pos += 1;
                value.container = Some(State::ArrayFirst);
            }
            b't' => self.literal(b"true")?,
            b'f' => self.literal(b"false")?,
            b'n' => self.literal(b"null")?,
            b'-' | b'0'..=b'9' => self.number()?,
            _ => return None,
        }
        Some(value)
    }

    fn literal(&mut self, literal: &[u8]) -> Option<()> {
        for &byte in literal {
            self.expect(byte)?;
        }
        Some(())
    }

    fn number(&mut self) -> Option<()> {
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.next()? {
            b'0' => {}
            b'1'..=b'9' => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.pos += 1;
                }
            }
            _ => return None,
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            self.digits()?;
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            self.digits()?;
        }
        // The container state requires a delimiter after every scalar, which
        // also rejects leading zeroes, concatenated literals, and number suffixes.
        Some(())
    }

    fn digits(&mut self) -> Option<()> {
        let start = self.pos;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        (self.pos > start).then_some(())
    }

    // The outer Option reports validity; the inner one is present only when
    // capturing. Skipping a string still validates its UTF-8 and every escape.
    fn string(&mut self, capture: bool) -> Option<Option<String>> {
        self.expect(b'"')?;
        let mut decoded = capture.then(String::new);
        loop {
            let start = self.pos;
            while matches!(self.peek(), Some(byte) if byte >= 0x20 && byte != b'"' && byte != b'\\')
            {
                self.pos += 1;
            }
            let chunk = std::str::from_utf8(self.line.get(start..self.pos)?).ok()?;
            if let Some(out) = decoded.as_mut() {
                out.push_str(chunk);
            }
            match self.next()? {
                b'"' => return Some(decoded),
                b'\\' => {
                    let ch = match self.next()? {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{08}',
                        b'f' => '\u{0c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => self.unicode_escape()?,
                        _ => return None,
                    };
                    if let Some(out) = decoded.as_mut() {
                        out.push(ch);
                    }
                }
                _ => return None,
            }
        }
    }

    fn unicode_escape(&mut self) -> Option<char> {
        let first = self.hex_quad()?;
        let scalar = match first {
            0xd800..=0xdbff => {
                self.expect(b'\\')?;
                self.expect(b'u')?;
                let second = self.hex_quad()?;
                if !(0xdc00..=0xdfff).contains(&second) {
                    return None;
                }
                0x10000 + ((first - 0xd800) << 10) + (second - 0xdc00)
            }
            0xdc00..=0xdfff => return None,
            _ => first,
        };
        char::from_u32(scalar)
    }

    fn hex_quad(&mut self) -> Option<u32> {
        let mut value = 0;
        for _ in 0..4 {
            let digit = match self.next()? {
                byte @ b'0'..=b'9' => byte - b'0',
                byte @ b'a'..=b'f' => byte - b'a' + 10,
                byte @ b'A'..=b'F' => byte - b'A' + 10,
                _ => return None,
            };
            value = (value << 4) | u32::from(digit);
        }
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::{MetaLine, inspect_session_meta, json_string_token};
    use serde_json::{Value, json};

    const REAL_LINE: &str = r#"{"timestamp":"2026-10-16T07:35:26.660Z","ordinal":0,"type":"session_meta","payload":{"session_id":"01a1..","id":"01a143a3-1f0f-7031-a8df-e99d93328ef5","timestamp":"2026-10-16T07:35:26.266Z","cwd":"/x","originator":"codex_exec","cli_version":"0.162.0","source":"exec","model_provider":"krill","base_instructions":{"text":"... 20 KB of text that may itself contain \"model_provider\" or \"type\":\"session_meta\" inside escaped strings ..."},"history_mode":"paginated","context_window":null}}"#;

    // Check both recognition and the meaning of the byte offsets. The insertion
    // check also distinguishes the effective payload's closing brace from any
    // other closing brace in the line.
    fn differential(line: &[u8]) -> bool {
        let original = serde_json::from_slice::<Value>(line).ok();
        let expected = original.as_ref().filter(|value| {
            value.is_object()
                && value.get("type").and_then(Value::as_str) == Some("session_meta")
                && value.get("payload").is_some_and(Value::is_object)
        });
        let actual = inspect_session_meta(line);
        assert_eq!(
            actual.is_some(),
            expected.is_some(),
            "recognition differs: {:?}",
            String::from_utf8_lossy(line)
        );
        let (Some(meta), Some(expected)) = (actual, expected) else {
            return false;
        };
        let payload = expected["payload"].as_object().unwrap();
        for (key, actual_string) in [
            ("id", meta.id.as_deref()),
            ("timestamp", meta.timestamp.as_deref()),
            ("model_provider", meta.provider.as_deref()),
        ] {
            assert_eq!(
                actual_string,
                payload.get(key).and_then(Value::as_str),
                "field {key}: {:?}",
                String::from_utf8_lossy(line)
            );
        }
        assert_eq!(meta.payload_nonempty, !payload.is_empty());
        assert_eq!(
            meta.provider_present,
            payload.contains_key("model_provider")
        );
        assert_eq!(
            meta.provider_scalar_span.is_some(),
            payload
                .get("model_provider")
                .is_some_and(|v| !v.is_object() && !v.is_array())
        );
        if let Some(span) = meta.provider_scalar_span.clone() {
            // Replacing the scalar token with a string sets the provider and
            // changes nothing else.
            let mut edited = line[..span.start].to_vec();
            edited.extend_from_slice(json_string_token("krill").as_bytes());
            edited.extend_from_slice(&line[span.end..]);
            let mut want = expected.clone();
            want["payload"]["model_provider"] = Value::String("krill".into());
            assert_eq!(serde_json::from_slice::<Value>(&edited).unwrap(), want);
        }
        assert_eq!(line.get(meta.payload_end), Some(&b'}'));
        assert_eq!(meta.provider_span.is_some(), meta.provider.is_some());
        if let Some(span) = meta.provider_span {
            assert!(span.start < span.end && span.end <= meta.payload_end);
            assert_eq!(line.get(span.start), Some(&b'"'));
            assert_eq!(line.get(span.end - 1), Some(&b'"'));
            assert_eq!(
                serde_json::from_slice::<String>(&line[span.clone()]).unwrap(),
                meta.provider.unwrap()
            );
            let mut replaced = Vec::new();
            replaced.extend_from_slice(&line[..span.start]);
            replaced.extend_from_slice(json_string_token("openai").as_bytes());
            replaced.extend_from_slice(&line[span.end..]);
            let mut wanted = expected.clone();
            wanted["payload"]["model_provider"] = json!("openai");
            assert_eq!(serde_json::from_slice::<Value>(&replaced).unwrap(), wanted);
            assert_eq!(&replaced[..span.start], &line[..span.start]);
            assert!(replaced.ends_with(&line[span.end..]));
        }
        let mut inserted = line[..meta.payload_end].to_vec();
        if meta.payload_nonempty {
            inserted.push(b',');
        }
        inserted.extend_from_slice(br#""__jsonscan_probe":true"#);
        inserted.extend_from_slice(&line[meta.payload_end..]);
        let mut wanted = expected.clone();
        wanted["payload"]["__jsonscan_probe"] = json!(true);
        assert_eq!(serde_json::from_slice::<Value>(&inserted).unwrap(), wanted);
        true
    }

    #[test]
    fn real_rollout_and_byte_exact_replacement() {
        let meta = inspect_session_meta(REAL_LINE.as_bytes()).unwrap();
        assert_eq!(
            meta.id.as_deref(),
            Some("01a143a3-1f0f-7031-a8df-e99d93328ef5")
        );
        assert_eq!(meta.timestamp.as_deref(), Some("2026-10-16T07:35:26.266Z"));
        assert_eq!(meta.provider.as_deref(), Some("krill"));
        assert_eq!(&REAL_LINE[meta.provider_span.unwrap()], r#""krill""#);
        assert_eq!(meta.payload_end, REAL_LINE.len() - 2);
        assert!(meta.payload_nonempty);
        assert!(differential(REAL_LINE.as_bytes()));
    }

    #[test]
    fn escaped_keys_values_and_unicode_byte_offsets() {
        let line = r#"{"\u0070ayload":{"before":"中文😀","i\u0064":"会话\u002d\uD83D\uDE00","time\u0073tamp":"\u0032\u0030\u0032\u0036","model\u005fprovider":"中😀\u4e2d\uD83D\uDE00\"\\\/\b\f\n\r\t"},"\u0074ype":"session\u005fmeta"}"#;
        let meta = inspect_session_meta(line.as_bytes()).unwrap();
        assert_eq!(meta.id.as_deref(), Some("会话-😀"));
        assert_eq!(meta.timestamp.as_deref(), Some("2026"));
        assert_eq!(
            meta.provider.as_deref(),
            Some("中😀中😀\"\\/\u{08}\u{0c}\n\r\t")
        );
        let span = meta.provider_span.unwrap();
        let expected_token = r#""中😀\u4e2d\uD83D\uDE00\"\\\/\b\f\n\r\t""#;
        assert_eq!(&line.as_bytes()[span.clone()], expected_token.as_bytes());
        assert_eq!(span.start, line.find(expected_token).unwrap());
        assert!(line[..span.start].chars().count() < span.start);
        assert!(differential(line.as_bytes()));
    }

    #[test]
    fn duplicates_replace_all_previous_metadata() {
        let line = r#"{"type":false,"payload":{"id":"discard","timestamp":"discard","model_provider":"discard"},"type":"session_meta","payload":{"id":"old","id":"new","timestamp":"old","timestamp":false,"model_provider":"old","model\u005fprovider":"new"},"type":"session_meta"}"#;
        let meta = inspect_session_meta(line.as_bytes()).unwrap();
        assert_eq!(meta.id.as_deref(), Some("new"));
        assert_eq!(meta.timestamp, None);
        assert_eq!(meta.provider.as_deref(), Some("new"));
        let start = line.rfind(r#""new""#).unwrap();
        assert_eq!(meta.provider_span, Some(start..start + 5));
        assert!(differential(line.as_bytes()));

        let line = br#"{"type":"session_meta","payload":{"id":"old","timestamp":"old","model_provider":"old"},"payload":{}}"#;
        assert_eq!(
            inspect_session_meta(line),
            Some(MetaLine {
                id: None,
                timestamp: None,
                provider: None,
                provider_span: None,
                provider_present: false,
                provider_scalar_span: None,
                payload_end: line.len() - 2,
                payload_nonempty: false,
            })
        );
        assert!(differential(line));
    }

    #[test]
    fn direct_members_only_and_non_string_values_clear_fields() {
        let line = br#"{"type":"session_meta","model_provider":"root","payload":{"id":"old","id":{},"timestamp":"old","timestamp":[],"model_provider":"old","model_provider":null,"base_instructions":{"model_provider":"nested","payload":{"id":"nested"}},"text":"\"model_provider\":\"fake\", \"type\":\"session_meta\", \"payload\":{}","array":[{"model_provider":"nested"}]}}"#;
        let meta = inspect_session_meta(line).unwrap();
        assert_eq!(meta.id, None);
        assert_eq!(meta.timestamp, None);
        assert_eq!(meta.provider, None);
        assert_eq!(meta.provider_span, None);
        // The effective (last) member is `null`: present, with a scalar span.
        assert!(meta.provider_present);
        let span = meta.provider_scalar_span.clone().unwrap();
        assert_eq!(&line[span], b"null");
        assert!(meta.payload_nonempty);
        assert!(differential(line));

        let line = br#"{"type":"session_meta","payload":{"base_instructions":{"model_provider":"nested"},"text":"model_provider"}}"#;
        assert_eq!(inspect_session_meta(line).unwrap().provider, None);
        assert!(differential(line));
    }

    #[test]
    fn json_whitespace_and_empty_payload() {
        let line = b" \t\r\n{ \r\n\"payload\" \t: \r\n{ \t\r\n} \t,\n \"type\" :\t\"session_meta\" \r\n}\r\n\t ";
        let meta = inspect_session_meta(line).unwrap();
        assert!(!meta.payload_nonempty);
        assert_eq!(meta.provider_span, None);
        assert_eq!(
            meta.payload_end,
            line.iter().position(|&b| b == b'}').unwrap()
        );
        assert!(differential(line));
    }

    #[test]
    fn differential_handcrafted_lines() {
        let lines = [
            REAL_LINE,
            r#"{"type":"session_meta","payload":{}}"#,
            r#"{"payload":{},"type":"session_meta"}"#,
            " \r\n{\t\"type\":\"session_meta\" , \"payload\" : { \"id\" : \"a\" } }\r",
            r#"{"\u0074ype":"session_meta","pay\u006coad":{"model\u005fprovider":"a"}}"#,
            r#"{"type":"session\u005fmeta","payload":{"\u0069d":"id","time\u0073tamp":"t"}}"#,
            r#"{"type":"session_meta","payload":{"model_provider":""}}"#,
            r#"{"type":"session_meta","payload":{"model_provider":null}}"#,
            r#"{"type":"session_meta","payload":{"model_provider":false}}"#,
            r#"{"type":"session_meta","payload":{"model_provider":42}}"#,
            r#"{"type":"session_meta","payload":{"model_provider":[]}}"#,
            r#"{"type":"session_meta","payload":{"model_provider":{}}}"#,
            r#"{"type":"session_meta","payload":{"id":null,"timestamp":false}}"#,
            r#"{"type":"session_meta","payload":{"id":[],"timestamp":{}}}"#,
            r#"{"type":"session_meta","payload":{"id":"","timestamp":"","model_provider":""}}"#,
            r#"{"type":"session_meta","payload":{"id":"a","id":7,"timestamp":"a","timestamp":null}}"#,
            r#"{"type":"session_meta","payload":{"id":7,"id":"b","timestamp":null,"timestamp":"b"}}"#,
            r#"{"type":"other","type":"session_meta","payload":{}}"#,
            r#"{"type":"session_meta","type":"other","payload":{}}"#,
            r#"{"type":"session_meta","type":null,"payload":{}}"#,
            r#"{"type":[],"payload":{},"type":"session_meta"}"#,
            r#"{"payload":null,"payload":{},"type":"session_meta"}"#,
            r#"{"payload":{},"payload":null,"type":"session_meta"}"#,
            r#"{"type":"session_meta","payload":{"model_provider":"a"},"payload":{"model_provider":"b"}}"#,
            r#"{"type":"session_meta","payload":{"model_provider":"a"},"payload":{}}"#,
            r#"{"type":"session_meta","payload":{},"pay\u006coad":{"id":"last"}}"#,
            r#"{"type":"session_meta","payload":{"model_provider":"a","model_provider":"b"}}"#,
            r#"{"type":"session_meta","payload":{"model_provider":"a","model_provider":null}}"#,
            r#"{"type":"session_meta","payload":{"model_provider":null,"model_provider":"b"}}"#,
            r#"{"type":"session_meta","payload":{"model_provider":"a","model_provider":{"model_provider":"b"}}}"#,
            r#"{"type":"session_meta","payload":{"nested":{"model_provider":"a"},"text":"\"model_provider\":\"b\""}}"#,
            r#"{"type":"session_meta","payload":{"array":[{"payload":{"model_provider":"a"}}]}}"#,
            r#"{"type":"session_meta","payload":{"type":"other","payload":{"id":"nested"},"id":"direct"}}"#,
            r#"{"type":"session_meta","payload":{},"nested":{"type":"other","payload":null}}"#,
            r#"{"nested":{"type":"session_meta","payload":{}}}"#,
            r#"{"type":"other","payload":{"type":"session_meta","payload":{}}}"#,
            r#"{"type":"session_meta","payload":"{\"model_provider\":\"fake\"}"}"#,
            r#"{"type":"session_meta","payload":{"before":"中文😀","model_provider":"提供者"}}"#,
            r#"{"type":"session_meta","payload":{"before":"\ud83d\ude00","model_provider":"\u4e2d\u6587"}}"#,
            r#"{"type":"session_meta","payload":{"model_provider":"\u0000\u001f\u007f\uffff"}}"#,
            r#"{"type":"session_meta","payload":{"model_provider":"\uD800\uDC00\uDBFF\uDFFF"}}"#,
            r#"{"type":"session_meta","payload":{"model_provider":"\"\\\/\b\f\n\r\t"}}"#,
            r#"{"type":"session_meta","payload":{"x":[0,-0,1,-1,1.0,-0.0,0.123,1e0,1E+2,1e-2,true,false,null]}}"#,
            r#"{"type":"session_meta","payload":{"x":18446744073709551616}}"#,
            r#"{"type":"session_meta","payload":{"x":-9223372036854775809}}"#,
            r#"{"type":"session_meta","payload":{"x":1e-400}}"#,
            r#"{"type":"session_meta","payload":{"x":0e9999999999}}"#,
            r#"{"type":"session_meta","payload":{"":"empty key","x":[{},[],[{}]]}}"#,
            r#"{"type":"session_meta","payload":{},"x":{},"y":[]}"#,
            r#"{"type":"session_meta"}"#,
            r#"{"payload":{}}"#,
            r#"{"type":"session_meta","payload":[]}"#,
            r#"{"type":"session_meta","payload":true}"#,
            r#"{"type":"session_meta","payload":0}"#,
            r#"{"type":"session_meta","payload":""}"#,
            r#"{"type":{},"payload":{}}"#,
            r#"{"type":["session_meta"],"payload":{}}"#,
            r#"{"type":"session_meta ","payload":{}}"#,
            r#"{"Type":"session_meta","payload":{}}"#,
            r#"{"type":"session_meta","Payload":{}}"#,
            r#"{"type":"session_meta\u0000","payload":{}}"#,
            r#"{"type":"session_meta","payload\u0000":{}}"#,
            "",
            " \t\r\n",
            "{}",
            "[]",
            "[{}]",
            r#"[{"type":"session_meta","payload":{}}]"#,
            r#""session_meta""#,
            "null",
            "true",
            "false",
            "42",
            r#"{"type":"session_meta","payload":{}} garbage"#,
            r#"{"type":"session_meta","payload":{}}{}"#,
            r#"{"type":"session_meta","payload":{}} null"#,
            r#"{"type":"session_meta","payload":{}},"#,
            r#"{"type":"session_meta","payload":{}}"#,
            r#"{"type":"session_meta","payload":{}"#,
            r#"{"type":"session_meta","payload":{"#,
            r#"{"type":"session_meta","payload":"#,
            r#"{"type":"session_meta","payload":{,}}"#,
            r#"{"type":"session_meta","payload":{"x":1,}}"#,
            r#"{"type":"session_meta","payload":{},}"#,
            r#"{"type":"session_meta","payload":{} "x":0}"#,
            r#"{"type":"session_meta","payload":{"x" 0}}"#,
            r#"{"type":"session_meta","payload":{x:0}}"#,
            r#"{"type":"session_meta","payload":{'x':0}}"#,
            r#"{"type":"session_meta","payload":{"x":}}"#,
            r#"{"type":"session_meta","payload":{"x":[,]}}"#,
            r#"{"type":"session_meta","payload":{"x":[1,]}}"#,
            r#"{"type":"session_meta","payload":{"x":[1 2]}}"#,
            r#"{"type":"session_meta","payload":{"x":[1}}}"#,
            r#"{"type":"session_meta","payload":{"x":tru}}"#,
            r#"{"type":"session_meta","payload":{"x":falsee}}"#,
            r#"{"type":"session_meta","payload":{"x":Null}}"#,
            r#"{"type":"session_meta","payload":{"x":nulltrue}}"#,
            r#"{"type":"session_meta","payload":{"x":NaN}}"#,
            r#"{"type":"session_meta","payload":{"x":Infinity}}"#,
            r#"{"type":"session_meta","payload":{"x":+1}}"#,
            r#"{"type":"session_meta","payload":{"x":01}}"#,
            r#"{"type":"session_meta","payload":{"x":-01}}"#,
            r#"{"type":"session_meta","payload":{"x":--1}}"#,
            r#"{"type":"session_meta","payload":{"x":-}}"#,
            r#"{"type":"session_meta","payload":{"x":.1}}"#,
            r#"{"type":"session_meta","payload":{"x":1.}}"#,
            r#"{"type":"session_meta","payload":{"x":1e}}"#,
            r#"{"type":"session_meta","payload":{"x":1e+}}"#,
            r#"{"type":"session_meta","payload":{"x":1e-}}"#,
            r#"{"type":"session_meta","payload":{"x":1.e2}}"#,
            r#"{"type":"session_meta","payload":{"x":1e2.0}}"#,
            r#"{"type":"session_meta","payload":{"x":0x10}}"#,
            r#"{"type":"session_meta","payload":{"x":1_000}}"#,
            r#"{"type":"session_meta","payload":{"x":"\x20"}}"#,
            r#"{"type":"session_meta","payload":{"x":"\v"}}"#,
            r#"{"type":"session_meta","payload":{"x":"\u0"}}"#,
            r#"{"type":"session_meta","payload":{"x":"\uZZZZ"}}"#,
            r#"{"type":"session_meta","payload":{"x":"\ud800"}}"#,
            r#"{"type":"session_meta","payload":{"x":"\udc00"}}"#,
            r#"{"type":"session_meta","payload":{"x":"\ud800\ud800"}}"#,
            r#"{"type":"session_meta","payload":{"x":"\udc00\ud800"}}"#,
            r#"{"type":"session_meta","payload":{"x":"\ud800\u0041"}}"#,
            r#"{"type":"session_meta","payload":{"x":"\ud800x"}}"#,
            r#"{"type":"session_meta","payload":{"\udfff":0}}"#,
            r#"{"type":"session_meta","payload":{"x":"\ud800"},"payload":{}}"#,
            r#"{"type":"session_meta","payload":{},"ignored":{"x":"\ud800"}}"#,
            r#"{"type":"session_meta","payload":{} /* comment */}"#,
            "\u{feff}{\"type\":\"session_meta\",\"payload\":{}}",
            "{\"type\":\"session_meta\",\"payload\":{}}\u{00a0}",
            "{\"type\":\"session_meta\",\"payload\":{}}\u{000b}",
            "{\"type\":\"session_meta\",\"payload\":{}}\0",
            "{\"type\":\"session_meta\",\"payload\":{\"x\":\"raw\nnewline\"}}",
        ];
        assert!(lines.len() >= 40);
        for line in lines {
            differential(line.as_bytes());
        }
    }

    #[test]
    fn truncation_at_every_byte_and_trailing_garbage() {
        for end in 0..REAL_LINE.len() {
            assert_eq!(inspect_session_meta(&REAL_LINE.as_bytes()[..end]), None);
        }
        for suffix in [b"x".as_slice(), b"{}", b"\0", b"\xff", b"// comment"] {
            let mut line = REAL_LINE.as_bytes().to_vec();
            line.extend_from_slice(suffix);
            assert_eq!(inspect_session_meta(&line), None);
            differential(&line);
        }
    }

    #[test]
    fn invalid_utf8_controls_and_escapes_are_rejected_even_when_skipped() {
        let bad_utf8: &[&[u8]] = &[
            b"\x80",
            b"\xc0\x80",
            b"\xc2",
            b"\xe2\x82",
            b"\xe0\x80\x80",
            b"\xed\xa0\x80",
            b"\xf0\x80\x80\x80",
            b"\xf4\x90\x80\x80",
            b"\xff",
        ];
        for bytes in bad_utf8 {
            for (prefix, suffix) in [
                (
                    br#"{"type":"session_meta","payload":{"model_provider":""#.as_slice(),
                    br#""}}"#.as_slice(),
                ),
                (
                    br#"{"type":"session_meta","payload":{"ignored":""#,
                    br#""}}"#,
                ),
                (
                    br#"{"type":"session_meta","payload":{"nested":{"ignored":""#,
                    br#""}}}"#,
                ),
                (
                    br#"{"type":"session_meta","payload":{},"ignored":""#,
                    br#""}"#,
                ),
            ] {
                let mut valid = prefix.to_vec();
                valid.extend_from_slice(b"valid");
                valid.extend_from_slice(suffix);
                assert!(differential(&valid));
                let mut line = prefix.to_vec();
                line.extend_from_slice(bytes);
                line.extend_from_slice(suffix);
                assert_eq!(inspect_session_meta(&line), None);
                differential(&line);
            }
            let mut line = br#"{"type":"session_meta","payload":{},""#.to_vec();
            line.extend_from_slice(bytes);
            line.extend_from_slice(br#"":0}"#);
            assert_eq!(inspect_session_meta(&line), None);
            differential(&line);
        }
        for byte in 0..0x20 {
            let mut line = br#"{"type":"session_meta","payload":{"ignored":""#.to_vec();
            line.push(byte);
            line.extend_from_slice(br#""}}"#);
            assert_eq!(inspect_session_meta(&line), None);
            differential(&line);
        }
        for invalid in [
            r"\uD800",
            r"\uDFFF",
            r"\uD800\\uDC00",
            r"\uD800\u0000",
            r"\uD800\UDC00",
            r"\uD800\uDBFF",
            r"\uD800\uDC0x",
            r"\u123",
            r"\U1234",
            r"\q",
            "\\",
        ] {
            for template in [
                r#"{"type":"session_meta","payload":{"ignored":"INVALID"}}"#,
                r#"{"type":"session_meta","payload":{},"ignored":{"INVALID":null}}"#,
                r#"{"type":"session_meta","payload":{"model_provider":"INVALID"}}"#,
            ] {
                let line = template.replace("INVALID", invalid);
                assert_eq!(inspect_session_meta(line.as_bytes()), None, "{line}");
                differential(line.as_bytes());
            }
        }
    }

    fn nested_line(arrays: usize) -> String {
        format!(
            "{{\"type\":\"session_meta\",\"payload\":{{\"x\":{}0{}}}}}",
            "[".repeat(arrays),
            "]".repeat(arrays)
        )
    }

    #[test]
    fn explicit_stack_enforces_depth_without_call_stack_growth() {
        // The root object and payload object account for two levels.
        assert!(inspect_session_meta(nested_line(510).as_bytes()).is_some());
        assert!(inspect_session_meta(nested_line(511).as_bytes()).is_none());
        assert!(inspect_session_meta(nested_line(100_000).as_bytes()).is_none());
        assert!(inspect_session_meta("[".repeat(100_000).as_bytes()).is_none());
        let truncated = format!(
            "{{\"type\":\"session_meta\",\"payload\":{{\"x\":{}",
            "[".repeat(100_000)
        );
        assert!(inspect_session_meta(truncated.as_bytes()).is_none());
        let mixed = format!(
            "{{\"type\":\"session_meta\",\"payload\":{{\"x\":{}0{}}}}}",
            r#"{"x":["#.repeat(100_000),
            "]}".repeat(100_000)
        );
        assert!(inspect_session_meta(mixed.as_bytes()).is_none());

        // The scanner's explicit 512 limit intentionally exceeds serde_json's
        // default recursion limit, so these inputs are outside the oracle test.
        assert!(serde_json::from_str::<Value>(&nested_line(510)).is_err());
    }

    #[test]
    fn unused_numbers_have_json_syntax_without_a_float_range_limit() {
        for number in ["1e400", "-1e400", "1e99999999999999999999"] {
            let line = format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"unused\":{number},\"model_provider\":\"a\"}}}}"
            );
            assert!(inspect_session_meta(line.as_bytes()).is_some());
            // RFC 8259 does not bound exponents. The scanner does not materialize
            // numbers, whereas the default serde_json Value uses finite f64.
            assert!(serde_json::from_str::<Value>(&line).is_err());
        }
    }

    #[test]
    fn large_instruction_string_does_not_hide_the_direct_provider() {
        let text = r#"中😀 \"model_provider\":\"fake\", \"type\":\"session_meta\", payload {} \n "#;
        let instructions = text.repeat(11 * 1024 * 1024 / text.len() + 1);
        let prefix = format!(
            r#"{{"type":"session_meta","payload":{{"base_instructions":{{"text":"{instructions}"}},"model_provider":"#
        );
        let line = format!(r#"{prefix}"krill","id":"large"}}}}"#);
        let meta = inspect_session_meta(line.as_bytes()).unwrap();
        assert!(line.len() > 10 * 1024 * 1024);
        assert_eq!(meta.provider.as_deref(), Some("krill"));
        assert_eq!(meta.provider_span, Some(prefix.len()..prefix.len() + 7));
        assert_eq!(meta.id.as_deref(), Some("large"));
        assert_eq!(meta.payload_end, line.len() - 2);
        assert!(differential(line.as_bytes()));
    }

    #[test]
    fn string_token_round_trips_controls_quotes_backslashes_and_unicode() {
        let controls: String = (0u8..0x20).map(char::from).collect();
        for text in [
            "",
            "plain",
            "\"\\/quotes",
            "中文😀\u{10000}\u{10ffff}",
            "\u{7f}\u{85}\u{2028}\u{2029}",
            controls.as_str(),
            "mixed\n\t\"\\中😀\0",
        ] {
            let token = json_string_token(text);
            assert!(token.starts_with('"') && token.ends_with('"'));
            assert!(!token.bytes().any(|byte| byte < 0x20));
            assert_eq!(serde_json::from_str::<String>(&token).unwrap(), text);
        }
        assert_eq!(
            json_string_token("\0\u{01}\u{08}\u{0c}\n\r\t\u{1f}\"\\"),
            r#""\u0000\u0001\b\f\n\r\t\u001f\"\\""#
        );
    }

    // All generated tokens are independent of the production string encoder.
    // Objects are rendered from member lists to retain duplicate keys.
    struct Generator(u64);

    impl Generator {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn choose(&mut self, count: usize) -> usize {
            (self.next() % count as u64) as usize
        }

        fn whitespace(&mut self) -> &'static str {
            ["", " ", "\t", "\r", "\n", " \t\r\n"][self.choose(6)]
        }

        fn token(&mut self, text: &str) -> String {
            let mut token = String::from("\"");
            for ch in text.chars() {
                if self.choose(4) == 0 {
                    let mut units = [0; 2];
                    for unit in ch.encode_utf16(&mut units) {
                        if self.choose(2) == 0 {
                            token.push_str(&format!("\\u{unit:04x}"));
                        } else {
                            token.push_str(&format!("\\u{unit:04X}"));
                        }
                    }
                } else {
                    match ch {
                        '"' => token.push_str("\\\""),
                        '\\' => token.push_str("\\\\"),
                        '/' => token.push_str("\\/"),
                        '\u{08}' => token.push_str("\\b"),
                        '\u{0c}' => token.push_str("\\f"),
                        '\n' => token.push_str("\\n"),
                        '\r' => token.push_str("\\r"),
                        '\t' => token.push_str("\\t"),
                        '\0'..='\u{1f}' => token.push_str(&format!("\\u{:04x}", ch as u32)),
                        _ => token.push(ch),
                    }
                }
            }
            token.push('"');
            token
        }

        fn string(&mut self) -> String {
            let choices = [
                "",
                "krill",
                "openai",
                "2026-10-16T07:35:26.266Z",
                "01a143a3-1f0f-7031-a8df-e99d93328ef5",
                "中文😀",
                "\0\u{1f}\n\r\t\u{08}\u{0c}",
                "\"model_provider\":\"fake\",\"type\":\"session_meta\"",
                "\\\\\"{}[],:/",
                "\u{10000}\u{10ffff}\u{ffff}",
            ];
            let text = choices[self.choose(choices.len())];
            self.token(text)
        }

        fn scalar(&mut self) -> String {
            let literals = [
                "null",
                "true",
                "false",
                "-0",
                "0",
                "42",
                "-1.23",
                "1E+20",
                "1e-300",
                "18446744073709551616",
            ];
            if self.choose(2) == 0 {
                self.string()
            } else {
                literals[self.choose(literals.len())].to_owned()
            }
        }

        fn object(&mut self, mut members: Vec<(&str, String)>) -> String {
            for i in (1..members.len()).rev() {
                let other = self.choose(i + 1);
                members.swap(i, other);
            }
            let mut out = String::from("{");
            out.push_str(self.whitespace());
            for (i, (key, value)) in members.into_iter().enumerate() {
                if i != 0 {
                    out.push(',');
                    out.push_str(self.whitespace());
                }
                out.push_str(&self.token(key));
                out.push_str(self.whitespace());
                out.push(':');
                out.push_str(self.whitespace());
                out.push_str(&value);
                out.push_str(self.whitespace());
            }
            out.push('}');
            out
        }

        fn value(&mut self, depth: usize) -> String {
            if depth == 0 {
                return self.scalar();
            }
            match self.choose(4) {
                0 => {
                    let keys = ["payload", "type", "model_provider", "id", "timestamp", "x"];
                    let mut members = Vec::new();
                    for _ in 0..self.choose(4) {
                        members.push((keys[self.choose(keys.len())], self.value(depth - 1)));
                    }
                    self.object(members)
                }
                1 => {
                    let mut out = String::from("[");
                    out.push_str(self.whitespace());
                    for i in 0..self.choose(4) {
                        if i != 0 {
                            out.push(',');
                            out.push_str(self.whitespace());
                        }
                        out.push_str(&self.value(depth - 1));
                        out.push_str(self.whitespace());
                    }
                    out.push(']');
                    out
                }
                _ => self.scalar(),
            }
        }

        fn payload(&mut self) -> String {
            let mut members = Vec::new();
            for key in ["id", "timestamp", "model_provider", "base_instructions"] {
                for _ in 0..self.choose(4) {
                    let value = if self.choose(3) == 0 {
                        self.value(3)
                    } else {
                        self.string()
                    };
                    members.push((key, value));
                }
            }
            self.object(members)
        }

        fn line(&mut self) -> String {
            let type_value = if self.choose(5) == 0 {
                self.scalar()
            } else {
                self.token("session_meta")
            };
            let payload = if self.choose(5) == 0 {
                self.value(3)
            } else {
                self.payload()
            };
            let mut members = vec![
                ("type", type_value),
                ("payload", payload),
                ("extra", self.value(3)),
                ("model_provider", self.string()),
            ];
            if self.choose(3) == 0 {
                let duplicate = if self.choose(2) == 0 {
                    self.token("session_meta")
                } else {
                    self.value(2)
                };
                members.push(("type", duplicate));
            }
            if self.choose(3) == 0 {
                let duplicate = if self.choose(2) == 0 {
                    self.payload()
                } else {
                    self.scalar()
                };
                members.push(("payload", duplicate));
            }
            let leading = self.whitespace();
            let body = self.object(members);
            format!("{leading}{body}{}", self.whitespace())
        }
    }

    #[test]
    fn differential_deterministic_random_lines_and_mutations() {
        let mut generator = Generator(0x7e57_c0de_baad_f00d);
        let mut accepted = 0;
        let mut rejected = 0;
        for _ in 0..4096 {
            let line = generator.line();
            if differential(line.as_bytes()) {
                accepted += 1;
            } else {
                rejected += 1;
            }
            let mut mutated = line.as_bytes().to_vec();
            let index = generator.choose(mutated.len());
            mutated[index] = generator.next() as u8;
            differential(&mutated);
            let cut = generator.choose(line.len());
            differential(&line.as_bytes()[..cut]);
        }
        assert!(accepted > 1500, "too few valid metadata lines: {accepted}");
        assert!(rejected > 500, "too few non-metadata lines: {rejected}");
    }

    #[test]
    fn deterministic_random_bytes_never_panic() {
        let mut generator = Generator(0x9e37_79b9_7f4a_7c15);
        for _ in 0..4096 {
            let mut line = br#"{"type":"session_meta","payload":{"value":"#.to_vec();
            for _ in 0..generator.choose(80) {
                line.push(generator.next() as u8);
            }
            line.extend_from_slice(b"}}");
            differential(&line);
        }
    }

    #[test]
    fn provider_member_presence_and_scalar_span() {
        let line = br#"{"type":"session_meta","payload":{"id":"t","model_provider":{"x":1}}}"#;
        let meta = inspect_session_meta(line).unwrap();
        assert!(meta.provider_present);
        assert_eq!(meta.provider_scalar_span, None);
        let line = br#"{"type":"session_meta","payload":{"id":"t","model_provider": 42 }}"#;
        let meta = inspect_session_meta(line).unwrap();
        assert_eq!(&line[meta.provider_scalar_span.unwrap()], b"42");
        let line = br#"{"type":"session_meta","payload":{"id":"t","model_provider":"krill"}}"#;
        let meta = inspect_session_meta(line).unwrap();
        assert_eq!(meta.provider_scalar_span, meta.provider_span);
        let line = br#"{"type":"session_meta","payload":{"id":"t"}}"#;
        let meta = inspect_session_meta(line).unwrap();
        assert!(!meta.provider_present);
        assert_eq!(meta.provider_scalar_span, None);
    }
}
