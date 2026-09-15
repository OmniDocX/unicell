//! Emit an EMF record stream from the vector [`Scene`] IR.
//!
//! Each element becomes: SETWORLDTRANSFORM, pen/brush creation + selection,
//! a BEGINPATH..ENDPATH bracket, and a stroke/fill paint record. Coordinates
//! are integers, so fractional path data is scaled up by a configurable
//! factor and compensated in the world transform.

use std::f64::consts::PI;

use emf_core::emfplus::{PlusEntry, PlusPaint, PlusStop};
use emf_core::types::*;
use emf_core::{EmfHeader, EmfRecord};
use kurbo::{Arc as KArc, PathEl, Point as KPoint, SvgArc, Vec2};
use vector_ir::{Element, FillRule, LineCap, LineJoin, Matrix, PathSeg, Point, Rgb, Scene, Stroke};

/// Options controlling EMF emission.
#[derive(Debug, Clone, Copy)]
pub struct EmitOptions {
    /// Upper bound on the fixed-point factor. The emitter picks the largest
    /// power of two not exceeding this that keeps every coordinate within a
    /// safe int32 range, maximizing precision automatically.
    pub scale: f64,
    /// Flatness tolerance (device units) for arc -> bezier conversion.
    pub arc_tolerance: f64,
    /// Byte-lossless mode: embed the original SVG document in the EMF so the
    /// reverse conversion can reproduce it exactly.
    pub lossless: bool,
}

impl Default for EmitOptions {
    fn default() -> Self {
        Self {
            // 2^24: high enough that a drawing spanning thousands of units
            // still gets sub-micron fixed-point resolution.
            scale: 16_777_216.0,
            arc_tolerance: 0.01,
            lossless: false,
        }
    }
}

struct Emitter {
    opts: EmitOptions,
    scale: f64,
    records: Vec<EmfRecord>,
    next_handle: u32,
    max_handle: u32,
    // Deduplication caches (definition -> handle).
    pens: Vec<(EmfRecord, u32)>,
    brushes: Vec<(EmfRecord, u32)>,
    cur_pen: Option<u32>,
    cur_brush: Option<u32>,
    cur_fill_rule: Option<FillRule>,
    // Device-space bounding box accumulator.
    min: Point,
    max: Point,
    has_bounds: bool,
    // Count of emitted elements; used to key the EMF+ paint table.
    emitted: u32,
    // Advanced paint (alpha/gradient) collected for the EMF+ block.
    plus: Vec<PlusEntry>,
}

/// Largest absolute device-space coordinate produced by an element, using the
/// same arc->bezier flattening as emission so the value is stable when arcs
/// are later re-read as beziers.
fn element_max_abs(el: &Element, arc_tol: f64) -> f64 {
    let ms = el.transform.mean_scale().max(1e-9);
    let tol = arc_tol / ms;
    let mut m = 0.0f64;
    let mut acc = |p: Point| {
        let d = el.transform.apply(p);
        m = m.max(d.x.abs()).max(d.y.abs());
    };
    let mut cur = Point::new(0.0, 0.0);
    for seg in &el.path {
        match *seg {
            PathSeg::MoveTo(p) | PathSeg::LineTo(p) => {
                acc(p);
                cur = p;
            }
            PathSeg::CubicTo(c1, c2, p) => {
                acc(c1);
                acc(c2);
                acc(p);
                cur = p;
            }
            PathSeg::Arc {
                rx,
                ry,
                rot,
                large,
                sweep,
                end,
            } => {
                for (c1, c2, p) in arc_to_beziers(cur, rx, ry, rot, large, sweep, end, tol) {
                    acc(c1);
                    acc(c2);
                    acc(p);
                }
                cur = end;
            }
            PathSeg::Close => {}
        }
    }
    m
}

/// Largest power-of-two fixed-point factor S such that `cmax * S` stays inside
/// a safe int32 range and `S <= opts.scale`.
fn choose_scale(cmax: f64, cap: f64) -> f64 {
    let limit = (1u32 << 30) as f64;
    let cmax = cmax.max(1.0);
    let cap = cap.max(1.0);
    let mut s = 1.0f64;
    while s * 2.0 <= cap && cmax * (s * 2.0) <= limit && s < (1u64 << 28) as f64 {
        s *= 2.0;
    }
    s
}

