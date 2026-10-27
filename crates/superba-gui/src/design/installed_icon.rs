//! The icon of a desktop app installed on this computer, read from the app
//! itself: a tool whose logo Superba may not ship shows the copy the user
//! installed instead (nothing is copied or redistributed).
//!
//! - **Windows**: the program's icon resource (`RT_GROUP_ICON`, its largest
//!   PNG or 32-bit image).
//! - **macOS**: the bundle's `.icns` (`CFBundleIconFile`), its PNG images.
//! - **Linux**: the package's icon in the `hicolor` theme or `pixmaps`.
//!
//! Only the headers and the resource section of a program are read (an
//! Electron program is a few hundred megabytes).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use resvg::tiny_skia::Pixmap;

/// The largest resource section read from a program.
const MAX_RESOURCES: u32 = 32 << 20;

/// The icon of the app at `path` (a program, a `.app` bundle or, on Linux,
/// its folder), `name` being its Linux package (`workbuddy`).
pub fn load(path: &Path, name: &str) -> Option<Pixmap> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    match ext.as_deref() {
        Some("app") => icns(&std::fs::read(bundle_icns(path)?).ok()?),
        Some("exe") => program_icon(path),
        _ => freedesktop(name),
    }
}

/// The bundle's icon file: `CFBundleIconFile` of an XML `Info.plist`, else
/// the only `.icns` in `Resources`.
fn bundle_icns(app: &Path) -> Option<PathBuf> {
    let resources = app.join("Contents").join("Resources");
    let plist = std::fs::read_to_string(app.join("Contents").join("Info.plist")).ok();
    if let Some(name) = plist
        .as_deref()
        .and_then(|p| plist_string(p, "CFBundleIconFile"))
    {
        let file = if name.to_ascii_lowercase().ends_with(".icns") {
            resources.join(&name)
        } else {
            resources.join(format!("{name}.icns"))
        };
        if file.is_file() {
            return Some(file);
        }
    }
    let mut found: Vec<PathBuf> = std::fs::read_dir(&resources)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("icns"))
        })
        .collect();
    found.sort();
    (found.len() == 1).then(|| found.remove(0))
}

/// The `<string>` after `<key>name</key>` in an XML property list.
fn plist_string(xml: &str, name: &str) -> Option<String> {
    let key = format!("<key>{name}</key>");
    let rest = xml[xml.find(&key)? + key.len()..].trim_start();
    let value = rest.strip_prefix("<string>")?;
    Some(value[..value.find("</string>")?].trim().to_owned())
}

/// The PNG image of an `.icns` closest above 128 pixels (else its largest).
pub fn icns(data: &[u8]) -> Option<Pixmap> {
    if data.get(..4)? != b"icns" {
        return None;
    }
    // Nominal sizes of the types that hold PNG data.
    let side = |t: &[u8]| match t {
        b"icp4" => 16,
        b"icp5" | b"ic11" => 32,
        b"icp6" | b"ic12" => 64,
        b"ic07" => 128,
        b"ic08" | b"ic13" => 256,
        b"ic09" | b"ic14" => 512,
        b"ic10" => 1024,
        _ => 0,
    };
    let mut images = Vec::new();
    let mut at = 8;
    while let Some(head) = data.get(at..at + 8) {
        let len = u32::from_be_bytes(head[4..8].try_into().ok()?) as usize;
        if len < 8 {
            break;
        }
        let body = data.get(at + 8..at + len)?;
        if body.starts_with(b"\x89PNG") && side(&head[..4]) > 0 {
            images.push((side(&head[..4]), body));
        }
        at += len;
    }
    images.sort_by_key(|(s, _)| if *s >= 128 { *s } else { 4096 - s });
    images
        .into_iter()
        .find_map(|(_, png)| Pixmap::decode_png(png).ok())
}

/// The icon in a Windows program's resources.
fn program_icon(path: &Path) -> Option<Pixmap> {
    let mut file = File::open(path).ok()?;
    let mut head = vec![0; 4096];
    let n = file.read(&mut head).ok()?;
    head.truncate(n);
    let section = resource_section(&head)?;
    if section.size > MAX_RESOURCES {
        return None;
    }
    let mut raw = vec![0; section.size as usize];
    file.seek(SeekFrom::Start(section.offset.into())).ok()?;
    file.read_exact(&mut raw).ok()?;
    pe_icon(&raw, section.rva, section.dir)
}

