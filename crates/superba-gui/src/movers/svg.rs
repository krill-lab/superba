//! Splits the scene's SVG into what the GUI draws: static layers, moving parts
//! and texts, each with the chain of transforms (fixed ones from `transform`
//! attributes, animated ones named by their CSS class) that places it in the
//! scene. Pure: no egui, no rasterization (see `render`).
//!
//! The source stays the single description of the artwork: every layer is a
//! standalone SVG document cut out of the source text (element byte ranges of
//! the parsed document) with "holes" where moving parts and texts are, so the
//! static layers are rasterized once and the parts once each. Without the
//! source's `<style>` (animations, base opacities, fonts) a part renders in its
//! own coordinates at full opacity; [`super::pose::Pose`] supplies the rest.
//!
//! Texts are drawn by egui (the app's fonts, translated: see `render`), so they
//! are holes in every layer, including the `<text>` of the `#crate` symbol,
//! which appears once for each `<use>` of it.
//!
//! Layering: the static content becomes two layers, the background (up to the
//! resting krill) and the foreground (the side table, plant and note, which the
//! resting krill's cup passes behind, and the status line); the moving parts
//! are drawn between them in document order, the texts on top.

use std::ops::Range;

use resvg::tiny_skia::Transform;

use super::pose::{ANIMATED_CLASSES, Pose};

/// One link of the chain from scene coordinates to a layer's own coordinates.
#[derive(Clone, Debug, PartialEq)]
pub enum Link {
    /// A `transform` attribute.
    Fixed(Transform),
    /// A CSS animation: the class and, for the steam wisps, which one.
    Anim(&'static str, usize),
}

/// The transform and opacity of something placed by a chain of links.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Placement {
    /// Root to element, outermost first (the element's own animation last).
    pub chain: Vec<Link>,
}

impl Placement {
    /// Local coordinates -> scene coordinates in `pose`.
    pub fn transform(&self, pose: &Pose) -> Transform {
        self.chain
            .iter()
            .fold(Transform::identity(), |m, link| match link {
                Link::Fixed(t) => m.pre_concat(*t),
                Link::Anim(class, i) => m.pre_concat(pose.transform(class, *i)),
            })
    }

    /// Product of the animated opacities along the chain.
    pub fn opacity(&self, pose: &Pose) -> f32 {
        self.chain
            .iter()
            .map(|link| match link {
                Link::Anim(class, i) => pose.opacity(class, *i),
                Link::Fixed(_) => 1.0,
            })
            .product()
    }

    /// The chain names `class` (the element or one of its groups).
    pub fn has(&self, class: &str) -> bool {
        self.chain
            .iter()
            .any(|l| matches!(l, Link::Anim(c, _) if *c == class))
    }
}

/// What a layer is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayerKind {
    /// Static content behind the moving parts.
    Background,
    /// Static content in front of the resting krill.
    Foreground,
    /// A moving part: the element with this animated class.
    Part(&'static str),
}

/// A layer: a standalone SVG of its content in its own coordinates.
#[derive(Clone, Debug)]
pub struct Layer {
    /// Kind.
    pub kind: LayerKind,
    /// The SVG document (same viewport as the source).
    pub svg: String,
    /// Where it goes.
    pub place: Placement,
}

/// `text-anchor`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    /// `start`.
    Start,
    /// `middle`.
    Middle,
    /// `end`.
    End,
}

/// A `<text>` of the scene (one per `<use>` for texts inside symbols).
#[derive(Clone, Debug, PartialEq)]
pub struct TextItem {
    /// The text as written in the SVG.
    pub text: String,
    /// Baseline anchor point, local coordinates.
    pub x: f32,
    /// Baseline, local coordinates.
    pub y: f32,
    /// `font-size`.
    pub size: f32,
    /// `font-weight` (400 when absent).
    pub weight: u16,
    /// `fill` as RGB.
    pub fill: [u8; 3],
    /// `text-anchor`.
    pub anchor: Anchor,
    /// `letter-spacing`.
    pub letter_spacing: f32,
    /// `class="mono"`.
    pub mono: bool,
    /// Where it goes.
    pub place: Placement,
}

