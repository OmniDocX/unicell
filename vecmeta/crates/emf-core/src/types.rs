//! Basic EMF wire types and constants ([MS-EMF] 2.1, 2.2).

/// 32-bit signed point (POINTL).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PointL {
    pub x: i32,
    pub y: i32,
}

impl PointL {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// 32-bit signed size (SIZEL).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SizeL {
    pub cx: i32,
    pub cy: i32,
}

impl SizeL {
    pub const fn new(cx: i32, cy: i32) -> Self {
        Self { cx, cy }
    }
}

/// 32-bit signed rectangle (RECTL).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RectL {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl RectL {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub fn width(&self) -> i32 {
        self.right - self.left
    }

    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }
}

/// World transform matrix (XFORM, six float32).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Xform {
    pub m11: f32,
    pub m12: f32,
    pub m21: f32,
    pub m22: f32,
    pub dx: f32,
    pub dy: f32,
}

impl Default for Xform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Xform {
    pub const IDENTITY: Xform = Xform {
        m11: 1.0,
        m12: 0.0,
        m21: 0.0,
        m22: 1.0,
        dx: 0.0,
        dy: 0.0,
    };
}

/// COLORREF: red, green, blue, reserved bytes in wire order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ColorRef {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl ColorRef {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

// --- Record type numbers ([MS-EMF] 2.1.1) ---------------------------------
pub const EMR_HEADER: u32 = 1;
pub const EMR_POLYBEZIER: u32 = 2;
pub const EMR_POLYGON: u32 = 3;
pub const EMR_POLYLINE: u32 = 4;
pub const EMR_POLYBEZIERTO: u32 = 5;
pub const EMR_POLYLINETO: u32 = 6;
pub const EMR_POLYPOLYLINE: u32 = 7;
pub const EMR_POLYPOLYGON: u32 = 8;
pub const EMR_SETWINDOWEXTEX: u32 = 9;
pub const EMR_SETWINDOWORGEX: u32 = 10;
pub const EMR_SETVIEWPORTEXTEX: u32 = 11;
pub const EMR_SETVIEWPORTORGEX: u32 = 12;
pub const EMR_EOF: u32 = 14;
pub const EMR_SETMAPMODE: u32 = 17;
pub const EMR_SETPOLYFILLMODE: u32 = 19;
pub const EMR_COMMENT: u32 = 70;
pub const EMR_MOVETOEX: u32 = 27;
pub const EMR_SAVEDC: u32 = 33;
pub const EMR_RESTOREDC: u32 = 34;
pub const EMR_SETWORLDTRANSFORM: u32 = 35;
pub const EMR_MODIFYWORLDTRANSFORM: u32 = 36;
pub const EMR_SELECTOBJECT: u32 = 37;
pub const EMR_CREATEPEN: u32 = 38;
pub const EMR_CREATEBRUSHINDIRECT: u32 = 39;
pub const EMR_DELETEOBJECT: u32 = 40;
pub const EMR_ANGLEARC: u32 = 41;
pub const EMR_ELLIPSE: u32 = 42;
pub const EMR_RECTANGLE: u32 = 43;
pub const EMR_ROUNDRECT: u32 = 44;
pub const EMR_ARC: u32 = 45;
pub const EMR_CHORD: u32 = 46;
pub const EMR_PIE: u32 = 47;
pub const EMR_LINETO: u32 = 54;
pub const EMR_ARCTO: u32 = 55;
pub const EMR_POLYDRAW: u32 = 56;
pub const EMR_SETARCDIRECTION: u32 = 57;
pub const EMR_SETMITERLIMIT: u32 = 58;
pub const EMR_BEGINPATH: u32 = 59;
pub const EMR_ENDPATH: u32 = 60;
pub const EMR_CLOSEFIGURE: u32 = 61;
pub const EMR_FILLPATH: u32 = 62;
pub const EMR_STROKEANDFILLPATH: u32 = 63;
pub const EMR_STROKEPATH: u32 = 64;
pub const EMR_ABORTPATH: u32 = 68;
pub const EMR_EXTTEXTOUTA: u32 = 83;
pub const EMR_EXTTEXTOUTW: u32 = 84;
pub const EMR_POLYBEZIER16: u32 = 85;
pub const EMR_POLYGON16: u32 = 86;
pub const EMR_POLYLINE16: u32 = 87;
pub const EMR_POLYBEZIERTO16: u32 = 88;
pub const EMR_POLYLINETO16: u32 = 89;
pub const EMR_POLYPOLYLINE16: u32 = 90;
pub const EMR_POLYPOLYGON16: u32 = 91;
pub const EMR_POLYDRAW16: u32 = 92;
pub const EMR_EXTCREATEPEN: u32 = 95;
pub const EMR_SMALLTEXTOUT: u32 = 108;

