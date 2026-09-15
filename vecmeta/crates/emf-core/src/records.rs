//! Strongly typed EMF records relevant to vector conversion.
//!
//! Records that carry no vector information (text, bitmaps, palettes, ...)
//! are preserved as [`EmfRecord::Unknown`] with their raw payload so the
//! record stream stays complete.

use crate::types::*;

/// Parsed EMF file header (EMR_HEADER, [MS-EMF] 2.3.4.2).
#[derive(Debug, Clone, PartialEq)]
pub struct EmfHeader {
    /// Inclusive-inclusive bounds of the drawing in device units.
    pub bounds: RectL,
    /// Frame rectangle in 0.01 mm units.
    pub frame: RectL,
    pub n_bytes: u32,
    pub n_records: u32,
    pub n_handles: u16,
    /// Reference device size in pixels.
    pub device: SizeL,
    /// Reference device size in millimeters.
    pub millimeters: SizeL,
}

impl Default for EmfHeader {
    fn default() -> Self {
        Self {
            bounds: RectL::default(),
            frame: RectL::default(),
            n_bytes: 0,
            n_records: 0,
            n_handles: 0,
            // 96 DPI-ish reference device.
            device: SizeL::new(1920, 1080),
            millimeters: SizeL::new(508, 286),
        }
    }
}

impl EmfHeader {
    /// Horizontal pixels per millimeter of the reference device.
    pub fn px_per_mm_x(&self) -> f64 {
        if self.millimeters.cx != 0 {
            self.device.cx as f64 / self.millimeters.cx as f64
        } else {
            96.0 / 25.4
        }
    }

    /// Vertical pixels per millimeter of the reference device.
    pub fn px_per_mm_y(&self) -> f64 {
        if self.millimeters.cy != 0 {
            self.device.cy as f64 / self.millimeters.cy as f64
        } else {
            96.0 / 25.4
        }
    }
}

/// One EMF record. 16-bit point variants are widened to 32 bits at parse
/// time; the writer re-emits the compact form when it fits.
#[derive(Debug, Clone, PartialEq)]
pub enum EmfRecord {
    Header(EmfHeader),
    Eof,

    // --- State ---
    SetMapMode(u32),
    SetWindowExtEx(SizeL),
    SetWindowOrgEx(PointL),
    SetViewportExtEx(SizeL),
    SetViewportOrgEx(PointL),
    SetPolyfillMode(u32),
    SetMiterLimit(f32),
    SetArcDirection(u32),
    MoveToEx(PointL),
    SaveDc,
    RestoreDc(i32),

    // --- Transform ---
    SetWorldTransform(Xform),
    ModifyWorldTransform {
        xform: Xform,
        mode: u32,
    },

    // --- Object management ---
    CreatePen {
        ih: u32,
        style: u32,
        width: i32,
        color: ColorRef,
    },
    ExtCreatePen {
        ih: u32,
        style: u32,
        width: u32,
        brush_style: u32,
        color: ColorRef,
        hatch: u32,
        style_entries: Vec<u32>,
    },
    CreateBrushIndirect {
        ih: u32,
        style: u32,
        color: ColorRef,
        hatch: u32,
    },
    SelectObject(u32),
    DeleteObject(u32),

    // --- Drawing ---
    Polybezier(Vec<PointL>),
    Polygon(Vec<PointL>),
    Polyline(Vec<PointL>),
    PolybezierTo(Vec<PointL>),
    PolylineTo(Vec<PointL>),
    Polypolyline {
        counts: Vec<u32>,
        pts: Vec<PointL>,
    },
    Polypolygon {
        counts: Vec<u32>,
        pts: Vec<PointL>,
    },
    PolyDraw {
        pts: Vec<PointL>,
        types: Vec<u8>,
    },
    LineTo(PointL),
    AngleArc {
        center: PointL,
        radius: u32,
        start_angle: f32,
        sweep_angle: f32,
    },
    Ellipse(RectL),
    Rectangle(RectL),
    RoundRect {
        rect: RectL,
        corner: SizeL,
    },
    Arc {
        rect: RectL,
        start: PointL,
        end: PointL,
    },
    ArcTo {
        rect: RectL,
        start: PointL,
        end: PointL,
    },
    Chord {
        rect: RectL,
        start: PointL,
        end: PointL,
    },
    Pie {
        rect: RectL,
        start: PointL,
        end: PointL,
    },

    // --- Path bracket ---
    BeginPath,
    EndPath,
    CloseFigure,
    AbortPath,
    FillPath(RectL),
    StrokePath(RectL),
    StrokeAndFillPath(RectL),

    /// EMR_COMMENT payload (all bytes after the `cbData` length field). Used
    /// for embedded EMF+ streams and the byte-lossless source metadata.
    Comment(Vec<u8>),

    /// Any record we do not model (text, bitmaps, clipping, EMF+, ...).
    /// `data` is the payload after iType/nSize.
    Unknown {
        itype: u32,
        data: Vec<u8>,
    },
}

