//! EMF binary reader: header parsing plus record framing with strict
//! bounds checking (robust against the `emf-corrupted` test corpus).

use crate::records::{EmfHeader, EmfRecord};
use crate::types::*;
use crate::EmfError;

/// A parsed EMF file: the header plus every following record in order.
#[derive(Debug, Clone)]
pub struct EmfFile {
    pub header: EmfHeader,
    /// All records after the header, EOF included.
    pub records: Vec<EmfRecord>,
}

impl EmfFile {
    /// Parse a complete EMF byte buffer.
    pub fn parse(data: &[u8]) -> Result<EmfFile, EmfError> {
        if data.len() < 88 {
            return Err(EmfError::Truncated("file smaller than EMF header"));
        }
        let mut header = None;
        let mut records = Vec::new();
        let mut off = 0usize;
        while off < data.len() {
            if data.len() - off < 8 {
                return Err(EmfError::Truncated("record frame header"));
            }
            let itype = u32::from_le_bytes(data[off..off + 4].try_into().unwrap());
            let nsize = u32::from_le_bytes(data[off + 4..off + 8].try_into().unwrap()) as usize;
            if nsize < 8 || !nsize.is_multiple_of(4) {
                return Err(EmfError::BadRecordSize { itype, nsize });
            }
            if nsize > data.len() - off {
                return Err(EmfError::Truncated("record payload out of bounds"));
            }
            let payload = &data[off + 8..off + nsize];
            if header.is_none() {
                if itype != EMR_HEADER {
                    return Err(EmfError::NotEmf);
                }
                header = Some(parse_header(payload)?);
            } else {
                let rec = parse_record(itype, payload)?;
                let eof = matches!(rec, EmfRecord::Eof);
                records.push(rec);
                if eof {
                    break;
                }
            }
            off += nsize;
        }
        let header = header.ok_or(EmfError::NotEmf)?;
        if !matches!(records.last(), Some(EmfRecord::Eof)) {
            return Err(EmfError::Truncated("missing EMR_EOF"));
        }
        Ok(EmfFile { header, records })
    }
}

/// Little-endian cursor over a record payload.
pub(crate) struct Cur<'a> {
    d: &'a [u8],
    pos: usize,
}

