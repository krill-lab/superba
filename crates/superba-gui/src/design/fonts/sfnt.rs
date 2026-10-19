//! Minimal, unhinted TrueType encoding for CoreText's quadratic outlines.
//! Used only for system fonts with Apple-private `hvgl` outlines. The result
//! lives in memory; it is never cached on disk or shipped with the application.

use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Point {
    pub x: i16,
    pub y: i16,
    pub on_curve: bool,
}

#[derive(Default)]
pub(super) struct Glyph {
    pub contours: Vec<Vec<Point>>,
}

impl Glyph {
    pub fn bounds(&self) -> [i16; 4] {
        let mut points = self.contours.iter().flatten();
        let Some(first) = points.next() else {
            return [0; 4];
        };
        points.fold([first.x, first.y, first.x, first.y], |b, p| {
            [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)]
        })
    }

    pub fn encode(&self, out: &mut Vec<u8>) -> Option<()> {
        if self.contours.is_empty() {
            return Some(());
        }
        out.extend(i16::try_from(self.contours.len()).ok()?.to_be_bytes());
        for bound in self.bounds() {
            out.extend(bound.to_be_bytes());
        }
        let mut count = 0_u16;
        for contour in &self.contours {
            if contour.is_empty() {
                return None;
            }
            count = count.checked_add(u16::try_from(contour.len()).ok()?)?;
            out.extend((count - 1).to_be_bytes());
        }
        out.extend(0_u16.to_be_bytes()); // no hinting instructions
        let mut xs = Vec::new();
        let mut ys = Vec::new();
        let (mut x, mut y) = (0_i16, 0_i16);
        for p in self.contours.iter().flatten() {
            let mut flag = u8::from(p.on_curve);
            delta(p.x.checked_sub(x)?, 0x02, 0x10, &mut flag, &mut xs);
            delta(p.y.checked_sub(y)?, 0x04, 0x20, &mut flag, &mut ys);
            out.push(flag);
            (x, y) = (p.x, p.y);
        }
        out.extend(xs);
        out.extend(ys);
        pad(out);
        Some(())
    }
}

fn delta(value: i16, short: u8, same: u8, flag: &mut u8, out: &mut Vec<u8>) {
    if value == 0 {
        *flag |= same;
    } else if value.unsigned_abs() <= 255 {
        *flag |= short;
        if value > 0 {
            *flag |= same;
        }
        out.push(value.unsigned_abs() as u8);
    } else {
        out.extend(value.to_be_bytes());
    }
}

fn pad(bytes: &mut Vec<u8>) {
    bytes.resize(bytes.len().next_multiple_of(4), 0);
}

fn checksum(bytes: &[u8]) -> u32 {
    bytes.chunks(4).fold(0_u32, |sum, chunk| {
        let mut word = [0; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        sum.wrapping_add(u32::from_be_bytes(word))
    })
}

pub(super) fn put16(bytes: &mut [u8], offset: usize, value: i16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
}

pub(super) fn put32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
}

pub(super) fn assemble(tables: BTreeMap<[u8; 4], Vec<u8>>) -> Option<Vec<u8>> {
    let count = u16::try_from(tables.len()).ok()?;
    let selector = count.checked_ilog2()? as u16;
    let search = 16_u16.checked_shl(selector.into())?;
    let mut out = vec![0; 12 + 16 * usize::from(count)];
    put32(&mut out, 0, 0x0001_0000);
    put16(&mut out, 4, count as i16);
    put16(&mut out, 6, search as i16);
    put16(&mut out, 8, selector as i16);
    put16(&mut out, 10, (count * 16 - search) as i16);
    let mut head = None;
    for (i, (tag, mut bytes)) in tables.into_iter().enumerate() {
        if tag == *b"head" {
            if bytes.len() < 54 {
                return None;
            }
            put32(&mut bytes, 8, 0);
            head = Some(out.len());
        }
        let record = 12 + 16 * i;
        out[record..record + 4].copy_from_slice(&tag);
        put32(&mut out, record + 4, checksum(&bytes));
        let offset = u32::try_from(out.len()).ok()?;
        put32(&mut out, record + 8, offset);
        put32(&mut out, record + 12, u32::try_from(bytes.len()).ok()?);
        out.extend(bytes);
        pad(&mut out);
    }
    let adjustment = 0xb1b0_afba_u32.wrapping_sub(checksum(&out));
    put32(&mut out, head? + 8, adjustment);
    Some(out)
}

#[cfg(test)]
#[path = "../../../tests/unit/font_sfnt.rs"]
mod tests;
