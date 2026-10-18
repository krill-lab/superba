//! Randomised checks of the YAML editor on documents written the ways people
//! write them (2 or 4 space indentation, lists under a key indented or not,
//! comments on their own lines and after values, blank lines, quoted keys,
//! small flow collections): every edit either means exactly the old document
//! plus the change, keeping every comment, or is refused with the text
//! unchanged; setting and removing a new entry gives the text back byte for
//! byte.

use serde_json::{Map, Value, json};
use superba_config::harness::slot::{self, SlotPath};
use superba_config::harness::yaml::{YamlDoc, to_value};

/// A small deterministic generator (no extra dependency).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }
}

const WORDS: &[&str] = &[
    "alpha",
    "beta",
    "gpt-5.6-luna",
    "Krill relay",
    "http://127.0.0.1:18731/v1",
    "4o",
    "yes",
    "12",
    "0.5",
    "a: b",
    "x # y",
    "注释",
    "café",
    "-dash",
    "",
    "true",
    "null",
    "~",
];

fn scalar(r: &mut Rng) -> Value {
    match r.below(6) {
        0 => json!(r.below(300000)),
        1 => json!(r.chance(50)),
        2 => Value::Null,
        _ => json!(r.pick(WORDS)),
    }
}

fn key(r: &mut Rng, i: usize) -> String {
    format!("{}{i}", r.pick(&["k", "name", "opt", "url"]))
}

fn value(r: &mut Rng, depth: u32) -> Value {
    if depth == 0 || r.chance(40) {
        return scalar(r);
    }
    if r.chance(50) {
        let n = 1 + r.below(3) as usize;
        let mut o = Map::new();
        for i in 0..n {
            o.insert(key(r, i), value(r, depth - 1));
        }
        Value::Object(o)
    } else {
        let n = 1 + r.below(3) as usize;
        Value::Array((0..n).map(|_| value(r, depth - 1)).collect())
    }
}

fn plain_ok(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_. /".contains(c))
        && s.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && !matches!(
            s.to_ascii_lowercase().as_str(),
            "yes" | "no" | "on" | "off" | "true" | "false" | "null" | "y" | "n"
        )
        && s.trim() == s
}

fn scalar_text(r: &mut Rng, v: &Value) -> String {
    match v {
        Value::Null => r.pick(&["null", "~", "null"]).to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) if plain_ok(s) && r.chance(70) => s.clone(),
        Value::String(s) if !s.contains(['\'', '\n']) && r.chance(50) => format!("'{s}'"),
        Value::String(s) => serde_json::to_string(s).unwrap(),
        Value::Object(o) if o.is_empty() => "{}".into(),
        Value::Array(a) if a.is_empty() => "[]".into(),
        _ => unreachable!(),
    }
}

fn is_scalarish(v: &Value) -> bool {
    match v {
        Value::Object(o) => o.is_empty(),
        Value::Array(a) => a.is_empty(),
        _ => true,
    }
}

/// Lines of `v` in the user's style.
struct Style {
    indent: usize,
    zero_seq: bool,
}

fn comment(r: &mut Rng, out: &mut Vec<String>, pad: usize) {
    if r.chance(15) {
        out.push(format!("{}# note {}", " ".repeat(pad), r.below(1000)));
    }
    if r.chance(8) {
        out.push(String::new());
    }
}

fn trailing(r: &mut Rng) -> String {
    if r.chance(15) {
        format!("  # t{}", r.below(100))
    } else {
        String::new()
    }
}