/// Where a program's resources are.
#[derive(Debug, PartialEq, Eq)]
struct Section {
    /// File offset of the section holding them.
    offset: u32,
    /// Its size in the file.
    size: u32,
    /// Its address once loaded.
    rva: u32,
    /// The resource directory's address.
    dir: u32,
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}
fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// The section holding the resource directory, from a program's headers.
fn resource_section(head: &[u8]) -> Option<Section> {
    if head.get(..2)? != b"MZ" {
        return None;
    }
    let pe = u32_at(head, 0x3c)? as usize;
    if head.get(pe..pe + 4)? != b"PE\0\0" {
        return None;
    }
    let sections = u16_at(head, pe + 6)? as usize;
    let optional = pe + 24;
    let directories = match u16_at(head, optional)? {
        0x10b => optional + 96,
        0x20b => optional + 112,
        _ => return None,
    };
    let dir = u32_at(head, directories + 2 * 8)?;
    if dir == 0 {
        return None;
    }
    let table = optional + u16_at(head, pe + 20)? as usize;
    (0..sections).find_map(|i| {
        let s = table + i * 40;
        let virtual_size = u32_at(head, s + 8)?;
        let rva = u32_at(head, s + 12)?;
        let size = u32_at(head, s + 16)?;
        let offset = u32_at(head, s + 20)?;
        (rva <= dir && dir < rva + virtual_size.max(size)).then_some(Section {
            offset,
            size,
            rva,
            dir,
        })
    })
}

const RT_ICON: u32 = 3;
const RT_GROUP_ICON: u32 = 14;

/// The entries of the resource directory at `at` (offsets from the root):
/// (id, `None` for a name), offset, is a subdirectory.
fn entries(rsrc: &[u8], at: usize) -> Vec<(Option<u32>, usize, bool)> {
    let count = |o| u16_at(rsrc, at + o).unwrap_or(0) as usize;
    (0..count(12) + count(14))
        .filter_map(|i| {
            let e = at + 16 + i * 8;
            let name = u32_at(rsrc, e)?;
            let data = u32_at(rsrc, e + 4)?;
            Some((
                (name & 0x8000_0000 == 0).then_some(name),
                (data & 0x7fff_ffff) as usize,
                data & 0x8000_0000 != 0,
            ))
        })
        .collect()
}

/// The data of the first language of resource `id` (any when `None`) of
/// `kind`.
fn resource(rsrc: &[u8], base: u32, kind: u32, id: Option<u32>) -> Option<&[u8]> {
    let (_, types, _) = entries(rsrc, 0)
        .into_iter()
        .find(|(t, _, dir)| *t == Some(kind) && *dir)?;
    let (_, names, _) = entries(rsrc, types)
        .into_iter()
        .find(|(n, _, dir)| *dir && (id.is_none() || *n == id))?;
    let (_, leaf, _) = entries(rsrc, names).into_iter().find(|(_, _, dir)| !dir)?;
    let rva = u32_at(rsrc, leaf)?;
    let size = u32_at(rsrc, leaf + 4)? as usize;
    let at = rva.checked_sub(base)? as usize;
    rsrc.get(at..at + size)
}

/// The largest image of the program's first icon group that decodes.
fn pe_icon(rsrc: &[u8], section_rva: u32, dir_rva: u32) -> Option<Pixmap> {
    let rsrc = rsrc.get(dir_rva.checked_sub(section_rva)? as usize..)?;
    let group = resource(rsrc, dir_rva, RT_GROUP_ICON, None)?;
    let count = u16_at(group, 4)? as usize;
    let mut images: Vec<(u32, u16, u16)> = (0..count)
        .filter_map(|i| {
            let e = 6 + i * 14;
            let width = match *group.get(e)? {
                0 => 256,
                w => u32::from(w),
            };
            Some((width, u16_at(group, e + 6)?, u16_at(group, e + 12)?))
        })
        .collect();
    images.sort_by_key(|(w, bits, _)| std::cmp::Reverse((*w, *bits)));
    images.into_iter().find_map(|(_, _, id)| {
        let data = resource(rsrc, dir_rva, RT_ICON, Some(u32::from(id)))?;
        if data.starts_with(b"\x89PNG") {
            Pixmap::decode_png(data).ok()
        } else {
            dib(data)
        }
    })
}

