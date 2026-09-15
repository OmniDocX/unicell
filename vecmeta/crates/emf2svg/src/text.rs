//! Parse EMF font (EMR_EXTCREATEFONTINDIRECTW) and text (EMR_EXTTEXTOUTW /
//! EMR_EXTTEXTOUTA) records from their raw payloads and vectorize the text.
//!
//! These records are kept as `EmfRecord::Unknown` by `emf-core` (preserving
//! their exact bytes for the lossless mode); here we decode just the fields
//! needed to reproduce the glyphs as filled outlines.

use glyph2path::{Anchor, TextStyle};
use vector_ir::PathSeg;

/// Record type numbers handled here.
pub const EMR_EXTTEXTOUTA: u32 = 83;
pub const EMR_EXTTEXTOUTW: u32 = 84;
pub const EMR_EXTCREATEFONTINDIRECTW: u32 = 82;

/// A font selected into the device context.
#[derive(Debug, Clone, PartialEq)]
pub struct FontSpec {
    /// Em size in logical units (absolute value of LOGFONT height).
    pub size: f64,
    pub weight: u16,
    pub italic: bool,
    pub family: String,
}

impl Default for FontSpec {
    fn default() -> Self {
        Self {
            size: 16.0,
            weight: 400,
            italic: false,
            family: "sans-serif".to_string(),
        }
    }
}

fn rd_i32(d: &[u8], o: usize) -> Option<i32> {
    d.get(o..o + 4)
        .map(|s| i32::from_le_bytes(s.try_into().unwrap()))
}

fn rd_u32(d: &[u8], o: usize) -> Option<u32> {
    d.get(o..o + 4)
        .map(|s| u32::from_le_bytes(s.try_into().unwrap()))
}

/// Parse an EMR_EXTCREATEFONTINDIRECTW payload (bytes after iType/nSize).
/// Layout: ihFont(u32) then a LOGFONTW (92 bytes).
pub fn parse_font(payload: &[u8]) -> Option<(u32, FontSpec)> {
    let ih = rd_u32(payload, 0)?;
    let lf = &payload.get(4..)?; // LOGFONTW starts here
    let height = rd_i32(lf, 0)?;
    let weight = rd_i32(lf, 16)?;
    let italic = *lf.get(20)? != 0;
    // lfFaceName: 32 UTF-16LE code units at offset 28.
    let face_off = 28;
    let mut name = String::new();
    let mut i = 0;
    while i < 32 {
        let o = face_off + i * 2;
        let Some(u) = lf.get(o..o + 2) else { break };
        let cu = u16::from_le_bytes(u.try_into().unwrap());
        if cu == 0 {
            break;
        }
        name.push(char::from_u32(cu as u32).unwrap_or('?'));
        i += 1;
    }
    if name.is_empty() {
        name = "sans-serif".to_string();
    }
    let size = height.unsigned_abs() as f64;
    Some((
        ih,
        FontSpec {
            size: if size > 0.0 { size } else { 16.0 },
            weight: weight.clamp(1, 1000) as u16,
            italic,
            family: name,
        },
    ))
}

/// Decoded text-out record: baseline reference point and the string.
pub struct TextOut {
    pub x: f64,
    pub y: f64,
    pub text: String,
}

/// Parse an EMR_EXTTEXTOUTW/A payload. `wide` selects UTF-16 vs ANSI string.
/// `offset_base` is the number of bytes (8) that precede the payload in the
/// full record, because EMF string offsets are measured from the record start.
pub fn parse_text_out(payload: &[u8], wide: bool) -> Option<TextOut> {
    // payload layout: rclBounds(16) iGraphicsMode(4) exScale(4) eyScale(4)
    // then EMRTEXT: Reference POINTL(8) nChars(4) offString(4) fOptions(4)
    // rcl(16) offDx(4). Offsets are from record start (8 bytes before payload).
    const EMRTEXT_OFF: usize = 16 + 4 + 4 + 4; // 28
    let ref_x = rd_i32(payload, EMRTEXT_OFF)?;
    let ref_y = rd_i32(payload, EMRTEXT_OFF + 4)?;
    let n_chars = rd_u32(payload, EMRTEXT_OFF + 8)? as usize;
    let off_string = rd_u32(payload, EMRTEXT_OFF + 12)? as usize;
    // Convert record-relative offset to payload-relative (record starts 8
    // bytes earlier than the payload).
    let str_off = off_string.checked_sub(8)?;
    let text = if wide {
        let mut s = String::with_capacity(n_chars);
        for i in 0..n_chars {
            let o = str_off + i * 2;
            let u = payload.get(o..o + 2)?;
            let cu = u16::from_le_bytes(u.try_into().unwrap());
            s.push(char::from_u32(cu as u32).unwrap_or('?'));
        }
        s
    } else {
        let bytes = payload.get(str_off..str_off + n_chars)?;
        bytes.iter().map(|&b| b as char).collect()
    };
    Some(TextOut {
        x: ref_x as f64,
        y: ref_y as f64,
        text,
    })
}

/// Vectorize a decoded text-out with the given font into filled outlines.
pub fn vectorize(t: &TextOut, font: &FontSpec) -> Vec<PathSeg> {
    let style = TextStyle {
        family: font.family.clone(),
        size: font.size,
        weight: font.weight,
        italic: font.italic,
    };
    glyph2path::text_to_paths(&style, &t.text, t.x, t.y, Anchor::Start)
}
