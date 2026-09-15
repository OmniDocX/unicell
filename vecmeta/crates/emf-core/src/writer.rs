//! EMF binary writer: serializes records and back-fills the header
//! (nBytes / nRecords) once the stream is complete.

use crate::records::{EmfHeader, EmfRecord};
use crate::types::*;

/// Byte buffer with little-endian push helpers.
#[derive(Default)]
struct Buf(Vec<u8>);

impl Buf {
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn i32(&mut self, v: i32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f32(&mut self, v: f32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn point_l(&mut self, p: PointL) {
        self.i32(p.x);
        self.i32(p.y);
    }
    fn size_l(&mut self, s: SizeL) {
        self.i32(s.cx);
        self.i32(s.cy);
    }
    fn rect_l(&mut self, r: RectL) {
        self.i32(r.left);
        self.i32(r.top);
        self.i32(r.right);
        self.i32(r.bottom);
    }
    fn xform(&mut self, x: Xform) {
        self.f32(x.m11);
        self.f32(x.m12);
        self.f32(x.m21);
        self.f32(x.m22);
        self.f32(x.dx);
        self.f32(x.dy);
    }
    fn color(&mut self, c: ColorRef) {
        self.0.extend_from_slice(&[c.r, c.g, c.b, 0]);
    }
}

fn point_bounds(pts: &[PointL]) -> RectL {
    let mut r = RectL::new(i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for p in pts {
        r.left = r.left.min(p.x);
        r.top = r.top.min(p.y);
        r.right = r.right.max(p.x);
        r.bottom = r.bottom.max(p.y);
    }
    if pts.is_empty() {
        RectL::default()
    } else {
        r
    }
}

/// Serialize one record (excluding the header) to bytes.
fn record_bytes(rec: &EmfRecord) -> (u32, Vec<u8>) {
    let mut b = Buf::default();
    let itype = match rec {
        EmfRecord::Header(_) => unreachable!("header is written by EmfWriter"),
        EmfRecord::Eof => {
            b.u32(0); // nPalEntries
            b.u32(16); // offPalEntries
            b.u32(20); // nSizeLast (total EOF record size)
            EMR_EOF
        }
        EmfRecord::SetMapMode(v) => {
            b.u32(*v);
            EMR_SETMAPMODE
        }
        EmfRecord::SetWindowExtEx(s) => {
            b.size_l(*s);
            EMR_SETWINDOWEXTEX
        }
        EmfRecord::SetWindowOrgEx(p) => {
            b.point_l(*p);
            EMR_SETWINDOWORGEX
        }
        EmfRecord::SetViewportExtEx(s) => {
            b.size_l(*s);
            EMR_SETVIEWPORTEXTEX
        }
        EmfRecord::SetViewportOrgEx(p) => {
            b.point_l(*p);
            EMR_SETVIEWPORTORGEX
        }
        EmfRecord::SetPolyfillMode(v) => {
            b.u32(*v);
            EMR_SETPOLYFILLMODE
        }
        EmfRecord::SetMiterLimit(v) => {
            b.f32(*v);
            EMR_SETMITERLIMIT
        }
        EmfRecord::SetArcDirection(v) => {
            b.u32(*v);
            EMR_SETARCDIRECTION
        }
        EmfRecord::MoveToEx(p) => {
            b.point_l(*p);
            EMR_MOVETOEX
        }
        EmfRecord::SaveDc => EMR_SAVEDC,
        EmfRecord::RestoreDc(v) => {
            b.i32(*v);
            EMR_RESTOREDC
        }
        EmfRecord::SetWorldTransform(x) => {
            b.xform(*x);
            EMR_SETWORLDTRANSFORM
        }
        EmfRecord::ModifyWorldTransform { xform, mode } => {
            b.xform(*xform);
            b.u32(*mode);
            EMR_MODIFYWORLDTRANSFORM
        }
        EmfRecord::CreatePen {
            ih,
            style,
            width,
            color,
        } => {
            b.u32(*ih);
            b.u32(*style);
            b.point_l(PointL::new(*width, 0));
            b.color(*color);
            EMR_CREATEPEN
        }
        EmfRecord::ExtCreatePen {
            ih,
            style,
            width,
            brush_style,
            color,
            hatch,
            style_entries,
        } => {
            b.u32(*ih);
            b.u32(0); // offBmi
            b.u32(0); // cbBmi
            b.u32(0); // offBits
            b.u32(0); // cbBits
            b.u32(*style);
            b.u32(*width);
            b.u32(*brush_style);
            b.color(*color);
            b.u32(*hatch);
            b.u32(style_entries.len() as u32);
            for e in style_entries {
                b.u32(*e);
            }
            EMR_EXTCREATEPEN
        }
        EmfRecord::CreateBrushIndirect {
            ih,
            style,
            color,
            hatch,
        } => {
            b.u32(*ih);
            b.u32(*style);
            b.color(*color);
            b.u32(*hatch);
            EMR_CREATEBRUSHINDIRECT
        }
        EmfRecord::SelectObject(ih) => {
            b.u32(*ih);
            EMR_SELECTOBJECT
        }
        EmfRecord::DeleteObject(ih) => {
            b.u32(*ih);
            EMR_DELETEOBJECT
        }
        EmfRecord::Polybezier(pts) => poly_body(&mut b, pts, EMR_POLYBEZIER),
        EmfRecord::Polygon(pts) => poly_body(&mut b, pts, EMR_POLYGON),
        EmfRecord::Polyline(pts) => poly_body(&mut b, pts, EMR_POLYLINE),
        EmfRecord::PolybezierTo(pts) => poly_body(&mut b, pts, EMR_POLYBEZIERTO),
        EmfRecord::PolylineTo(pts) => poly_body(&mut b, pts, EMR_POLYLINETO),
        EmfRecord::Polypolyline { counts, pts } => {
            polypoly_body(&mut b, counts, pts, EMR_POLYPOLYLINE)
        }
        EmfRecord::Polypolygon { counts, pts } => {
            polypoly_body(&mut b, counts, pts, EMR_POLYPOLYGON)
        }
        EmfRecord::PolyDraw { pts, types } => {
            b.rect_l(point_bounds(pts));
            b.u32(pts.len() as u32);
            for p in pts {
                b.point_l(*p);
            }
            for t in types {
                b.0.push(*t);
            }
            while b.0.len() % 4 != 0 {
                b.0.push(0);
            }
            EMR_POLYDRAW
        }
        EmfRecord::LineTo(p) => {
            b.point_l(*p);
            EMR_LINETO
        }
        EmfRecord::AngleArc {
            center,
            radius,
            start_angle,
            sweep_angle,
        } => {
            b.point_l(*center);
            b.u32(*radius);
            b.f32(*start_angle);
            b.f32(*sweep_angle);
            EMR_ANGLEARC
        }
        EmfRecord::Ellipse(r) => {
            b.rect_l(*r);
            EMR_ELLIPSE
        }
        EmfRecord::Rectangle(r) => {
            b.rect_l(*r);
            EMR_RECTANGLE
        }
        EmfRecord::RoundRect { rect, corner } => {
            b.rect_l(*rect);
            b.size_l(*corner);
            EMR_ROUNDRECT
        }
        EmfRecord::Arc { rect, start, end } => arc_body(&mut b, rect, start, end, EMR_ARC),
        EmfRecord::ArcTo { rect, start, end } => arc_body(&mut b, rect, start, end, EMR_ARCTO),
        EmfRecord::Chord { rect, start, end } => arc_body(&mut b, rect, start, end, EMR_CHORD),
        EmfRecord::Pie { rect, start, end } => arc_body(&mut b, rect, start, end, EMR_PIE),
        EmfRecord::BeginPath => EMR_BEGINPATH,
        EmfRecord::EndPath => EMR_ENDPATH,
        EmfRecord::CloseFigure => EMR_CLOSEFIGURE,
        EmfRecord::AbortPath => EMR_ABORTPATH,
        EmfRecord::FillPath(r) => {
            b.rect_l(*r);
            EMR_FILLPATH
        }
        EmfRecord::StrokePath(r) => {
            b.rect_l(*r);
            EMR_STROKEPATH
        }
        EmfRecord::StrokeAndFillPath(r) => {
            b.rect_l(*r);
            EMR_STROKEANDFILLPATH
        }
        EmfRecord::Comment(data) => {
            b.u32(data.len() as u32);
            b.0.extend_from_slice(data);
            while b.0.len() % 4 != 0 {
                b.0.push(0);
            }
            EMR_COMMENT
        }
        EmfRecord::Unknown { itype, data } => {
            b.0.extend_from_slice(data);
            *itype
        }
    };
    (itype, b.0)
}

fn poly_body(b: &mut Buf, pts: &[PointL], itype: u32) -> u32 {
    b.rect_l(point_bounds(pts));
    b.u32(pts.len() as u32);
    for p in pts {
        b.point_l(*p);
    }
    itype
}

fn polypoly_body(b: &mut Buf, counts: &[u32], pts: &[PointL], itype: u32) -> u32 {
    b.rect_l(point_bounds(pts));
    b.u32(counts.len() as u32);
    b.u32(pts.len() as u32);
    for c in counts {
        b.u32(*c);
    }
    for p in pts {
        b.point_l(*p);
    }
    itype
}

fn arc_body(b: &mut Buf, rect: &RectL, start: &PointL, end: &PointL, itype: u32) -> u32 {
    b.rect_l(*rect);
    b.point_l(*start);
    b.point_l(*end);
    itype
}

/// Serialize a complete EMF file: header + records (EOF must be last).
pub fn write_emf(header: &EmfHeader, records: &[EmfRecord]) -> Vec<u8> {
    // Header record: 88 bytes fixed part, no description.
    let mut hdr = Buf::default();
    hdr.u32(EMR_HEADER);
    hdr.u32(88);
    hdr.rect_l(header.bounds);
    hdr.rect_l(header.frame);
    hdr.u32(ENHMETA_SIGNATURE);
    hdr.u32(0x0001_0000); // nVersion
    hdr.u32(0); // nBytes (patched below)
    hdr.u32(0); // nRecords (patched below)
    hdr.u16(header.n_handles);
    hdr.u16(0); // sReserved
    hdr.u32(0); // nDescription
    hdr.u32(0); // offDescription
    hdr.u32(0); // nPalEntries
    hdr.size_l(header.device);
    hdr.size_l(header.millimeters);

    let mut out = hdr.0;
    let mut n_records: u32 = 1;
    for rec in records {
        let (itype, body) = record_bytes(rec);
        let mut frame = Buf::default();
        frame.u32(itype);
        frame.u32((8 + body.len()) as u32);
        out.extend_from_slice(&frame.0);
        out.extend_from_slice(&body);
        n_records += 1;
    }

    let n_bytes = out.len() as u32;
    out[48..52].copy_from_slice(&n_bytes.to_le_bytes());
    out[52..56].copy_from_slice(&n_records.to_le_bytes());
    out
}