/// Convert a scene into a complete EMF byte buffer.
pub fn scene_to_emf(scene: &Scene, opts: EmitOptions) -> Vec<u8> {
    let (header, records) = scene_to_records(scene, opts);
    emf_core::write_emf(&header, &records)
}

/// Build the EMF header and record list for a scene (EOF included). Exposed so
/// callers can inject extra records (e.g. the byte-lossless source comment)
/// before serialization.
pub fn scene_to_records(scene: &Scene, opts: EmitOptions) -> (EmfHeader, Vec<EmfRecord>) {
    // Device-space extent: declared view box unioned with actual geometry so
    // the fixed-point factor never overflows int32 yet stays stable across
    // round trips. Geometry bounds use the SAME arc->bezier flattening the
    // emitter uses, so an arc and its bezier form (produced on the next hop)
    // yield an identical extent and therefore the same factor.
    let mut cmax = 1.0f64;
    for v in [
        scene.min_x,
        scene.min_y,
        scene.min_x + scene.width,
        scene.min_y + scene.height,
    ] {
        cmax = cmax.max(v.abs());
    }
    for el in &scene.elements {
        cmax = cmax.max(element_max_abs(el, opts.arc_tolerance));
    }
    let scale = choose_scale(cmax.ceil(), opts.scale);

    let mut em = Emitter {
        opts,
        scale,
        records: Vec::new(),
        next_handle: 1,
        max_handle: 0,
        pens: Vec::new(),
        brushes: Vec::new(),
        cur_pen: None,
        cur_brush: None,
        cur_fill_rule: None,
        min: Point::new(f64::INFINITY, f64::INFINITY),
        max: Point::new(f64::NEG_INFINITY, f64::NEG_INFINITY),
        has_bounds: false,
        emitted: 0,
        plus: Vec::new(),
    };

    // Global page mapping: logical units are device units times `scale`, so a
    // 1:scale viewport/window ratio maps them back. This keeps every world
    // transform identity (exact) instead of a lossy 32-bit XFORM matrix.
    if scale != 1.0 {
        let s = scale as i32;
        em.records.push(EmfRecord::SetMapMode(MM_ANISOTROPIC));
        em.records.push(EmfRecord::SetWindowExtEx(SizeL::new(s, s)));
        em.records
            .push(EmfRecord::SetViewportExtEx(SizeL::new(1, 1)));
    }

    for el in &scene.elements {
        em.emit_element(el);
    }
    // Emit the EMF+ advanced-paint block (alpha/gradients) before EOF.
    if !em.plus.is_empty() {
        let payload = emf_core::emfplus::encode(&em.plus);
        em.records.push(EmfRecord::Comment(payload));
    }
    em.records.push(EmfRecord::Eof);

    // Header bounds (device units): prefer the scene's declared view box.
    let bounds = if scene.width > 0.0 && scene.height > 0.0 {
        RectL::new(
            scene.min_x.floor() as i32,
            scene.min_y.floor() as i32,
            (scene.min_x + scene.width).ceil() as i32,
            (scene.min_y + scene.height).ceil() as i32,
        )
    } else if em.has_bounds {
        RectL::new(
            em.min.x.floor() as i32,
            em.min.y.floor() as i32,
            em.max.x.ceil() as i32,
            em.max.y.ceil() as i32,
        )
    } else {
        RectL::new(0, 0, 1, 1)
    };

    // Reference device: 96 DPI over the bounds, expressed in 0.01 mm frame.
    let px_per_mm = 96.0 / 25.4;
    let frame = RectL::new(
        (bounds.left as f64 / px_per_mm * 100.0) as i32,
        (bounds.top as f64 / px_per_mm * 100.0) as i32,
        (bounds.right as f64 / px_per_mm * 100.0) as i32,
        (bounds.bottom as f64 / px_per_mm * 100.0) as i32,
    );

    let header = EmfHeader {
        bounds,
        frame,
        n_bytes: 0,
        n_records: 0,
        n_handles: (em.max_handle + 1).min(u16::MAX as u32) as u16,
        device: SizeL::new(1920, 1080),
        millimeters: SizeL::new((1920.0 / px_per_mm) as i32, (1080.0 / px_per_mm) as i32),
    };

    (header, em.records)
}