/// The analysed scene.
#[derive(Clone, Debug)]
pub struct Scene {
    /// viewBox width and height.
    pub size: (f32, f32),
    /// Layers in drawing order.
    pub layers: Vec<Layer>,
    /// Texts (drawn after every layer).
    pub texts: Vec<TextItem>,
}

/// Presentation attributes children inherit (taken from a part's groups).
const INHERITED: &[&str] = &[
    "fill",
    "fill-opacity",
    "fill-rule",
    "stroke",
    "stroke-width",
    "stroke-linecap",
    "stroke-linejoin",
    "stroke-dasharray",
    "stroke-opacity",
];

/// The animated class of an element (`None` for static ones).
fn animated_class(node: roxmltree::Node<'_, '_>) -> Option<&'static str> {
    let classes = node.attribute("class")?;
    classes
        .split_ascii_whitespace()
        .find_map(|c| ANIMATED_CLASSES.iter().copied().find(|a| *a == c))
}

/// Which steam wisp (`steam`, `steam two`, `steam three`).
fn steam_index(node: roxmltree::Node<'_, '_>) -> usize {
    let classes = node.attribute("class").unwrap_or_default();
    if classes.split_ascii_whitespace().any(|c| c == "three") {
        2
    } else if classes.split_ascii_whitespace().any(|c| c == "two") {
        1
    } else {
        0
    }
}

/// Classes whose animation is a transform (others only fade).
fn moves(class: &str) -> bool {
    !matches!(
        class,
        "walk-legs" | "still-legs" | "delivered" | "return-arm" | "dot"
    )
}

/// The links an element contributes (its `transform`, its animation).
fn links_of(node: roxmltree::Node<'_, '_>) -> Vec<Link> {
    let mut out = Vec::new();
    match animated_class(node) {
        // A CSS transform replaces the `transform` attribute.
        Some(class) if moves(class) => out.push(Link::Anim(class, steam_index(node))),
        other => {
            if let Some(t) = node.attribute("transform").and_then(parse_transform) {
                out.push(Link::Fixed(t));
            }
            if let Some(class) = other {
                out.push(Link::Anim(class, steam_index(node)));
            }
        }
    }
    out
}

/// The chain of `node`'s ancestors (not `node` itself), outermost first.
fn ancestor_chain(node: roxmltree::Node<'_, '_>) -> Vec<Link> {
    let mut nodes: Vec<_> = node
        .ancestors()
        .skip(1)
        .filter(|n| n.is_element() && n.tag_name().name() != "svg")
        .collect();
    nodes.reverse();
    nodes.into_iter().flat_map(links_of).collect()
}

/// Parse a `transform` list made of `translate`, `scale`, `rotate` and `matrix`.
pub fn parse_transform(s: &str) -> Option<Transform> {
    let mut out = Transform::identity();
    let mut rest = s.trim();
    while !rest.is_empty() {
        let open = rest.find('(')?;
        let close = rest.find(')')?;
        let name = rest[..open].trim().trim_start_matches(',').trim();
        let args: Vec<f32> = rest[open + 1..close]
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|a| !a.is_empty())
            .map(str::parse)
            .collect::<Result<_, _>>()
            .ok()?;
        let t = match (name, args.as_slice()) {
            ("translate", [x]) => Transform::from_translate(*x, 0.0),
            ("translate", [x, y]) => Transform::from_translate(*x, *y),
            ("scale", [s]) => Transform::from_scale(*s, *s),
            ("scale", [x, y]) => Transform::from_scale(*x, *y),
            ("rotate", [a]) => Transform::from_rotate(*a),
            ("rotate", [a, cx, cy]) => Transform::from_rotate_at(*a, *cx, *cy),
            ("matrix", [a, b, c, d, e, f]) => Transform::from_row(*a, *b, *c, *d, *e, *f),
            _ => return None,
        };
        out = out.pre_concat(t);
        rest = rest[close + 1..].trim();
    }
    Some(out)
}