// --- Map modes -------------------------------------------------------------
pub const MM_TEXT: u32 = 1;
pub const MM_LOMETRIC: u32 = 2;
pub const MM_HIMETRIC: u32 = 3;
pub const MM_LOENGLISH: u32 = 4;
pub const MM_HIENGLISH: u32 = 5;
pub const MM_TWIPS: u32 = 6;
pub const MM_ISOTROPIC: u32 = 7;
pub const MM_ANISOTROPIC: u32 = 8;

// --- Polygon fill modes ------------------------------------------------------
pub const ALTERNATE: u32 = 1;
pub const WINDING: u32 = 2;

// --- Arc directions ----------------------------------------------------------
pub const AD_COUNTERCLOCKWISE: u32 = 1;
pub const AD_CLOCKWISE: u32 = 2;

// --- Pen styles ---------------------------------------------------------------
pub const PS_SOLID: u32 = 0x0;
pub const PS_DASH: u32 = 0x1;
pub const PS_DOT: u32 = 0x2;
pub const PS_DASHDOT: u32 = 0x3;
pub const PS_DASHDOTDOT: u32 = 0x4;
pub const PS_NULL: u32 = 0x5;
pub const PS_INSIDEFRAME: u32 = 0x6;
pub const PS_USERSTYLE: u32 = 0x7;
pub const PS_ALTERNATE: u32 = 0x8;
pub const PS_STYLE_MASK: u32 = 0x0000_000F;

pub const PS_ENDCAP_ROUND: u32 = 0x0000_0000;
pub const PS_ENDCAP_SQUARE: u32 = 0x0000_0100;
pub const PS_ENDCAP_FLAT: u32 = 0x0000_0200;
pub const PS_ENDCAP_MASK: u32 = 0x0000_0F00;

pub const PS_JOIN_ROUND: u32 = 0x0000_0000;
pub const PS_JOIN_BEVEL: u32 = 0x0000_1000;
pub const PS_JOIN_MITER: u32 = 0x0000_2000;
pub const PS_JOIN_MASK: u32 = 0x0000_F000;

pub const PS_COSMETIC: u32 = 0x0000_0000;
pub const PS_GEOMETRIC: u32 = 0x0001_0000;
pub const PS_TYPE_MASK: u32 = 0x000F_0000;

// --- Brush styles ---------------------------------------------------------------
pub const BS_SOLID: u32 = 0;
pub const BS_NULL: u32 = 1;
pub const BS_HATCHED: u32 = 2;

// --- Stock objects (SELECTOBJECT with the high bit set) --------------------------
pub const STOCK_OBJECT_FLAG: u32 = 0x8000_0000;
pub const WHITE_BRUSH: u32 = 0;
pub const LTGRAY_BRUSH: u32 = 1;
pub const GRAY_BRUSH: u32 = 2;
pub const DKGRAY_BRUSH: u32 = 3;
pub const BLACK_BRUSH: u32 = 4;
pub const NULL_BRUSH: u32 = 5;
pub const WHITE_PEN: u32 = 6;
pub const BLACK_PEN: u32 = 7;
pub const NULL_PEN: u32 = 8;

// --- PolyDraw point types ----------------------------------------------------------
pub const PT_CLOSEFIGURE: u8 = 0x01;
pub const PT_LINETO: u8 = 0x02;
pub const PT_BEZIERTO: u8 = 0x04;
pub const PT_MOVETO: u8 = 0x06;

// --- ModifyWorldTransform modes -----------------------------------------------------
pub const MWT_IDENTITY: u32 = 1;
pub const MWT_LEFTMULTIPLY: u32 = 2;
pub const MWT_RIGHTMULTIPLY: u32 = 3;
pub const MWT_SET: u32 = 4;

/// EMF header signature (" EMF").
pub const ENHMETA_SIGNATURE: u32 = 0x464D_4520;

// --- Comment identifiers ([MS-EMF] 2.3.3) ---------------------------------
/// Public comment identifier for embedded EMF+ data ("EMF+").
pub const EMR_COMMENT_EMFPLUS: u32 = 0x2B46_4D45;
/// Private comment magic used to embed the original source document for the
/// byte-lossless round-trip mode ("OMNI").
pub const OMNI_COMMENT_MAGIC: u32 = 0x494E_4D4F;
/// Sub-tag: the embedded payload is the original SVG document (UTF-8).
pub const OMNI_KIND_SVG: u32 = 1;
