//! Path geometry primitives.

/// A 2D point with double precision coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

/// One path segment, mirroring the expressive power shared by SVG path data
/// and EMF path bracket records.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PathSeg {
    /// Start a new subpath at the given point.
    MoveTo(Point),
    /// Straight line to the given point.
    LineTo(Point),
    /// Cubic Bezier with two control points and an end point.
    CubicTo(Point, Point, Point),
    /// Elliptical arc (SVG `A` semantics) ending at `end`.
    Arc {
        rx: f64,
        ry: f64,
        /// x-axis rotation in degrees.
        rot: f64,
        large: bool,
        sweep: bool,
        end: Point,
    },
    /// Close the current subpath.
    Close,
}

impl PathSeg {
    /// End point of this segment, if it has an explicit one.
    pub fn end_point(&self) -> Option<Point> {
        match self {
            PathSeg::MoveTo(p) | PathSeg::LineTo(p) => Some(*p),
            PathSeg::CubicTo(_, _, p) => Some(*p),
            PathSeg::Arc { end, .. } => Some(*end),
            PathSeg::Close => None,
        }
    }
}