fn emit(r: &mut Rng, st: &Style, v: &Value, col: usize, out: &mut Vec<String>) {
    let pad = " ".repeat(col);
    match v {
        Value::Object(o) => {
            for (k, val) in o {
                comment(r, out, col);
                let kt = if r.chance(15) {
                    serde_json::to_string(k).unwrap()
                } else {
                    k.clone()
                };
                let flat_map = val.as_object().is_some_and(|o| {
                    o.values().all(|x| {
                        x.is_number() || x.is_boolean() || x.as_str().is_some_and(plain_ok)
                    })
                });
                if is_scalarish(val) {
                    let t = scalar_text(r, val);
                    out.push(format!("{pad}{kt}: {t}{}", trailing(r)));
                } else if flat_map && r.chance(25) {
                    let items: Vec<String> = val
                        .as_object()
                        .unwrap()
                        .iter()
                        .map(|(k, x)| {
                            format!(
                                "{k}: {}",
                                match x {
                                    Value::String(s) => s.clone(),
                                    o => o.to_string(),
                                }
                            )
                        })
                        .collect();
                    out.push(format!(
                        "{pad}{kt}: {{{}}}{}",
                        items.join(", "),
                        trailing(r)
                    ));
                } else if let (Value::Array(a), true) = (val, r.chance(10)) {
                    if a.iter().all(|x| x.is_string() || x.is_number()) {
                        let items: Vec<String> = a
                            .iter()
                            .map(|x| serde_json::to_string(x).unwrap())
                            .collect();
                        out.push(format!("{pad}{kt}: [{}]", items.join(", ")));
                    } else {
                        out.push(format!("{pad}{kt}:{}", trailing(r)));
                        let c = if st.zero_seq { col } else { col + st.indent };
                        emit(r, st, val, c, out);
                    }
                } else {
                    out.push(format!("{pad}{kt}:{}", trailing(r)));
                    let c = if val.is_array() && st.zero_seq {
                        col
                    } else {
                        col + st.indent
                    };
                    emit(r, st, val, c, out);
                }
            }
        }
        Value::Array(a) => {
            for item in a {
                comment(r, out, col);
                if is_scalarish(item) {
                    let t = scalar_text(r, item);
                    out.push(format!("{pad}- {t}{}", trailing(r)));
                } else if item.is_object() {
                    let mut inner = Vec::new();
                    emit(r, st, item, col + 2, &mut inner);
                    // The first member goes on the dash line.
                    let first = inner
                        .iter()
                        .position(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
                        .unwrap();
                    for l in &inner[..first] {
                        out.push(l.clone());
                    }
                    out.push(format!("{pad}- {}", inner[first].trim_start()));
                    out.extend(inner[first + 1..].iter().cloned());
                } else {
                    out.push(format!("{pad}-"));
                    emit(r, st, item, col + 2, out);
                }
            }
        }
        _ => unreachable!(),
    }
}

fn document(r: &mut Rng) -> (String, Value) {
    let mut entries = Vec::new();
    let ids = [
        "llm-pi-ai",
        "agent-default-model",
        "session-log-deepseek",
        "storage",
    ];
    let n = r.below(4) as usize;
    for i in 0..n {
        let mut o = Map::new();
        if r.chance(85) {
            o.insert("id".into(), json!(ids[i % ids.len()]));
            if r.chance(70) {
                let mut config = Map::new();
                if i % ids.len() == 0 && r.chance(70) {
                    let mut providers = Map::new();
                    for j in 0..r.below(3) {
                        providers.insert(format!("p{j}"), value(r, 3));
                    }
                    config.insert("providers".into(), Value::Object(providers));
                }
                for j in 0..r.below(2) {
                    config.insert(key(r, j as usize), value(r, 2));
                }
                o.insert("config".into(), Value::Object(config));
            } else if r.chance(50) {
                o.insert("disabled".into(), json!(true));
            }
        } else {
            o.insert("insert".into(), json!([{ "id": "x", "name": "y" }]));
        }
        entries.push(Value::Object(o));
    }
    let v = Value::Array(entries);
    let st = Style {
        indent: *r.pick(&[2usize, 4]),
        zero_seq: r.chance(30),
    };
    let mut lines = Vec::new();
    if r.chance(60) {
        lines.push("# dsh patch layer".to_owned());
    }
    if v.as_array().unwrap().is_empty() {
        lines.push("[]".into());
    } else {
        emit(r, &st, &v, 0, &mut lines);
    }
    if r.chance(30) {
        lines.push("# end".into());
    }
    let mut text = lines.join("\n");
    if r.chance(85) {
        text.push('\n');
    }
    if r.chance(10) {
        text = text.replace('\n', "\r\n");
    }
    (text, v)
}

fn paths() -> Vec<SlotPath> {
    let llm = SlotPath::find(&[("id", "llm-pi-ai")]);
    vec![
        llm.clone()
            .then_key("config")
            .then_key("p0")
            .then_key("superba"),
        llm.clone()
            .then_key("config")
            .then_key("providers")
            .then_key("superba"),
        llm.clone()
            .then_key("config")
            .then_key("providers")
            .then_key("p0"),
        SlotPath::find(&[("id", "agent-default-model")]),
        SlotPath::find(&[("id", "session-telemetry-otel")]),
        llm.then_key("config").then_key("k0"),
    ]
}

fn comments(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| l.trim_start().starts_with('#'))
        .map(|l| l.trim().to_owned())
        .collect()
}

