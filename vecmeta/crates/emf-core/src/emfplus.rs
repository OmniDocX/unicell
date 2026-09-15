//! Minimal EMF+ (EMF Plus) comment block for carrying advanced paint that
//! classic EMF cannot express: per-element ARGB alpha and linear gradients.
//!
//! The block is a well-formed EMF+ stream embedded in an `EMR_COMMENT`
//! (identifier `EMR_COMMENT_EMFPLUS`): an `EmfPlusHeader`, one private paint
//! table record, and `EmfPlusEndOfFile`. EMF+-aware readers skip the private
//! record by its size; classic viewers ignore the comment entirely and use
//! the solid-color fallback drawn with normal EMF records.

use crate::types::EMR_COMMENT_EMFPLUS;

const EMFPLUS_HEADER: u16 = 0x4001;
const EMFPLUS_EOF: u16 = 0x4002;
/// Private record type (outside the standard 0x4001..0x403A range) holding the
/// per-element paint table.
const EMFPLUS_OMNI_PAINT: u16 = 0x40FF;
const EMFPLUS_VERSION: u32 = 0xDBC0_1002;

/// A gradient stop: offset in [0,1] and RGBA.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlusStop {
    pub offset: f32,
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

/// Advanced paint for one element.
#[derive(Debug, Clone, PartialEq)]
pub enum PlusPaint {
    /// Solid fill color with alpha.
    Solid { r: u8, g: u8, b: u8, a: u8 },
    /// Linear gradient fill in element-local coordinates.
    Linear {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        stops: Vec<PlusStop>,
    },
    /// Stroke color with alpha.
    StrokeSolid { r: u8, g: u8, b: u8, a: u8 },
}

/// One paint-table entry, keyed by the element's emission index.
#[derive(Debug, Clone, PartialEq)]
pub struct PlusEntry {
    pub index: u32,
    pub paint: PlusPaint,
}

struct W(Vec<u8>);
impl W {
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f32(&mut self, v: f32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
}

fn plus_record(out: &mut W, rtype: u16, flags: u16, data: &[u8]) {
    // Record header: Type(u16) Flags(u16) Size(u32, incl 12-byte header)
    // DataSize(u32, data length excluding trailing padding).
    let mut padded = data.len();
    while !padded.is_multiple_of(4) {
        padded += 1;
    }
    out.u16(rtype);
    out.u16(flags);
    out.u32((12 + padded) as u32);
    out.u32(data.len() as u32);
    out.0.extend_from_slice(data);
    for _ in data.len()..padded {
        out.0.push(0);
    }
}

/// Encode a paint table into an EMF+ comment payload (including the leading
/// EMF+ comment identifier).
pub fn encode(entries: &[PlusEntry]) -> Vec<u8> {
    let mut out = W(Vec::new());
    // EMR_COMMENT public identifier for embedded EMF+ data.
    out.u32(EMR_COMMENT_EMFPLUS);

    // EmfPlusHeader.
    let mut hdr = W(Vec::new());
    hdr.u32(EMFPLUS_VERSION);
    hdr.u32(1); // EmfPlusFlags: dual (classic EMF present)
    hdr.u32(96); // LogicalDpiX
    hdr.u32(96); // LogicalDpiY
    plus_record(&mut out, EMFPLUS_HEADER, 0, &hdr.0);

    // Private paint table.
    let mut tbl = W(Vec::new());
    tbl.u32(entries.len() as u32);
    for e in entries {
        tbl.u32(e.index);
        match &e.paint {
            PlusPaint::Solid { r, g, b, a } => {
                tbl.u32(0); // kind = solid
                tbl.0.extend_from_slice(&[*r, *g, *b, *a]);
            }
            PlusPaint::Linear {
                x1,
                y1,
                x2,
                y2,
                stops,
            } => {
                tbl.u32(1); // kind = linear
                tbl.f32(*x1);
                tbl.f32(*y1);
                tbl.f32(*x2);
                tbl.f32(*y2);
                tbl.u32(stops.len() as u32);
                for s in stops {
                    tbl.f32(s.offset);
                    tbl.0.extend_from_slice(&[s.r, s.g, s.b, s.a]);
                }
            }
            PlusPaint::StrokeSolid { r, g, b, a } => {
                tbl.u32(2); // kind = stroke solid
                tbl.0.extend_from_slice(&[*r, *g, *b, *a]);
            }
        }
    }
    plus_record(&mut out, EMFPLUS_OMNI_PAINT, 0, &tbl.0);

    // EmfPlusEndOfFile.
    plus_record(&mut out, EMFPLUS_EOF, 0, &[]);
    out.0
}

struct R<'a> {
    d: &'a [u8],
    p: usize,
}
impl<'a> R<'a> {
    fn u16(&mut self) -> Option<u16> {
        let v = self.d.get(self.p..self.p + 2)?;
        self.p += 2;
        Some(u16::from_le_bytes(v.try_into().unwrap()))
    }
    fn u32(&mut self) -> Option<u32> {
        let v = self.d.get(self.p..self.p + 4)?;
        self.p += 4;
        Some(u32::from_le_bytes(v.try_into().unwrap()))
    }
    fn f32(&mut self) -> Option<f32> {
        let v = self.d.get(self.p..self.p + 4)?;
        self.p += 4;
        Some(f32::from_le_bytes(v.try_into().unwrap()))
    }
    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let v = self.d.get(self.p..self.p + n)?;
        self.p += n;
        Some(v)
    }
}