impl EmfRecord {
    /// Human readable name of the record type (for warnings/verbose output).
    pub fn type_name(&self) -> &'static str {
        match self {
            EmfRecord::Header(_) => "EMR_HEADER",
            EmfRecord::Eof => "EMR_EOF",
            EmfRecord::SetMapMode(_) => "EMR_SETMAPMODE",
            EmfRecord::SetWindowExtEx(_) => "EMR_SETWINDOWEXTEX",
            EmfRecord::SetWindowOrgEx(_) => "EMR_SETWINDOWORGEX",
            EmfRecord::SetViewportExtEx(_) => "EMR_SETVIEWPORTEXTEX",
            EmfRecord::SetViewportOrgEx(_) => "EMR_SETVIEWPORTORGEX",
            EmfRecord::SetPolyfillMode(_) => "EMR_SETPOLYFILLMODE",
            EmfRecord::SetMiterLimit(_) => "EMR_SETMITERLIMIT",
            EmfRecord::SetArcDirection(_) => "EMR_SETARCDIRECTION",
            EmfRecord::MoveToEx(_) => "EMR_MOVETOEX",
            EmfRecord::SaveDc => "EMR_SAVEDC",
            EmfRecord::RestoreDc(_) => "EMR_RESTOREDC",
            EmfRecord::SetWorldTransform(_) => "EMR_SETWORLDTRANSFORM",
            EmfRecord::ModifyWorldTransform { .. } => "EMR_MODIFYWORLDTRANSFORM",
            EmfRecord::CreatePen { .. } => "EMR_CREATEPEN",
            EmfRecord::ExtCreatePen { .. } => "EMR_EXTCREATEPEN",
            EmfRecord::CreateBrushIndirect { .. } => "EMR_CREATEBRUSHINDIRECT",
            EmfRecord::SelectObject(_) => "EMR_SELECTOBJECT",
            EmfRecord::DeleteObject(_) => "EMR_DELETEOBJECT",
            EmfRecord::Polybezier(_) => "EMR_POLYBEZIER",
            EmfRecord::Polygon(_) => "EMR_POLYGON",
            EmfRecord::Polyline(_) => "EMR_POLYLINE",
            EmfRecord::PolybezierTo(_) => "EMR_POLYBEZIERTO",
            EmfRecord::PolylineTo(_) => "EMR_POLYLINETO",
            EmfRecord::Polypolyline { .. } => "EMR_POLYPOLYLINE",
            EmfRecord::Polypolygon { .. } => "EMR_POLYPOLYGON",
            EmfRecord::PolyDraw { .. } => "EMR_POLYDRAW",
            EmfRecord::LineTo(_) => "EMR_LINETO",
            EmfRecord::AngleArc { .. } => "EMR_ANGLEARC",
            EmfRecord::Ellipse(_) => "EMR_ELLIPSE",
            EmfRecord::Rectangle(_) => "EMR_RECTANGLE",
            EmfRecord::RoundRect { .. } => "EMR_ROUNDRECT",
            EmfRecord::Arc { .. } => "EMR_ARC",
            EmfRecord::ArcTo { .. } => "EMR_ARCTO",
            EmfRecord::Chord { .. } => "EMR_CHORD",
            EmfRecord::Pie { .. } => "EMR_PIE",
            EmfRecord::BeginPath => "EMR_BEGINPATH",
            EmfRecord::EndPath => "EMR_ENDPATH",
            EmfRecord::CloseFigure => "EMR_CLOSEFIGURE",
            EmfRecord::AbortPath => "EMR_ABORTPATH",
            EmfRecord::FillPath(_) => "EMR_FILLPATH",
            EmfRecord::StrokePath(_) => "EMR_STROKEPATH",
            EmfRecord::StrokeAndFillPath(_) => "EMR_STROKEANDFILLPATH",
            EmfRecord::Comment(_) => "EMR_COMMENT",
            EmfRecord::Unknown { .. } => "EMR_UNKNOWN",
        }
    }

    /// Build an EMR_COMMENT carrying a private OMNI payload of the given kind.
    /// Layout: `[magic u32][kind u32][payload...]`.
    pub fn omni_comment(kind: u32, payload: &[u8]) -> EmfRecord {
        let mut data = Vec::with_capacity(8 + payload.len());
        data.extend_from_slice(&OMNI_COMMENT_MAGIC.to_le_bytes());
        data.extend_from_slice(&kind.to_le_bytes());
        data.extend_from_slice(payload);
        EmfRecord::Comment(data)
    }

    /// If this record is an OMNI private comment, return `(kind, payload)`.
    pub fn as_omni_payload(&self) -> Option<(u32, &[u8])> {
        let EmfRecord::Comment(data) = self else {
            return None;
        };
        if data.len() < 8 {
            return None;
        }
        let magic = u32::from_le_bytes(data[0..4].try_into().ok()?);
        if magic != OMNI_COMMENT_MAGIC {
            return None;
        }
        let kind = u32::from_le_bytes(data[4..8].try_into().ok()?);
        Some((kind, &data[8..]))
    }
}
