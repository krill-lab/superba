//! The tray icon's pixels (assets/icon/tray, rendered by
//! `scripts/icons/render-tray.sh` from assets/icon/superba-tray*.svg).
//!
//! - Windows: superba.exe carries `superba-tray.ico` as icon resource
//!   [`WINDOWS_RESOURCE_ID`] (16 to 64 px, every notification-area scale);
//!   the PNGs are the fallback for a build without resources.
//! - macOS: a black template image that macOS tints for the menu bar.
//! - Linux: the coloured PNGs; the StatusNotifierItem host picks a size.

/// An icon as straight RGBA pixels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rgba {
    /// Width and height in pixels.
    pub size: u32,
    /// `size * size * 4` bytes, row by row.
    pub rgba: Vec<u8>,
}

/// The tray icon's resource id in superba.exe (build.rs).
pub const WINDOWS_RESOURCE_ID: u16 = 2;

/// Coloured icons as `(pixel size, PNG bytes)`, smallest first.
pub const COLOUR: [(u32, &[u8]); 8] = [
    (
        16,
        include_bytes!("../../../../assets/icon/tray/superba-tray-16.png"),
    ),
    (
        20,
        include_bytes!("../../../../assets/icon/tray/superba-tray-20.png"),
    ),
    (
        22,
        include_bytes!("../../../../assets/icon/tray/superba-tray-22.png"),
    ),
    (
        24,
        include_bytes!("../../../../assets/icon/tray/superba-tray-24.png"),
    ),
    (
        32,
        include_bytes!("../../../../assets/icon/tray/superba-tray-32.png"),
    ),
    (
        40,
        include_bytes!("../../../../assets/icon/tray/superba-tray-40.png"),
    ),
    (
        48,
        include_bytes!("../../../../assets/icon/tray/superba-tray-48.png"),
    ),
    (
        64,
        include_bytes!("../../../../assets/icon/tray/superba-tray-64.png"),
    ),
];

/// The macOS menu-bar template image (18 pt at 2x: 36 px).
pub const TEMPLATE_2X: &[u8] =
    include_bytes!("../../../../assets/icon/tray/superba-tray-template-36.png");

/// Decode a PNG.
pub fn decode(png: &[u8]) -> Option<Rgba> {
    let icon = eframe::icon_data::from_png_bytes(png).ok()?;
    (icon.width == icon.height).then_some(Rgba {
        size: icon.width,
        rgba: icon.rgba,
    })
}

/// The coloured icon closest to `px` (the next larger one when there is no
/// exact size, so the system scales down).
pub fn colour(px: u32) -> Rgba {
    let (_, png) = COLOUR
        .iter()
        .find(|(size, _)| *size >= px)
        .unwrap_or(&COLOUR[COLOUR.len() - 1]);
    decode(png).expect("the embedded tray icons are valid PNGs")
}

/// Every coloured size (a StatusNotifierItem host picks one).
pub fn colour_all() -> Vec<Rgba> {
    COLOUR.iter().filter_map(|(_, png)| decode(png)).collect()
}

/// The macOS template image.
pub fn template() -> Rgba {
    decode(TEMPLATE_2X).expect("the embedded template icon is a valid PNG")
}

/// RGBA to the ARGB32 (network byte order) of a StatusNotifierItem pixmap.
pub fn to_argb32(rgba: &[u8]) -> Vec<u8> {
    let (pixels, _) = rgba.as_chunks::<4>();
    pixels
        .iter()
        .flat_map(|&[r, g, b, a]| [a, r, g, b])
        .collect()
}