#[test]
fn random_documents_edit_exactly_or_not_at_all() {
    for seed in [0x5eed_1234u64, 7, 42, 0xdead_beef] {
        run(seed);
    }
}

fn run(seed: u64) {
    let mut r = Rng(seed);
    let (mut ok, mut refused, mut round_trips) = (0, 0, 0);
    for case in 0..3000 {
        let (text, meaning) = document(&mut r);
        let doc = match YamlDoc::parse(&text) {
            Ok(d) => d,
            Err(e) => panic!("case {case}: generated text does not parse ({e:?}):\n{text}"),
        };
        assert_eq!(
            doc.value(),
            &meaning,
            "case {case}: parser disagrees:\n{text}"
        );
        let path = r.pick(&paths()).clone();
        let want = if r.chance(70) {
            let mut v = value(&mut r, 3);
            if let Some(slot::Seg::Find(f)) = path.last() {
                let mut o = match v {
                    Value::Object(o) => o,
                    _ => Map::new(),
                };
                o.insert(f[0].0.clone(), json!(f[0].1));
                v = Value::Object(o);
            }
            Some(v)
        } else {
            None
        };
        let mut edited = doc.clone();
        let mut expected = meaning.clone();
        slot::set(&mut expected, &path, want.clone()).unwrap();
        let created = slot::missing_containers(&meaning, &path);
        let existed = slot::get(&meaning, &path).is_some();
        match edited.set(&path, want.as_ref()) {
            Ok(()) => {
                ok += 1;
                assert_eq!(to_value(edited.text()).unwrap(), expected, "case {case}");
                // Cross-check with another YAML reader (dsh's `yaml` package)
                // outside cargo: SUPERBA_YAML_DUMP=<dir> writes each result.
                if let Ok(dir) = std::env::var("SUPERBA_YAML_DUMP") {
                    let base = std::path::Path::new(&dir).join(format!("{seed}-{case}"));
                    std::fs::write(base.with_extension("yml"), edited.text()).unwrap();
                    std::fs::write(base.with_extension("json"), expected.to_string()).unwrap();
                }
                // Comments of the original are all still there, except those
                // inside a replaced or removed entry.
                if !existed {
                    let after = comments(edited.text());
                    for c in comments(&text) {
                        assert!(
                            after.contains(&c),
                            "case {case}: lost {c:?}\n{text}\n---\n{}",
                            edited.text()
                        );
                    }
                }
                // A new entry set and removed again gives the text back
                // (a container that was an empty value, `key:`, comes back
                // as absent: not byte for byte).
                let null_on_way = path
                    .prefixes()
                    .iter()
                    .any(|p| slot::get(&meaning, p).is_some_and(Value::is_null));
                if !existed && want.is_some() && !null_on_way {
                    let mut back = edited.clone();
                    back.set(&path, None).unwrap();
                    for c in created.iter().rev() {
                        if slot::get(back.value(), c)
                            .is_some_and(|v| slot::is_skeleton(v, c.last()))
                        {
                            back.set(c, None).unwrap();
                        }
                    }
                    assert_eq!(
                        back.text(),
                        text,
                        "case {case}: not byte-identical after removing"
                    );
                    round_trips += 1;
                }
            }
            Err(_) => {
                refused += 1;
                assert_eq!(edited.text(), text, "case {case}: refused but changed");
            }
        }
    }
    // The generator must exercise the editor, not only its refusals.
    assert!(ok > 1500, "ok {ok} refused {refused}");
    assert!(refused > 0, "refusals are exercised too: {refused}");
    assert!(round_trips > 500, "round trips {round_trips}");
    eprintln!("ok {ok}, refused {refused}, byte-identical round trips {round_trips}");
}
