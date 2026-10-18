#![allow(missing_docs)]

//! The application icon (user bug 52): on macOS it follows Apple's icon grid, an
//! 824 px tile at 100,100 of the 1024 px canvas, so Superba is as big as other
//! apps in the Dock, Finder and Launchpad.

use std::path::{Path, PathBuf};

use superba_gui::app::{is_app_bundle_executable, window_icon};

fn repo_file(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

/// Bounding box `(left, top, right, bottom)` (exclusive) of the opaque tile:
/// pixels at least half opaque, so the soft shadow does not count.
fn tile_box(icon: &egui::IconData) -> (u32, u32, u32, u32) {
    let (mut l, mut t, mut r, mut b) = (u32::MAX, u32::MAX, 0, 0);
    for y in 0..icon.height {
        for x in 0..icon.width {
            if icon.rgba[((y * icon.width + x) * 4 + 3) as usize] >= 128 {
                (l, t, r, b) = (l.min(x), t.min(y), r.max(x + 1), b.max(y + 1));
            }
        }
    }
    (l, t, r, b)
}

fn assert_on_apple_grid(what: &str, icon: &egui::IconData) {
    let px = f64::from(icon.width);
    let (l, t, r, b) = tile_box(icon);
    let (margin, tile) = (px * 100.0 / 1024.0, px * 824.0 / 1024.0);
    for (name, got, want) in [
        ("left", l, margin),
        ("top", t, margin),
        ("width", r - l, tile),
        ("height", b - t, tile),
    ] {
        assert!(
            (f64::from(got) - want).abs() <= 1.0,
            "{what} ({px} px): tile {name} is {got}, Apple's grid wants {want:.1}"
        );
    }
}

/// `(type, data)` of every entry of an .icns file.
fn icns_entries(blob: &[u8]) -> Vec<(String, &[u8])> {
    assert_eq!(&blob[..4], b"icns");
    let be = |at: usize| u32::from_be_bytes(blob[at..at + 4].try_into().unwrap()) as usize;
    assert_eq!(be(4), blob.len(), "icns header length");
    let mut entries = Vec::new();
    let mut at = 8;
    while at < blob.len() {
        let len = be(at + 4);
        let ty = String::from_utf8_lossy(&blob[at..at + 4]).into_owned();
        entries.push((ty, &blob[at + 8..at + len]));
        at += len;
    }
    entries
}

/// Pixels of an `ic04`/`ic05` entry: `ARGB`, then Apple's RLE of the alpha, red,
/// green and blue planes (a control byte below 0x80 is followed by that many plus
/// one literal bytes; from 0x80 up it repeats the next byte control - 0x80 + 3 times).
/// macOS reads the colours as premultiplied by alpha, so none may exceed it.
fn decode_argb(data: &[u8], px: u32) -> egui::IconData {
    assert_eq!(&data[..4], b"ARGB");
    let count = (px * px) as usize;
    let mut at = 4;
    let mut planes = Vec::new();
    for _ in 0..4 {
        let mut plane = Vec::with_capacity(count);
        while plane.len() < count {
            let control = data[at] as usize;
            if control < 0x80 {
                plane.extend_from_slice(&data[at + 1..at + 2 + control]);
                at += 2 + control;
            } else {
                plane.extend(std::iter::repeat_n(data[at + 1], control - 0x80 + 3));
                at += 2;
            }
        }
        assert_eq!(plane.len(), count, "a run crosses the end of a plane");
        planes.push(plane);
    }
    assert_eq!(at, data.len(), "trailing bytes after the four planes");
    let [a, r, g, b] = [&planes[0], &planes[1], &planes[2], &planes[3]];
    for i in 0..count {
        assert!(
            r[i] <= a[i] && g[i] <= a[i] && b[i] <= a[i],
            "ARGB pixel {i} is not premultiplied: {:?}",
            (a[i], r[i], g[i], b[i])
        );
    }
    let rgba = (0..count).flat_map(|i| [r[i], g[i], b[i], a[i]]).collect();
    egui::IconData {
        rgba,
        width: px,
        height: px,
    }
}

#[test]
fn bundle_executables_are_recognised() {
    for exe in [
        "/Applications/Superba.app/Contents/MacOS/superba",
        "/Users/me/Applications/Superba.app/Contents/MacOS/superba",
        "/Volumes/Superba 0.1.1/Superba.app/Contents/MacOS/superba",
        "/tmp/x/Other.app/Contents/MacOS/superba",
    ] {
        assert!(is_app_bundle_executable(Path::new(exe)), "{exe}");
    }
    for exe in [
        "/usr/local/bin/superba",
        "/Users/me/superba-0.1.1-macos-universal/spb",
        "/target/release/superba",
        "/Applications/Superba/Contents/MacOS/superba",
        "/Applications/Superba.app/Contents/Resources/superba",
        "/Applications/Superba.app/MacOS/superba",
        "/.app/Contents/MacOS/superba",
        "superba",
        "",
    ] {
        assert!(!is_app_bundle_executable(Path::new(exe)), "{exe}");
    }
}

#[test]
fn the_window_icon_is_a_256_px_image() {
    // The test binary never runs inside an app bundle, so an icon is set everywhere.
    let icon = window_icon();
    assert_eq!((icon.width, icon.height), (256, 256));
    assert_eq!(icon.rgba.len(), 256 * 256 * 4);
    #[cfg(target_os = "macos")]
    assert_on_apple_grid("window_icon() on macOS", &icon);
}

#[test]
fn the_macos_icons_follow_apples_grid() {
    let png = std::fs::read(repo_file("assets/icon/png/superba-macos-256.png")).unwrap();
    let icon = eframe::icon_data::from_png_bytes(&png).unwrap();
    assert_on_apple_grid("superba-macos-256.png", &icon);

    let blob = std::fs::read(repo_file("scripts/release/Superba.icns")).unwrap();
    let entries = icns_entries(&blob);
    // What iconutil writes for a full .iconset: 16 to 512 px plus @2x. macOS reads
    // 16 and 32 px only as ARGB (ic04/ic05; PNG data in icp4/icp5 comes out as
    // noise) and icp6 as 48 px, so neither icp type is used.
    let expected = [
        ("ic04", 16),
        ("ic11", 32),
        ("ic05", 32),
        ("ic12", 64),
        ("ic07", 128),
        ("ic13", 256),
        ("ic08", 256),
        ("ic14", 512),
        ("ic09", 512),
        ("ic10", 1024),
    ];
    let types: Vec<&str> = entries.iter().map(|(ty, _)| ty.as_str()).collect();
    let want: Vec<&str> = expected.iter().map(|(ty, _)| *ty).collect();
    assert_eq!(types, want, "Superba.icns entries");
    for ((ty, data), (_, px)) in entries.iter().zip(expected) {
        let icon = if matches!(ty.as_str(), "ic04" | "ic05") {
            decode_argb(data, px)
        } else {
            assert_eq!(&data[..8], b"\x89PNG\r\n\x1a\n", "{ty} is a PNG");
            eframe::icon_data::from_png_bytes(data).unwrap()
        };
        assert_eq!((icon.width, icon.height), (px, px), "{ty}");
        // 16 px is too coarse for a 1 px tolerance (the tile edge is half a pixel).
        if px >= 32 {
            assert_on_apple_grid(&format!("Superba.icns {ty}"), &icon);
        }
    }
}