/// `source[range]` without the `holes` (sorted, inside `range`, disjoint).
fn cut(source: &str, range: Range<usize>, holes: &[Range<usize>]) -> String {
    let mut out = String::new();
    let mut at = range.start;
    for h in holes {
        if h.start >= at && h.end <= range.end {
            out.push_str(&source[at..h.start]);
            at = h.end;
        }
    }
    out.push_str(&source[at..range.end]);
    out
}

/// Ranges of the outermost descendants of `node` (not `node`) that `is_hole`.
fn holes_in(
    node: roxmltree::Node<'_, '_>,
    is_hole: &dyn Fn(roxmltree::Node<'_, '_>) -> bool,
) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut stack: Vec<_> = node.children().filter(|c| c.is_element()).collect();
    stack.reverse();
    while let Some(n) = stack.pop() {
        if is_hole(n) {
            out.push(n.range());
        } else {
            let mut kids: Vec<_> = n.children().filter(|c| c.is_element()).collect();
            kids.reverse();
            stack.extend(kids);
        }
    }
    out.sort_by_key(|r| r.start);
    out
}

fn is_text(n: roxmltree::Node<'_, '_>) -> bool {
    n.tag_name().name() == "text"
}

/// Analyse the scene's SVG source.
pub fn analyse(source: &str) -> Result<Scene, String> {
    let doc = roxmltree::Document::parse(source).map_err(|e| e.to_string())?;
    let root = doc.root_element();
    let view: Vec<f32> = root
        .attribute("viewBox")
        .unwrap_or("0 0 1440 900")
        .split_ascii_whitespace()
        .filter_map(|v| v.parse().ok())
        .collect();
    let (w, h) = match view.as_slice() {
        [_, _, w, h] => (*w, *h),
        _ => (1440.0, 900.0),
    };
    let head = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="{w}" height="{h}" viewBox="0 0 {w} {h}">"#
    );
    // Shared definitions, without their texts (drawn by egui).
    let defs = root
        .children()
        .find(|n| n.has_tag_name("defs"))
        .map(|d| cut(source, d.range(), &holes_in(d, &is_text)))
        .unwrap_or_default();
    let document = |body: &str| format!("{head}{defs}{body}</svg>");

    let hole = |n: roxmltree::Node<'_, '_>| is_text(n) || animated_class(n).is_some();
    let top: Vec<_> = root
        .children()
        .filter(|n| {
            n.is_element() && !matches!(n.tag_name().name(), "defs" | "style" | "title" | "desc")
        })
        .collect();
    // The foreground starts after the group of the resting krill.
    let split = top
        .iter()
        .position(|n| {
            n.descendants()
                .any(|d| animated_class(d) == Some("rest-body"))
        })
        .map_or(top.len(), |i| i + 1);
    let static_layer = |nodes: &[roxmltree::Node<'_, '_>]| -> String {
        nodes
            .iter()
            .filter(|n| !hole(**n))
            .map(|n| cut(source, n.range(), &holes_in(*n, &hole)))
            .collect()
    };
    let mut layers = vec![Layer {
        kind: LayerKind::Background,
        svg: document(&static_layer(&top[..split])),
        place: Placement::default(),
    }];
    let foreground = Layer {
        kind: LayerKind::Foreground,
        svg: document(&static_layer(&top[split..])),
        place: Placement::default(),
    };
    let foreground_at = top.get(split).map_or(usize::MAX, |n| n.range().start);
    let mut foreground = Some(foreground);

    // Moving parts in document order.
    for node in root.descendants().filter(|n| n.is_element()) {
        let Some(class) = animated_class(node) else {
            continue;
        };
        if node.ancestors().any(|a| a.has_tag_name("defs")) {
            continue;
        }
        if node.range().start > foreground_at
            && let Some(f) = foreground.take()
        {
            layers.push(f);
        }
        let holes = holes_in(node, &hole);
        let body = cut(source, node.range(), &holes);
        // Nothing to draw besides nested parts (e.g. `.facing`).
        if !has_drawable(&body) {
            continue;
        }
        let mut inherited = String::new();
        for name in INHERITED {
            if let Some(v) = node.ancestors().skip(1).find_map(|a| a.attribute(*name)) {
                inherited.push_str(&format!(r#" {name}="{v}""#));
            }
        }
        let mut chain = ancestor_chain(node);
        chain.extend(links_of(node));
        layers.push(Layer {
            kind: LayerKind::Part(class),
            svg: document(&format!("<g{inherited}>{body}</g>")),
            place: Placement { chain },
        });
    }
    if let Some(f) = foreground.take() {
        layers.push(f);
    }

    // Texts: in the document, and inside every `<use>` of a symbol.
    let mut texts = Vec::new();
    for node in root.descendants().filter(|n| n.is_element()) {
        if node.ancestors().any(|a| a.has_tag_name("defs")) {
            continue;
        }
        if is_text(node) {
            let mut chain = ancestor_chain(node);
            chain.extend(links_of(node));
            texts.extend(text_item(node, Placement { chain }));
        } else if node.has_tag_name("use") {
            let target = node
                .attribute(("http://www.w3.org/1999/xlink", "href"))
                .or_else(|| node.attribute("href"))
                .and_then(|h| h.strip_prefix('#'))
                .and_then(|id| doc.descendants().find(|d| d.attribute("id") == Some(id)));
            let Some(target) = target else {
                continue;
            };
            let mut base = ancestor_chain(node);
            base.extend(links_of(node));
            for t in target.descendants().filter(|d| is_text(*d)) {
                let mut chain = base.clone();
                // Groups between the symbol and its text.
                let mut inner: Vec<_> =
                    t.ancestors().skip(1).take_while(|a| *a != target).collect();
                inner.reverse();
                chain.extend(inner.into_iter().flat_map(links_of));
                chain.extend(links_of(t));
                texts.extend(text_item(t, Placement { chain }));
            }
        }
    }
    Ok(Scene {
        size: (w, h),
        layers,
        texts,
    })
}

/// Whether an SVG fragment draws anything besides empty groups.
fn has_drawable(body: &str) -> bool {
    [
        "<path", "<rect", "<circle", "<ellipse", "<use", "<line", "<poly",
    ]
    .iter()
    .any(|t| body.contains(t))
}

fn text_item(node: roxmltree::Node<'_, '_>, place: Placement) -> Option<TextItem> {
    let text: String = node
        .descendants()
        .filter(|n| n.is_text())
        .filter_map(|n| n.text())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if text.is_empty() {
        return None;
    }
    let num = |name: &str, default: f32| {
        node.attribute(name)
            .and_then(|v| v.trim_end_matches("px").parse::<f32>().ok())
            .unwrap_or(default)
    };
    let fill = node
        .attribute("fill")
        .and_then(parse_hex)
        .unwrap_or([0, 0, 0]);
    Some(TextItem {
        text,
        x: num("x", 0.0),
        y: num("y", 0.0),
        size: num("font-size", 16.0),
        weight: node
            .attribute("font-weight")
            .and_then(|w| w.parse().ok())
            .unwrap_or(400),
        fill,
        anchor: match node.attribute("text-anchor") {
            Some("middle") => Anchor::Middle,
            Some("end") => Anchor::End,
            _ => Anchor::Start,
        },
        letter_spacing: num("letter-spacing", 0.0),
        mono: node
            .attribute("class")
            .is_some_and(|c| c.split_ascii_whitespace().any(|c| c == "mono")),
        place,
    })
}

/// `#RRGGBB` / `#RGB`.
fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let hex = s.strip_prefix('#')?;
    let channel = |i: usize, len: usize| {
        u8::from_str_radix(&hex[i * len..i * len + len], 16)
            .ok()
            .map(|v| if len == 1 { v * 17 } else { v })
    };
    match hex.len() {
        6 => Some([channel(0, 2)?, channel(1, 2)?, channel(2, 2)?]),
        3 => Some([channel(0, 1)?, channel(1, 1)?, channel(2, 1)?]),
        _ => None,
    }
}
