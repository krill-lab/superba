//! A flat view of small XML documents (Sparkle appcasts, RPM `repomd.xml` and
//! `primary.xml`): every element with its slash-joined path of local names, its
//! attributes (local names, unescaped) and its text. No DTDs or external
//! entities are ever resolved.

use std::collections::BTreeMap;

use quick_xml::events::Event;
use quick_xml::{Reader, XmlVersion};
use superba_core::{Error, Result};

/// One element.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Element {
    /// Local names from the root, joined with `/` (`rss/channel/item/enclosure`).
    pub path: String,
    /// Attributes by local name.
    pub attrs: BTreeMap<String, String>,
    /// Text content (direct children only).
    pub text: String,
}

impl Element {
    /// An attribute.
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.get(name).map(String::as_str)
    }
}

fn predefined(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        _ => {
            let num = name.strip_prefix('#')?;
            let code = match num.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => num.parse().ok()?,
            };
            char::from_u32(code)?
        }
    })
}

/// Parse `xml` into its elements, in document order.
pub(crate) fn elements(xml: &str, what: &str) -> Result<Vec<Element>> {
    let err = |e: &dyn std::fmt::Display| Error::parse(what, e.to_string());
    let mut reader = Reader::from_str(xml);
    let mut out: Vec<Element> = Vec::new();
    let mut stack: Vec<(String, usize)> = Vec::new();
    let start =
        |e: &quick_xml::events::BytesStart<'_>, stack: &[(String, usize)]| -> Result<Element> {
            let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
            let mut path: Vec<&str> = stack.iter().map(|(n, _)| n.as_str()).collect();
            path.push(&name);
            let mut attrs = BTreeMap::new();
            for a in e.attributes() {
                let a = a.map_err(|e| err(&e))?;
                let key = String::from_utf8_lossy(a.key.local_name().as_ref()).into_owned();
                let value = a
                    .normalized_value(XmlVersion::Implicit1_0)
                    .map_err(|e| err(&e))?;
                attrs.insert(key, value.into_owned());
            }
            Ok(Element {
                path: path.join("/"),
                attrs,
                text: String::new(),
            })
        };
    loop {
        match reader.read_event().map_err(|e| err(&e))? {
            Event::Start(e) => {
                let el = start(&e, &stack)?;
                let name = el.path.rsplit('/').next().unwrap_or_default().to_owned();
                stack.push((name, out.len()));
                out.push(el);
            }
            Event::Empty(e) => out.push(start(&e, &stack)?),
            Event::End(_) => {
                stack.pop();
            }
            Event::Text(t) => {
                if let Some((_, i)) = stack.last() {
                    out[*i]
                        .text
                        .push_str(&t.xml10_content().map_err(|e| err(&e))?);
                }
            }
            Event::CData(t) => {
                if let Some((_, i)) = stack.last() {
                    out[*i]
                        .text
                        .push_str(&String::from_utf8_lossy(&t.into_inner()));
                }
            }
            Event::GeneralRef(r) => {
                if let Some((_, i)) = stack.last()
                    && let Some(c) = r.decode().ok().as_deref().and_then(predefined)
                {
                    out[*i].text.push(c);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}