impl Emitter {
    fn emit_element(&mut self, el: &Element) {
        if el.path.is_empty() || (el.stroke.is_none() && el.fill.is_none()) {
            return;
        }
        self.accumulate_bounds(el);

        // Record advanced paint (alpha/gradient) for the EMF+ block, keyed by
        // this element's emission index.
        let idx = self.emitted;
        self.emitted += 1;
        if let Some(fill) = &el.fill {
            if let Some(g) = &fill.gradient {
                // Element-level fill alpha is folded into the stop alphas so a
                // single gradient entry captures the composite paint. Gradient
                // endpoints are stored in DEVICE space (transform applied) so
                // the reader can map them into whatever local space its
                // reconstructed elements use.
                let fa = fill.alpha as u32;
                let p1 = el.transform.apply(Point::new(g.x1, g.y1));
                let p2 = el.transform.apply(Point::new(g.x2, g.y2));
                self.plus.push(PlusEntry {
                    index: idx,
                    paint: PlusPaint::Linear {
                        x1: p1.x as f32,
                        y1: p1.y as f32,
                        x2: p2.x as f32,
                        y2: p2.y as f32,
                        stops: g
                            .stops
                            .iter()
                            .map(|s| PlusStop {
                                offset: s.offset as f32,
                                r: s.color.r,
                                g: s.color.g,
                                b: s.color.b,
                                a: ((s.alpha as u32 * fa) / 255) as u8,
                            })
                            .collect(),
                    },
                });
            } else if fill.alpha != 255 {
                self.plus.push(PlusEntry {
                    index: idx,
                    paint: PlusPaint::Solid {
                        r: fill.color.r,
                        g: fill.color.g,
                        b: fill.color.b,
                        a: fill.alpha,
                    },
                });
            }
        }
        if let Some(stroke) = &el.stroke {
            if stroke.alpha != 255 {
                self.plus.push(PlusEntry {
                    index: idx,
                    paint: PlusPaint::StrokeSolid {
                        r: stroke.color.r,
                        g: stroke.color.g,
                        b: stroke.color.b,
                        a: stroke.alpha,
                    },
                });
            }
        }

        // Fill rule.
        if let Some(fill) = &el.fill {
            if self.cur_fill_rule != Some(fill.rule) {
                let mode = match fill.rule {
                    FillRule::EvenOdd => ALTERNATE,
                    FillRule::NonZero => WINDING,
                };
                self.records.push(EmfRecord::SetPolyfillMode(mode));
                self.cur_fill_rule = Some(fill.rule);
            }
        }

        // The element transform is baked into the coordinates (below), so pen
        // widths must be scaled by the transform's mean scale as well.
        let ms = {
            let m = el.transform.mean_scale();
            if m > 0.0 && m.is_finite() {
                m
            } else {
                1.0
            }
        };

        // Select pen and brush.
        self.select_pen(el.stroke.as_ref(), ms * self.scale);
        self.select_brush(el.fill.as_ref().map(|f| f.color));

        // Path bracket: bake `transform` into every point, then apply the
        // global fixed-point factor. World transform stays identity.
        self.records.push(EmfRecord::BeginPath);
        self.emit_path(&el.path, &el.transform);
        self.records.push(EmfRecord::EndPath);

        let empty = RectL::default();
        match (el.stroke.is_some(), el.fill.is_some()) {
            (true, true) => self.records.push(EmfRecord::StrokeAndFillPath(empty)),
            (true, false) => self.records.push(EmfRecord::StrokePath(empty)),
            (false, true) => self.records.push(EmfRecord::FillPath(empty)),
            (false, false) => {}
        }
    }

    fn accumulate_bounds(&mut self, el: &Element) {
        if let Some((lo, hi)) = el.bounds() {
            self.min.x = self.min.x.min(lo.x);
            self.min.y = self.min.y.min(lo.y);
            self.max.x = self.max.x.max(hi.x);
            self.max.y = self.max.y.max(hi.y);
            self.has_bounds = true;
        }
    }

    fn alloc_handle(&mut self) -> u32 {
        let h = self.next_handle;
        self.next_handle += 1;
        self.max_handle = self.max_handle.max(h);
        h
    }

    fn select_pen(&mut self, stroke: Option<&Stroke>, scale: f64) {
        let Some(s) = stroke else {
            // NULL pen (stock object) so the path is not stroked.
            self.records
                .push(EmfRecord::SelectObject(STOCK_OBJECT_FLAG | NULL_PEN));
            self.cur_pen = None;
            return;
        };
        let rec = pen_record(s, scale, 0);
        // Look for an existing identical pen (ignoring the handle field).
        let handle = self
            .pens
            .iter()
            .find(|(r, _)| pen_eq_ignoring_handle(r, &rec))
            .map(|(_, h)| *h);
        let handle = match handle {
            Some(h) => h,
            None => {
                let h = self.alloc_handle();
                let rec = pen_record(s, scale, h);
                self.records.push(rec.clone());
                self.pens.push((rec, h));
                h
            }
        };
        if self.cur_pen != Some(handle) {
            self.records.push(EmfRecord::SelectObject(handle));
            self.cur_pen = Some(handle);
        }
    }

