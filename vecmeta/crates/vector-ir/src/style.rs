//! Stroke and fill styles.

/// Opaque RGB color (classic EMF pens/brushes carry no alpha).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Format as `#rrggbb`.
    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LineCap {
    #[default]
    Flat,
    Round,
    Square,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LineJoin {
    #[default]
    Miter,
    Round,
    Bevel,
}

/// Stroke style. `width` is expressed in the element's own (untransformed)
/// coordinate space.
#[derive(Debug, Clone, PartialEq)]
pub struct Stroke {
    pub color: Rgb,
    pub width: f64,
    pub dash: Option<Vec<f64>>,
    pub cap: LineCap,
    pub join: LineJoin,
    pub miter_limit: f64,
    pub alpha: u8,
}

impl Default for Stroke {
    fn default() -> Self {
        Self {
            color: Rgb::default(),
            width: 1.0,
            dash: None,
            cap: LineCap::default(),
            join: LineJoin::default(),
            miter_limit: 4.0,
            alpha: 255,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FillRule {
    #[default]
    NonZero,
    EvenOdd,
}

/// One stop of a gradient: `offset` in [0,1], color and alpha.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GradientStop {
    pub offset: f64,
    pub color: Rgb,
    pub alpha: u8,
}

/// A linear gradient in the element's local (userspace) coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct LinearGradient {
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
    pub stops: Vec<GradientStop>,
}

/// Fill style. `color` is the solid (or gradient-average) color used by
/// classic EMF; `alpha` and `gradient` carry the richer paint reproduced via
/// EMF+ when present.
#[derive(Debug, Clone, PartialEq)]
pub struct Fill {
    pub color: Rgb,
    pub rule: FillRule,
    pub alpha: u8,
    pub gradient: Option<LinearGradient>,
}

impl Fill {
    /// Opaque solid fill.
    pub fn solid(color: Rgb, rule: FillRule) -> Self {
        Self {
            color,
            rule,
            alpha: 255,
            gradient: None,
        }
    }
}
