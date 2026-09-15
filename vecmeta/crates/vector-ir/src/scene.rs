//! Scene: a flat, ordered list of drawable elements.

use crate::{Fill, Matrix, PathSeg, Point, Stroke};

/// One drawable element: a path with an optional stroke and/or fill,
/// positioned by its own transform matrix.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Element {
    /// Transform mapping the element's coordinates into scene space.
    pub transform: Matrix,
    pub path: Vec<PathSeg>,
    pub stroke: Option<Stroke>,
    pub fill: Option<Fill>,
}

impl Element {
    /// Axis-aligned bounding box in scene space, based on segment anchor
    /// points (control points included, so the box is conservative).
    pub fn bounds(&self) -> Option<(Point, Point)> {
        let mut min = Point::new(f64::INFINITY, f64::INFINITY);
        let mut max = Point::new(f64::NEG_INFINITY, f64::NEG_INFINITY);
        let mut any = false;
        let mut add = |p: Point| {
            let t = self.transform.apply(p);
            min.x = min.x.min(t.x);
            min.y = min.y.min(t.y);
            max.x = max.x.max(t.x);
            max.y = max.y.max(t.y);
            any = true;
        };
        for seg in &self.path {
            match *seg {
                PathSeg::MoveTo(p) | PathSeg::LineTo(p) => add(p),
                PathSeg::CubicTo(c1, c2, p) => {
                    add(c1);
                    add(c2);
                    add(p);
                }
                PathSeg::Arc { rx, ry, end, .. } => {
                    // Conservative: pad end point by the radii.
                    add(Point::new(end.x - rx, end.y - ry));
                    add(Point::new(end.x + rx, end.y + ry));
                }
                PathSeg::Close => {}
            }
        }
        if any {
            Some((min, max))
        } else {
            None
        }
    }
}

/// A complete vector scene.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Scene {
    /// Top-left corner of the view box in scene (user) units.
    pub min_x: f64,
    pub min_y: f64,
    /// Canvas size in scene (user) units.
    pub width: f64,
    pub height: f64,
    pub elements: Vec<Element>,
    /// Non-fatal notes about skipped/unsupported content.
    pub warnings: Vec<String>,
}

impl Scene {
    /// Bounding box of all elements in scene space.
    pub fn bounds(&self) -> Option<(Point, Point)> {
        let mut acc: Option<(Point, Point)> = None;
        for el in &self.elements {
            if let Some((lo, hi)) = el.bounds() {
                acc = Some(match acc {
                    None => (lo, hi),
                    Some((alo, ahi)) => (
                        Point::new(alo.x.min(lo.x), alo.y.min(lo.y)),
                        Point::new(ahi.x.max(hi.x), ahi.y.max(hi.y)),
                    ),
                });
            }
        }
        acc
    }
}