    fn select_brush(&mut self, color: Option<Rgb>) {
        let Some(c) = color else {
            self.records
                .push(EmfRecord::SelectObject(STOCK_OBJECT_FLAG | NULL_BRUSH));
            self.cur_brush = None;
            return;
        };
        let rec = EmfRecord::CreateBrushIndirect {
            ih: 0,
            style: BS_SOLID,
            color: ColorRef::new(c.r, c.g, c.b),
            hatch: 0,
        };
        let handle = self
            .brushes
            .iter()
            .find(|(r, _)| brush_eq_ignoring_handle(r, &rec))
            .map(|(_, h)| *h);
        let handle = match handle {
            Some(h) => h,
            None => {
                let h = self.alloc_handle();
                let rec = EmfRecord::CreateBrushIndirect {
                    ih: h,
                    style: BS_SOLID,
                    color: ColorRef::new(c.r, c.g, c.b),
                    hatch: 0,
                };
                self.records.push(rec.clone());
                self.brushes.push((rec, h));
                h
            }
        };
        if self.cur_brush != Some(handle) {
            self.records.push(EmfRecord::SelectObject(handle));
            self.cur_brush = Some(handle);
        }
    }

    fn emit_path(&mut self, segs: &[PathSeg], transform: &Matrix) {
        // Points are recorded in the element's local space and mapped to
        // fixed-point device coordinates by `dev`.
        let scale = self.scale;
        let dev = |p: Point| sp(transform.apply(p), scale);
        let mut cur = Point::new(0.0, 0.0);
        let mut sub_start = Point::new(0.0, 0.0);
        for seg in segs {
            match *seg {
                PathSeg::MoveTo(p) => {
                    self.records.push(EmfRecord::MoveToEx(dev(p)));
                    cur = p;
                    sub_start = p;
                }
                PathSeg::LineTo(p) => {
                    self.records.push(EmfRecord::LineTo(dev(p)));
                    cur = p;
                }
                PathSeg::CubicTo(c1, c2, p) => {
                    self.records
                        .push(EmfRecord::PolybezierTo(vec![dev(c1), dev(c2), dev(p)]));
                    cur = p;
                }
                PathSeg::Arc {
                    rx,
                    ry,
                    rot,
                    large,
                    sweep,
                    end,
                } => {
                    // Flatten in local space; bezier control points are
                    // affine-covariant so baking the transform afterwards is
                    // exact. Tolerance is expressed in device units.
                    let ms = transform.mean_scale().max(1e-9);
                    let tol = self.opts.arc_tolerance / ms;
                    let beziers = arc_to_beziers(cur, rx, ry, rot, large, sweep, end, tol);
                    for (c1, c2, p) in beziers {
                        self.records
                            .push(EmfRecord::PolybezierTo(vec![dev(c1), dev(c2), dev(p)]));
                    }
                    cur = end;
                }
                PathSeg::Close => {
                    self.records.push(EmfRecord::CloseFigure);
                    cur = sub_start;
                }
            }
        }
    }
}

/// Scale + round a device-space point to integer logical coordinates.
fn sp(p: Point, scale: f64) -> PointL {
    PointL::new((p.x * scale).round() as i32, (p.y * scale).round() as i32)
}

fn pen_record(s: &Stroke, scale: f64, handle: u32) -> EmfRecord {
    let mut style = PS_GEOMETRIC;
    style |= match s.cap {
        LineCap::Round => PS_ENDCAP_ROUND,
        LineCap::Square => PS_ENDCAP_SQUARE,
        LineCap::Flat => PS_ENDCAP_FLAT,
    };
    style |= match s.join {
        LineJoin::Round => PS_JOIN_ROUND,
        LineJoin::Bevel => PS_JOIN_BEVEL,
        LineJoin::Miter => PS_JOIN_MITER,
    };
    let width = (s.width.max(0.0) * scale).round().max(1.0) as u32;
    let (style_bits, entries) = match &s.dash {
        Some(d) if !d.is_empty() => (
            style | PS_USERSTYLE,
            d.iter()
                .map(|v| (v * scale).round().max(1.0) as u32)
                .collect(),
        ),
        _ => (style | PS_SOLID, Vec::new()),
    };
    EmfRecord::ExtCreatePen {
        ih: handle,
        style: style_bits,
        width,
        brush_style: BS_SOLID,
        color: ColorRef::new(s.color.r, s.color.g, s.color.b),
        hatch: 0,
        style_entries: entries,
    }
}