/// Decode a paint table from an EMF+ comment payload, if it contains one.
pub fn decode(comment_data: &[u8]) -> Option<Vec<PlusEntry>> {
    let mut r = R {
        d: comment_data,
        p: 0,
    };
    if r.u32()? != EMR_COMMENT_EMFPLUS {
        return None;
    }
    // Walk EMF+ records looking for the private paint table.
    while r.p + 12 <= comment_data.len() {
        let rtype = r.u16()?;
        let _flags = r.u16()?;
        let size = r.u32()? as usize;
        let data_size = r.u32()? as usize;
        if size < 12 {
            return None;
        }
        let body_start = r.p;
        if rtype == EMFPLUS_OMNI_PAINT {
            let mut t = R {
                d: comment_data.get(body_start..body_start + data_size)?,
                p: 0,
            };
            return decode_table(&mut t);
        }
        // Advance to the next record (size includes the 12-byte header).
        r.p = body_start + (size - 12);
    }
    None
}

fn decode_table(t: &mut R) -> Option<Vec<PlusEntry>> {
    let count = t.u32()?;
    let mut out = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let index = t.u32()?;
        let kind = t.u32()?;
        let paint = match kind {
            0 => {
                let c = t.bytes(4)?;
                PlusPaint::Solid {
                    r: c[0],
                    g: c[1],
                    b: c[2],
                    a: c[3],
                }
            }
            1 => {
                let x1 = t.f32()?;
                let y1 = t.f32()?;
                let x2 = t.f32()?;
                let y2 = t.f32()?;
                let n = t.u32()?;
                let mut stops = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    let offset = t.f32()?;
                    let c = t.bytes(4)?;
                    stops.push(PlusStop {
                        offset,
                        r: c[0],
                        g: c[1],
                        b: c[2],
                        a: c[3],
                    });
                }
                PlusPaint::Linear {
                    x1,
                    y1,
                    x2,
                    y2,
                    stops,
                }
            }
            2 => {
                let c = t.bytes(4)?;
                PlusPaint::StrokeSolid {
                    r: c[0],
                    g: c[1],
                    b: c[2],
                    a: c[3],
                }
            }
            _ => return None,
        };
        out.push(PlusEntry { index, paint });
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paint_table_roundtrip() {
        let entries = vec![
            PlusEntry {
                index: 0,
                paint: PlusPaint::Solid {
                    r: 10,
                    g: 20,
                    b: 30,
                    a: 128,
                },
            },
            PlusEntry {
                index: 3,
                paint: PlusPaint::Linear {
                    x1: 1.0,
                    y1: 2.0,
                    x2: 3.5,
                    y2: 4.0,
                    stops: vec![
                        PlusStop {
                            offset: 0.0,
                            r: 6,
                            g: 182,
                            b: 212,
                            a: 255,
                        },
                        PlusStop {
                            offset: 1.0,
                            r: 15,
                            g: 118,
                            b: 110,
                            a: 200,
                        },
                    ],
                },
            },
        ];
        let bytes = encode(&entries);
        let back = decode(&bytes).expect("decode");
        assert_eq!(entries, back);
    }
}