/// A 32-bit `BITMAPINFOHEADER` image (bottom-up BGRA, twice its height for
/// the mask); other depths are left to the monogram.
fn dib(data: &[u8]) -> Option<Pixmap> {
    let header = u32_at(data, 0)? as usize;
    let width = u32_at(data, 4)?;
    let height = u32_at(data, 8)? / 2;
    if u16_at(data, 14)? != 32 || width == 0 || width > 1024 || height != width {
        return None;
    }
    let row = width as usize * 4;
    let pixels = data.get(header..header + row * height as usize)?;
    let mut pixmap = Pixmap::new(width, height)?;
    let out = pixmap.data_mut();
    for y in 0..height as usize {
        let src = &pixels[(height as usize - 1 - y) * row..][..row];
        for (x, &[b, g, r, alpha]) in src.as_chunks::<4>().0.iter().enumerate() {
            let a = u16::from(alpha);
            let o = (y * width as usize + x) * 4;
            // Premultiplied, as a Pixmap is.
            out[o] = (u16::from(r) * a / 255) as u8;
            out[o + 1] = (u16::from(g) * a / 255) as u8;
            out[o + 2] = (u16::from(b) * a / 255) as u8;
            out[o + 3] = alpha;
        }
    }
    Some(pixmap)
}

/// The package's icon in the `hicolor` theme (or `pixmaps`), by the name its
/// `.desktop` file gives, else by the package name.
fn freedesktop(name: &str) -> Option<Pixmap> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let shares: Vec<PathBuf> = home
        .iter()
        .map(|h| h.join(".local/share"))
        .chain(["/usr/local/share", "/usr/share"].map(PathBuf::from))
        .collect();
    let mut names = vec![name.to_owned()];
    for share in &shares {
        let desktop = share.join("applications").join(format!("{name}.desktop"));
        if let Ok(text) = std::fs::read_to_string(desktop)
            && let Some(icon) = text.lines().find_map(|l| l.strip_prefix("Icon="))
        {
            let icon = icon.trim();
            if icon.starts_with('/') {
                if let Some(p) = png_file(Path::new(icon)) {
                    return Some(p);
                }
            } else {
                names.insert(0, icon.to_owned());
            }
            break;
        }
    }
    const SIZES: [&str; 6] = [
        "256x256",
        "512x512",
        "128x128",
        "1024x1024",
        "96x96",
        "64x64",
    ];
    names.iter().find_map(|n| {
        shares
            .iter()
            .flat_map(|s| {
                SIZES
                    .iter()
                    .map(move |z| s.join("icons/hicolor").join(z).join("apps"))
            })
            .chain([PathBuf::from("/usr/share/pixmaps")])
            .find_map(|dir| png_file(&dir.join(format!("{n}.png"))))
    })
}