fn pen_eq_ignoring_handle(a: &EmfRecord, b: &EmfRecord) -> bool {
    if let (
        EmfRecord::ExtCreatePen {
            style: s1,
            width: w1,
            color: c1,
            style_entries: e1,
            ..
        },
        EmfRecord::ExtCreatePen {
            style: s2,
            width: w2,
            color: c2,
            style_entries: e2,
            ..
        },
    ) = (a, b)
    {
        s1 == s2 && w1 == w2 && c1 == c2 && e1 == e2
    } else {
        false
    }
}

fn brush_eq_ignoring_handle(a: &EmfRecord, b: &EmfRecord) -> bool {
    if let (
        EmfRecord::CreateBrushIndirect {
            style: s1,
            color: c1,
            hatch: h1,
            ..
        },
        EmfRecord::CreateBrushIndirect {
            style: s2,
            color: c2,
            hatch: h2,
            ..
        },
    ) = (a, b)
    {
        s1 == s2 && c1 == c2 && h1 == h2
    } else {
        false
    }
}

/// Convert an SVG-style elliptical arc into a series of cubic beziers using
/// kurbo. Returns `(ctrl1, ctrl2, end)` triples.
#[allow(clippy::too_many_arguments)]
fn arc_to_beziers(
    from: Point,
    rx: f64,
    ry: f64,
    rot_deg: f64,
    large: bool,
    sweep: bool,
    to: Point,
    tolerance: f64,
) -> Vec<(Point, Point, Point)> {
    let svg_arc = SvgArc {
        from: KPoint::new(from.x, from.y),
        to: KPoint::new(to.x, to.y),
        radii: Vec2::new(rx.abs(), ry.abs()),
        x_rotation: rot_deg * PI / 180.0,
        large_arc: large,
        sweep,
    };
    let arc = match KArc::from_svg_arc(&svg_arc) {
        Some(a) => a,
        None => {
            // Degenerate (zero radius or coincident points): straight line.
            return vec![(from, to, to)];
        }
    };
    let mut out = Vec::new();
    arc.to_cubic_beziers(tolerance.max(1e-6), |p1, p2, p3| {
        out.push((
            Point::new(p1.x, p1.y),
            Point::new(p2.x, p2.y),
            Point::new(p3.x, p3.y),
        ));
    });
    if out.is_empty() {
        out.push((from, to, to));
    }
    out
}

// Keep PathEl referenced so the import is meaningful if kurbo API shifts.
#[allow(dead_code)]
fn _assert_kurbo(_e: PathEl) {}

#[cfg(test)]
mod tests {
    use super::*;
    use vector_ir::{Element, Fill, Stroke};

    fn simple_scene() -> Scene {
        Scene {
            min_x: 0.0,
            min_y: 0.0,
            width: 100.0,
            height: 100.0,
            elements: vec![Element {
                transform: Matrix::IDENTITY,
                path: vec![
                    PathSeg::MoveTo(Point::new(0.0, 0.0)),
                    PathSeg::LineTo(Point::new(50.0, 0.0)),
                    PathSeg::LineTo(Point::new(50.0, 50.0)),
                    PathSeg::Close,
                ],
                stroke: Some(Stroke {
                    color: Rgb::new(0, 0, 0),
                    width: 2.0,
                    ..Default::default()
                }),
                fill: Some(Fill::solid(Rgb::new(255, 0, 0), FillRule::NonZero)),
            }],
            warnings: vec![],
        }
    }

    #[test]
    fn emits_parseable_emf() {
        let bytes = scene_to_emf(&simple_scene(), EmitOptions::default());
        let file = emf_core::EmfFile::parse(&bytes).expect("valid EMF");
        assert!(file
            .records
            .iter()
            .any(|r| matches!(r, EmfRecord::BeginPath)));
        assert!(file
            .records
            .iter()
            .any(|r| matches!(r, EmfRecord::StrokeAndFillPath(_))));
    }
}