impl<'a> Cur<'a> {
    pub fn new(d: &'a [u8]) -> Self {
        Self { d, pos: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.d.len() - self.pos
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], EmfError> {
        if self.remaining() < n {
            return Err(EmfError::Truncated("record field out of bounds"));
        }
        let s = &self.d[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    pub fn u32(&mut self) -> Result<u32, EmfError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    pub fn i32(&mut self) -> Result<i32, EmfError> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    pub fn f32(&mut self) -> Result<f32, EmfError> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    pub fn i16(&mut self) -> Result<i16, EmfError> {
        Ok(i16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    pub fn u16(&mut self) -> Result<u16, EmfError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    pub fn u8(&mut self) -> Result<u8, EmfError> {
        Ok(self.take(1)?[0])
    }

    pub fn point_l(&mut self) -> Result<PointL, EmfError> {
        Ok(PointL::new(self.i32()?, self.i32()?))
    }

    pub fn point_s(&mut self) -> Result<PointL, EmfError> {
        Ok(PointL::new(self.i16()? as i32, self.i16()? as i32))
    }

    pub fn size_l(&mut self) -> Result<SizeL, EmfError> {
        Ok(SizeL::new(self.i32()?, self.i32()?))
    }

    pub fn rect_l(&mut self) -> Result<RectL, EmfError> {
        Ok(RectL::new(
            self.i32()?,
            self.i32()?,
            self.i32()?,
            self.i32()?,
        ))
    }

    pub fn xform(&mut self) -> Result<Xform, EmfError> {
        Ok(Xform {
            m11: self.f32()?,
            m12: self.f32()?,
            m21: self.f32()?,
            m22: self.f32()?,
            dx: self.f32()?,
            dy: self.f32()?,
        })
    }

    pub fn color(&mut self) -> Result<ColorRef, EmfError> {
        let r = self.u8()?;
        let g = self.u8()?;
        let b = self.u8()?;
        let _reserved = self.u8()?;
        Ok(ColorRef::new(r, g, b))
    }
}

fn parse_header(payload: &[u8]) -> Result<EmfHeader, EmfError> {
    let mut c = Cur::new(payload);
    let bounds = c.rect_l()?;
    let frame = c.rect_l()?;
    let signature = c.u32()?;
    if signature != ENHMETA_SIGNATURE {
        return Err(EmfError::NotEmf);
    }
    let _version = c.u32()?;
    let n_bytes = c.u32()?;
    let n_records = c.u32()?;
    let n_handles = c.u16()?;
    let _reserved = c.u16()?;
    let _n_description = c.u32()?;
    let _off_description = c.u32()?;
    let _n_pal_entries = c.u32()?;
    let device = c.size_l()?;
    let millimeters = c.size_l()?;
    Ok(EmfHeader {
        bounds,
        frame,
        n_bytes,
        n_records,
        n_handles,
        device,
        millimeters,
    })
}

/// Read `n` points, 32-bit wide.
fn points_l(c: &mut Cur, n: u32) -> Result<Vec<PointL>, EmfError> {
    if n as usize > c.remaining() / 8 {
        return Err(EmfError::Truncated("point array out of bounds"));
    }
    (0..n).map(|_| c.point_l()).collect()
}

/// Read `n` points, 16-bit wide (widened to i32).
fn points_s(c: &mut Cur, n: u32) -> Result<Vec<PointL>, EmfError> {
    if n as usize > c.remaining() / 4 {
        return Err(EmfError::Truncated("point16 array out of bounds"));
    }
    (0..n).map(|_| c.point_s()).collect()
}

fn poly(payload: &[u8], wide: bool) -> Result<Vec<PointL>, EmfError> {
    let mut c = Cur::new(payload);
    let _bounds = c.rect_l()?;
    let n = c.u32()?;
    if wide {
        points_l(&mut c, n)
    } else {
        points_s(&mut c, n)
    }
}

fn polypoly(payload: &[u8], wide: bool) -> Result<(Vec<u32>, Vec<PointL>), EmfError> {
    let mut c = Cur::new(payload);
    let _bounds = c.rect_l()?;
    let n_polys = c.u32()?;
    let n_pts = c.u32()?;
    if n_polys as usize > c.remaining() / 4 {
        return Err(EmfError::Truncated("poly count array out of bounds"));
    }
    let counts: Vec<u32> = (0..n_polys).map(|_| c.u32()).collect::<Result<_, _>>()?;
    let total: u64 = counts.iter().map(|&v| v as u64).sum();
    if total != n_pts as u64 {
        return Err(EmfError::Malformed("poly counts do not sum to point count"));
    }
    let pts = if wide {
        points_l(&mut c, n_pts)?
    } else {
        points_s(&mut c, n_pts)?
    };
    Ok((counts, pts))
}

fn polydraw(payload: &[u8], wide: bool) -> Result<(Vec<PointL>, Vec<u8>), EmfError> {
    let mut c = Cur::new(payload);
    let _bounds = c.rect_l()?;
    let n = c.u32()?;
    let pts = if wide {
        points_l(&mut c, n)?
    } else {
        points_s(&mut c, n)?
    };
    if n as usize > c.remaining() {
        return Err(EmfError::Truncated("polydraw type array out of bounds"));
    }
    let types: Vec<u8> = (0..n).map(|_| c.u8()).collect::<Result<_, _>>()?;
    Ok((pts, types))
}

fn arc_like(payload: &[u8]) -> Result<(RectL, PointL, PointL), EmfError> {
    let mut c = Cur::new(payload);
    Ok((c.rect_l()?, c.point_l()?, c.point_l()?))
}

pub(crate) fn parse_record(itype: u32, payload: &[u8]) -> Result<EmfRecord, EmfError> {
    let mut c = Cur::new(payload);
    let rec = match itype {
        EMR_EOF => EmfRecord::Eof,
        EMR_SETMAPMODE => EmfRecord::SetMapMode(c.u32()?),
        EMR_SETWINDOWEXTEX => EmfRecord::SetWindowExtEx(c.size_l()?),
        EMR_SETWINDOWORGEX => EmfRecord::SetWindowOrgEx(c.point_l()?),
        EMR_SETVIEWPORTEXTEX => EmfRecord::SetViewportExtEx(c.size_l()?),
        EMR_SETVIEWPORTORGEX => EmfRecord::SetViewportOrgEx(c.point_l()?),
        EMR_SETPOLYFILLMODE => EmfRecord::SetPolyfillMode(c.u32()?),
        EMR_SETMITERLIMIT => EmfRecord::SetMiterLimit(c.f32()?),
        EMR_SETARCDIRECTION => EmfRecord::SetArcDirection(c.u32()?),
        EMR_MOVETOEX => EmfRecord::MoveToEx(c.point_l()?),
        EMR_SAVEDC => EmfRecord::SaveDc,
        EMR_RESTOREDC => EmfRecord::RestoreDc(c.i32()?),
        EMR_SETWORLDTRANSFORM => EmfRecord::SetWorldTransform(c.xform()?),
        EMR_MODIFYWORLDTRANSFORM => EmfRecord::ModifyWorldTransform {
            xform: c.xform()?,
            mode: c.u32()?,
        },
        EMR_CREATEPEN => EmfRecord::CreatePen {
            ih: c.u32()?,
            style: c.u32()?,
            width: {
                let w = c.point_l()?;
                w.x
            },
            color: c.color()?,
        },
        EMR_EXTCREATEPEN => {
            let ih = c.u32()?;
            let _off_bmi = c.u32()?;
            let _cb_bmi = c.u32()?;
            let _off_bits = c.u32()?;
            let _cb_bits = c.u32()?;
            let style = c.u32()?;
            let width = c.u32()?;
            let brush_style = c.u32()?;
            let color = c.color()?;
            let hatch = c.u32()?;
            let n_entries = c.u32()?;
            if n_entries as usize > c.remaining() / 4 {
                return Err(EmfError::Truncated("pen style entries out of bounds"));
            }
            let style_entries = (0..n_entries).map(|_| c.u32()).collect::<Result<_, _>>()?;
            EmfRecord::ExtCreatePen {
                ih,
                style,
                width,
                brush_style,
                color,
                hatch,
                style_entries,
            }
        }
        EMR_CREATEBRUSHINDIRECT => EmfRecord::CreateBrushIndirect {
            ih: c.u32()?,
            style: c.u32()?,
            color: c.color()?,
            hatch: c.u32()?,
        },
        EMR_SELECTOBJECT => EmfRecord::SelectObject(c.u32()?),
        EMR_DELETEOBJECT => EmfRecord::DeleteObject(c.u32()?),
        EMR_POLYBEZIER => EmfRecord::Polybezier(poly(payload, true)?),
        EMR_POLYGON => EmfRecord::Polygon(poly(payload, true)?),
        EMR_POLYLINE => EmfRecord::Polyline(poly(payload, true)?),
        EMR_POLYBEZIERTO => EmfRecord::PolybezierTo(poly(payload, true)?),
        EMR_POLYLINETO => EmfRecord::PolylineTo(poly(payload, true)?),
        EMR_POLYBEZIER16 => EmfRecord::Polybezier(poly(payload, false)?),
        EMR_POLYGON16 => EmfRecord::Polygon(poly(payload, false)?),
        EMR_POLYLINE16 => EmfRecord::Polyline(poly(payload, false)?),
        EMR_POLYBEZIERTO16 => EmfRecord::PolybezierTo(poly(payload, false)?),
        EMR_POLYLINETO16 => EmfRecord::PolylineTo(poly(payload, false)?),
        EMR_POLYPOLYLINE => {
            let (counts, pts) = polypoly(payload, true)?;
            EmfRecord::Polypolyline { counts, pts }
        }
        EMR_POLYPOLYGON => {
            let (counts, pts) = polypoly(payload, true)?;
            EmfRecord::Polypolygon { counts, pts }
        }
        EMR_POLYPOLYLINE16 => {
            let (counts, pts) = polypoly(payload, false)?;
            EmfRecord::Polypolyline { counts, pts }
        }
        EMR_POLYPOLYGON16 => {
            let (counts, pts) = polypoly(payload, false)?;
            EmfRecord::Polypolygon { counts, pts }
        }
        EMR_POLYDRAW => {
            let (pts, types) = polydraw(payload, true)?;
            EmfRecord::PolyDraw { pts, types }
        }
        EMR_POLYDRAW16 => {
            let (pts, types) = polydraw(payload, false)?;
            EmfRecord::PolyDraw { pts, types }
        }
        EMR_LINETO => EmfRecord::LineTo(c.point_l()?),
        EMR_ANGLEARC => EmfRecord::AngleArc {
            center: c.point_l()?,
            radius: c.u32()?,
            start_angle: c.f32()?,
            sweep_angle: c.f32()?,
        },
        EMR_ELLIPSE => EmfRecord::Ellipse(c.rect_l()?),
        EMR_RECTANGLE => EmfRecord::Rectangle(c.rect_l()?),
        EMR_ROUNDRECT => EmfRecord::RoundRect {
            rect: c.rect_l()?,
            corner: c.size_l()?,
        },
        EMR_ARC => {
            let (rect, start, end) = arc_like(payload)?;
            EmfRecord::Arc { rect, start, end }
        }
        EMR_ARCTO => {
            let (rect, start, end) = arc_like(payload)?;
            EmfRecord::ArcTo { rect, start, end }
        }
        EMR_CHORD => {
            let (rect, start, end) = arc_like(payload)?;
            EmfRecord::Chord { rect, start, end }
        }
        EMR_PIE => {
            let (rect, start, end) = arc_like(payload)?;
            EmfRecord::Pie { rect, start, end }
        }
        EMR_BEGINPATH => EmfRecord::BeginPath,
        EMR_ENDPATH => EmfRecord::EndPath,
        EMR_CLOSEFIGURE => EmfRecord::CloseFigure,
        EMR_ABORTPATH => EmfRecord::AbortPath,
        EMR_FILLPATH => EmfRecord::FillPath(c.rect_l()?),
        EMR_STROKEPATH => EmfRecord::StrokePath(c.rect_l()?),
        EMR_STROKEANDFILLPATH => EmfRecord::StrokeAndFillPath(c.rect_l()?),
        EMR_COMMENT => {
            // cbData, then the comment bytes.
            let cb = c.u32()? as usize;
            if cb > c.remaining() {
                return Err(EmfError::Truncated("comment data out of bounds"));
            }
            EmfRecord::Comment(payload[4..4 + cb].to_vec())
        }
        _ => EmfRecord::Unknown {
            itype,
            data: payload.to_vec(),
        },
    };
    Ok(rec)
}
