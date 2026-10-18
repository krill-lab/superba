//! A small JSON scanner that keeps byte positions: the members of every object
//! with the spans of their key and value. `serde_json` checks the syntax; this
//! only finds where things are (problem positions, masking, highlighting), so
//! it is lenient and stops at the first thing it does not understand.

use std::ops::Range;

/// The kind of a JSON value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Object,
    Array,
    String,
    Number,
    Bool,
    Null,
}

impl Kind {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Kind::Object => "object",
            Kind::Array => "array",
            Kind::String => "string",
            Kind::Number => "number",
            Kind::Bool => "boolean",
            Kind::Null => "null",
        }
    }
}

/// One object member.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Member {
    /// Names from the root (`["tokens", "access_token"]`); array elements are
    /// `"[]"`.
    pub path: Vec<String>,
    /// The key, quotes included.
    pub key: Range<usize>,
    /// The value (a string with its quotes).
    pub value: Range<usize>,
    /// Its kind.
    pub kind: Kind,
}

impl Member {
    /// Dotted path (`tokens.access_token`).
    pub(crate) fn dotted(&self) -> String {
        self.path.join(".")
    }

    /// The member's own name.
    pub(crate) fn name(&self) -> &str {
        self.path.last().map_or("", String::as_str)
    }
}

struct Scanner<'a> {
    s: &'a [u8],
    i: usize,
    out: Vec<Member>,
}

impl Scanner<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    /// A string starting at `self.i` (a `"`); returns its decoded text.
    fn string(&mut self) -> Option<String> {
        if self.s.get(self.i) != Some(&b'"') {
            return None;
        }
        let start = self.i;
        self.i += 1;
        while self.i < self.s.len() {
            match self.s[self.i] {
                b'\\' => self.i += 2,
                b'"' => {
                    self.i += 1;
                    let raw = std::str::from_utf8(&self.s[start..self.i]).ok()?;
                    return Some(serde_json::from_str::<String>(raw).unwrap_or_default());
                }
                _ => self.i += 1,
            }
        }
        None
    }

    fn value(&mut self, path: &mut Vec<String>) -> Option<Kind> {
        self.ws();
        match *self.s.get(self.i)? {
            b'{' => {
                self.i += 1;
                loop {
                    self.ws();
                    match *self.s.get(self.i)? {
                        b'}' => {
                            self.i += 1;
                            return Some(Kind::Object);
                        }
                        b',' => {
                            self.i += 1;
                            continue;
                        }
                        b'"' => {}
                        _ => return None,
                    }
                    let key_start = self.i;
                    let name = self.string()?;
                    let key = key_start..self.i;
                    self.ws();
                    if self.s.get(self.i) != Some(&b':') {
                        return None;
                    }
                    self.i += 1;
                    self.ws();
                    let value_start = self.i;
                    path.push(name);
                    let slot = self.out.len();
                    // Reserve the member's place so a parent comes before its children.
                    self.out.push(Member {
                        path: path.clone(),
                        key: key.clone(),
                        value: value_start..value_start,
                        kind: Kind::Null,
                    });
                    let kind = self.value(path);
                    path.pop();
                    let kind = kind?;
                    self.out[slot].value = value_start..self.i;
                    self.out[slot].kind = kind;
                }
            }
            b'[' => {
                self.i += 1;
                loop {
                    self.ws();
                    match *self.s.get(self.i)? {
                        b']' => {
                            self.i += 1;
                            return Some(Kind::Array);
                        }
                        b',' => {
                            self.i += 1;
                        }
                        _ => {
                            path.push("[]".into());
                            let kind = self.value(path);
                            path.pop();
                            kind?;
                        }
                    }
                }
            }
            b'"' => self.string().map(|_| Kind::String),
            b't' | b'f' | b'n' => {
                let rest = &self.s[self.i..];
                for (word, kind) in [
                    (&b"true"[..], Kind::Bool),
                    (b"false", Kind::Bool),
                    (b"null", Kind::Null),
                ] {
                    if rest.starts_with(word) {
                        self.i += word.len();
                        return Some(kind);
                    }
                }
                None
            }
            b'-' | b'0'..=b'9' => {
                while self.i < self.s.len()
                    && matches!(
                        self.s[self.i],
                        b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'
                    )
                {
                    self.i += 1;
                }
                Some(Kind::Number)
            }
            _ => None,
        }
    }
}

/// Every object member of `text` in document order (parents before children),
/// as far as the text could be read.
pub(crate) fn members(text: &str) -> Vec<Member> {
    let mut sc = Scanner {
        s: text.as_bytes(),
        i: 0,
        out: Vec::new(),
    };
    let mut path = Vec::new();
    let _ = sc.value(&mut path);
    // Drop members whose value could not be read (left at an empty span).
    sc.out.into_iter().filter(|m| !m.value.is_empty()).collect()
}