fn png_file(path: &Path) -> Option<Pixmap> {
    let data = std::fs::read(path).ok()?;
    Pixmap::decode_png(&data).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(side: u32) -> Vec<u8> {
        let mut p = Pixmap::new(side, side).unwrap();
        p.fill(resvg::tiny_skia::Color::from_rgba8(10, 200, 90, 255));
        p.encode_png().unwrap()
    }

    #[test]
    fn icns_takes_the_png_closest_above_128() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../scripts/release/Superba.icns"
        );
        let icon = icns(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!((icon.width(), icon.height()), (128, 128));
        assert!(icns(b"icns\0\0\0\x08").is_none());
        assert!(icns(b"\x89PNG").is_none());
    }

    #[test]
    fn plist_icon_name() {
        let xml = "<dict>\n<key>CFBundleIconFile</key>\n\t<string>icon.icns</string>\n</dict>";
        assert_eq!(
            plist_string(xml, "CFBundleIconFile").as_deref(),
            Some("icon.icns")
        );
        assert_eq!(plist_string(xml, "CFBundleName"), None);
    }

    /// A PE32+ program whose only section holds an icon group of a 48 pixel
    /// 32-bit image and a 256 pixel PNG.
    fn program() -> Vec<u8> {
        const SECTION: u32 = 0x400;
        const RVA: u32 = 0x2000;
        let png = png(256);
        let mut bmp = Vec::new();
        for v in [40u32, 48, 96] {
            bmp.extend(v.to_le_bytes());
        }
        bmp.extend(1u16.to_le_bytes());
        bmp.extend(32u16.to_le_bytes());
        bmp.extend([0; 24]);
        bmp.extend([0xff, 0x00, 0x00, 0xff].repeat(48 * 48));
        // Resource tree: root -> type -> id -> language -> data entry.
        let mut r = vec![0u8; 0x200];
        let dir = |r: &mut Vec<u8>, at: usize, ids: &[(u32, u32)]| {
            r[at + 14..at + 16].copy_from_slice(&(ids.len() as u16).to_le_bytes());
            for (i, (id, to)) in ids.iter().enumerate() {
                let e = at + 16 + i * 8;
                r[e..e + 4].copy_from_slice(&id.to_le_bytes());
                r[e + 4..e + 8].copy_from_slice(&to.to_le_bytes());
            }
        };
        const SUB: u32 = 0x8000_0000;
        dir(
            &mut r,
            0,
            &[(RT_ICON, SUB | 0x30), (RT_GROUP_ICON, SUB | 0x50)],
        );
        dir(&mut r, 0x30, &[(1, SUB | 0x68), (2, SUB | 0x80)]);
        dir(&mut r, 0x50, &[(7, SUB | 0x98)]);
        dir(&mut r, 0x68, &[(1033, 0x100)]);
        dir(&mut r, 0x80, &[(1033, 0x110)]);
        dir(&mut r, 0x98, &[(1033, 0x120)]);
        let mut group = vec![0, 0, 1, 0, 2, 0];
        for (w, id, len) in [(48u8, 1u16, bmp.len()), (0, 2, png.len())] {
            group.extend([w, w, 0, 0, 1, 0, 32, 0]);
            group.extend((len as u32).to_le_bytes());
            group.extend(id.to_le_bytes());
        }
        let mut blobs: Vec<u8> = Vec::new();
        for (leaf, blob) in [(0x100, &bmp), (0x110, &png), (0x120, &group)] {
            let at = RVA + 0x200 + blobs.len() as u32;
            r[leaf..leaf + 4].copy_from_slice(&at.to_le_bytes());
            r[leaf + 4..leaf + 8].copy_from_slice(&(blob.len() as u32).to_le_bytes());
            blobs.extend(blob.iter());
        }
        r.extend(blobs);
        let mut h = vec![0u8; SECTION as usize];
        h[..2].copy_from_slice(b"MZ");
        h[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        h[0x80..0x84].copy_from_slice(b"PE\0\0");
        h[0x86..0x88].copy_from_slice(&1u16.to_le_bytes());
        h[0x94..0x96].copy_from_slice(&240u16.to_le_bytes());
        h[0x98..0x9a].copy_from_slice(&0x20bu16.to_le_bytes());
        let res = 0x98 + 112 + 16;
        h[res..res + 4].copy_from_slice(&RVA.to_le_bytes());
        h[res + 4..res + 8].copy_from_slice(&(r.len() as u32).to_le_bytes());
        let s = 0x98 + 240;
        h[s..s + 5].copy_from_slice(b".rsrc");
        for (o, v) in [
            (8, r.len() as u32),
            (12, RVA),
            (16, r.len() as u32),
            (20, SECTION),
        ] {
            h[s + o..s + o + 4].copy_from_slice(&v.to_le_bytes());
        }
        h.extend(r);
        h
    }

    #[test]
    fn program_icon_prefers_the_largest_image() {
        let exe = program();
        let section = resource_section(&exe).unwrap();
        assert_eq!(section.offset, 0x400);
        let raw = &exe[section.offset as usize..];
        let icon = pe_icon(raw, section.rva, section.dir).unwrap();
        assert_eq!(icon.width(), 256);
        let dir = std::env::temp_dir().join(format!("superba-icon-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("App.exe");
        std::fs::write(&path, &exe).unwrap();
        assert_eq!(load(&path, "app").map(|p| p.width()), Some(256));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_32_bit_image_is_read_bottom_up_and_premultiplied() {
        let mut bmp = Vec::new();
        for v in [40u32, 2, 4] {
            bmp.extend(v.to_le_bytes());
        }
        bmp.extend(1u16.to_le_bytes());
        bmp.extend(32u16.to_le_bytes());
        bmp.extend([0; 24]);
        // Bottom row opaque blue, top row half-transparent red (BGRA).
        bmp.extend([
            255, 0, 0, 255, 255, 0, 0, 255, 0, 0, 255, 128, 0, 0, 255, 128,
        ]);
        let p = dib(&bmp).unwrap().data().to_vec();
        assert_eq!(&p[..4], &[128, 0, 0, 128]);
        assert_eq!(&p[8..12], &[0, 0, 255, 255]);
        bmp[14] = 24;
        assert!(dib(&bmp).is_none());
    }

    #[test]
    fn not_a_program() {
        assert!(resource_section(b"MZ").is_none());
        assert!(resource_section(&[0; 512]).is_none());
        assert!(load(Path::new("/nonexistent/App.exe"), "app").is_none());
        assert!(load(Path::new("/nonexistent/App.app"), "app").is_none());
    }
}
