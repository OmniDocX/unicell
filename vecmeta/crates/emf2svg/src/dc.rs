//! GDI device context state tracked during record playback.

use emf_core::types::*;
use vector_ir::{FillRule, LineCap, LineJoin, Matrix, Point, Rgb};

use crate::text::FontSpec;

/// Pen definition as stored in the object table / device context.
#[derive(Debug, Clone, PartialEq)]
pub struct PenDef {
    pub color: Rgb,
    /// Width in logical units (geometric pens).
    pub width: f64,
    /// Cosmetic pens are always one device pixel wide.
    pub cosmetic: bool,
    pub dash: Option<Vec<f64>>,
    pub cap: LineCap,
    pub join: LineJoin,
}

/// An entry of the EMF object table.
#[derive(Debug, Clone, PartialEq)]
pub enum GdiObject {
    /// `None` means a NULL pen.
    Pen(Option<PenDef>),
    /// `None` means a NULL brush; otherwise a solid color.
    Brush(Option<Rgb>),
    /// A font (for text vectorization).
    Font(FontSpec),
}

/// Mutable drawing state (a GDI DC subset relevant to vector output).
#[derive(Debug, Clone)]
pub struct Dc {
    pub map_mode: u32,
    pub window_org: PointL,
    pub window_ext: SizeL,
    pub viewport_org: PointL,
    pub viewport_ext: SizeL,
    /// World transform as an f64 matrix (source values are f32).
    pub world: Matrix,
    pub pen: Option<PenDef>,
    pub brush: Option<Rgb>,
    pub fill_rule: FillRule,
    pub clockwise: bool,
    pub miter_limit: f64,
    /// Current position in logical units.
    pub cur_pos: Point,
    /// Currently selected font (for text vectorization).
    pub font: FontSpec,
    /// Current text color (SETTEXTCOLOR); defaults to black.
    pub text_color: Rgb,
}

impl Default for Dc {
    fn default() -> Self {
        Self {
            map_mode: MM_TEXT,
            window_org: PointL::default(),
            window_ext: SizeL::new(1, 1),
            viewport_org: PointL::default(),
            viewport_ext: SizeL::new(1, 1),
            world: Matrix::IDENTITY,
            // GDI defaults: BLACK_PEN and WHITE_BRUSH selected.
            pen: Some(PenDef {
                color: Rgb::new(0, 0, 0),
                width: 1.0,
                cosmetic: true,
                dash: None,
                cap: LineCap::Round,
                join: LineJoin::Round,
            }),
            brush: Some(Rgb::new(255, 255, 255)),
            // GDI default polygon fill mode is ALTERNATE.
            fill_rule: FillRule::EvenOdd,
            clockwise: false,
            miter_limit: 10.0,
            cur_pos: Point::new(0.0, 0.0),
            font: FontSpec::default(),
            text_color: Rgb::new(0, 0, 0),
        }
    }
}

impl Dc {
    /// Page-space mapping (logical -> device units) derived from the map
    /// mode and the window/viewport pairs, mirroring `point_cal` in the C
    /// implementation.
    pub fn page_matrix(&self, px_per_mm_x: f64, px_per_mm_y: f64) -> Matrix {
        let (mut sx, mut sy) = match self.map_mode {
            MM_TEXT => (1.0, 1.0),
            MM_LOMETRIC => (px_per_mm_x * 0.1, -px_per_mm_y * 0.1),
            MM_HIMETRIC => (px_per_mm_x * 0.01, -px_per_mm_y * 0.01),
            MM_LOENGLISH => (px_per_mm_x * 0.254, -px_per_mm_y * 0.254),
            MM_HIENGLISH => (px_per_mm_x * 0.0254, -px_per_mm_y * 0.0254),
            MM_TWIPS => (px_per_mm_x * 25.4 / 1440.0, -px_per_mm_y * 25.4 / 1440.0),
            MM_ISOTROPIC | MM_ANISOTROPIC => {
                let sx = if self.window_ext.cx != 0 {
                    self.viewport_ext.cx as f64 / self.window_ext.cx as f64
                } else {
                    1.0
                };
                let sy = if self.window_ext.cy != 0 {
                    self.viewport_ext.cy as f64 / self.window_ext.cy as f64
                } else {
                    1.0
                };
                (sx, sy)
            }
            _ => (1.0, 1.0),
        };
        if self.map_mode == MM_ISOTROPIC {
            // GDI shrinks the larger scale to keep the aspect ratio.
            let m = sx.abs().min(sy.abs());
            if m > 0.0 {
                sx = m * sx.signum();
                sy = m * sy.signum();
            }
        }
        Matrix::new(
            sx,
            0.0,
            0.0,
            sy,
            self.viewport_org.x as f64 - self.window_org.x as f64 * sx,
            self.viewport_org.y as f64 - self.window_org.y as f64 * sy,
        )
    }
}

/// Convert an EMF XFORM (f32) into the f64 IR matrix. Field order matches:
/// GDI computes `x' = x*m11 + y*m21 + dx`, identical to SVG matrix(a b c d e f).
pub fn xform_to_matrix(x: &Xform) -> Matrix {
    Matrix::new(
        x.m11 as f64,
        x.m12 as f64,
        x.m21 as f64,
        x.m22 as f64,
        x.dx as f64,
        x.dy as f64,
    )
}
