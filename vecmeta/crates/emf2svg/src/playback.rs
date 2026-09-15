//! Record playback: turn an EMF record stream into a vector [`Scene`].
//!
//! Coordinates stay in EMF *logical* units inside the IR; the page mapping
//! (map mode + window/viewport) and the world transform are folded into each
//! element's transform matrix. This keeps path coordinates exact integers so
//! the SVG -> EMF round trip can reproduce them losslessly.

use std::collections::{BTreeSet, HashMap};

use emf_core::types::*;
use emf_core::{EmfFile, EmfRecord};
use vector_ir::{
    Element, Fill, FillRule, LineCap, LineJoin, Matrix, PathSeg, Point, Rgb, Scene, Stroke,
};

use crate::dc::{xform_to_matrix, Dc, GdiObject, PenDef};

/// Which paint operations a path bracket requested.
#[derive(Clone, Copy, Default)]
struct PaintReq {
    stroke: bool,
    fill: bool,
}

struct Player {
    dc: Dc,
    dc_stack: Vec<Dc>,
    objects: HashMap<u32, GdiObject>,
    px_per_mm_x: f64,
    px_per_mm_y: f64,
    scene: Scene,

    /// Accumulated path segments (logical units) of the current figure.
    path: Vec<PathSeg>,
    /// Transform captured when the current path/figure started.
    path_ctm: Matrix,
    in_path: bool,
    started_subpath: bool,
    /// iType values of records skipped as non-vector (deduplicated).
    skipped: BTreeSet<u32>,
}

impl Player {
    fn new(file: &EmfFile) -> Self {
        Player {
            dc: Dc::default(),
            dc_stack: Vec::new(),
            objects: HashMap::new(),
            px_per_mm_x: file.header.px_per_mm_x(),
            px_per_mm_y: file.header.px_per_mm_y(),
            scene: Scene::default(),
            path: Vec::new(),
            path_ctm: Matrix::IDENTITY,
            in_path: false,
            started_subpath: false,
            skipped: BTreeSet::new(),
        }
    }

    /// Current transform: page mapping composed with the world transform.
    fn ctm(&self) -> Matrix {
        let page = self.dc.page_matrix(self.px_per_mm_x, self.px_per_mm_y);
        page.mul(&self.dc.world)
    }

    fn stroke_style(&self) -> Option<Stroke> {
        self.dc.pen.as_ref().map(|p| Stroke {
            color: p.color,
            width: if p.cosmetic { 0.0 } else { p.width },
            dash: p.dash.clone(),
            cap: p.cap,
            join: p.join,
            miter_limit: self.dc.miter_limit,
            alpha: 255,
        })
    }

    fn fill_style(&self) -> Option<Fill> {
        self.dc
            .brush
            .map(|color| Fill::solid(color, self.dc.fill_rule))
    }

    /// Emit a standalone (non-path) shape with the current pen and brush.
    fn emit_shape(&mut self, segs: Vec<PathSeg>, stroke: bool, fill: bool) {
        if segs.is_empty() {
            return;
        }
        let element = Element {
            transform: self.ctm(),
            path: segs,
            stroke: if stroke { self.stroke_style() } else { None },
            fill: if fill { self.fill_style() } else { None },
        };
        if element.stroke.is_some() || element.fill.is_some() {
            self.scene.elements.push(element);
        }
    }

    fn lp(p: PointL) -> Point {
        Point::new(p.x as f64, p.y as f64)
    }

    /// Append a poly-based figure to a segment list, closing when `close`.
    fn poly_to_segs(pts: &[PointL], close: bool, cont: bool, from: Point) -> Vec<PathSeg> {
        let mut segs = Vec::new();
        if pts.is_empty() {
            return segs;
        }
        if cont {
            segs.push(PathSeg::MoveTo(from));
            for p in pts {
                segs.push(PathSeg::LineTo(Self::lp(*p)));
            }
        } else {
            segs.push(PathSeg::MoveTo(Self::lp(pts[0])));
            for p in &pts[1..] {
                segs.push(PathSeg::LineTo(Self::lp(*p)));
            }
        }
        if close {
            segs.push(PathSeg::Close);
        }
        segs
    }

    fn bezier_to_segs(pts: &[PointL], cont: bool, from: Point) -> Vec<PathSeg> {
        let mut segs = Vec::new();
        let mut idx = 0;
        let mut start = from;
        if !cont {
            if pts.is_empty() {
                return segs;
            }
            start = Self::lp(pts[0]);
            idx = 1;
        }
        segs.push(PathSeg::MoveTo(start));
        while idx + 2 < pts.len() + 1 && idx + 2 <= pts.len() {
            if idx + 2 >= pts.len() {
                break;
            }
            let c1 = Self::lp(pts[idx]);
            let c2 = Self::lp(pts[idx + 1]);
            let end = Self::lp(pts[idx + 2]);
            segs.push(PathSeg::CubicTo(c1, c2, end));
            idx += 3;
        }
        segs
    }

    fn last_point(segs: &[PathSeg]) -> Option<Point> {
        segs.iter().rev().find_map(|s| s.end_point())
    }

    /// Push segments into the current path, coalescing subpaths.
    fn extend_path(&mut self, mut segs: Vec<PathSeg>) {
        if segs.is_empty() {
            return;
        }
        if self.started_subpath {
            // Drop a leading MoveTo that merely restates the current point.
            if let Some(PathSeg::MoveTo(_)) = segs.first() {
                if !self.path.is_empty() {
                    segs.remove(0);
                }
            }
        }
        self.path.append(&mut segs);
        self.started_subpath = true;
    }
}

/// Convert a parsed EMF file into a vector [`Scene`].
pub fn play(file: &EmfFile) -> Scene {
    let mut pl = Player::new(file);

    // Canvas size derived from the header bounds (device units).
    let b = file.header.bounds;
    pl.scene.min_x = b.left.min(b.right) as f64;
    pl.scene.min_y = b.top.min(b.bottom) as f64;
    pl.scene.width = (b.width().abs().max(1)) as f64;
    pl.scene.height = (b.height().abs().max(1)) as f64;
    pl.dc.viewport_ext = SizeL::new(1, 1);
    pl.dc.window_ext = SizeL::new(1, 1);

    for rec in &file.records {
        step(&mut pl, rec);
    }

    // Recover advanced paint (alpha/gradients) from an embedded EMF+ block.
    apply_emfplus(&mut pl.scene, file);

    if !pl.skipped.is_empty() {
        let list: Vec<String> = pl.skipped.iter().map(|t| t.to_string()).collect();
        pl.scene.warnings.push(format!(
            "skipped {} non-vector record type(s) (iType: {})",
            pl.skipped.len(),
            list.join(", ")
        ));
    }

    pl.scene
}

/// Scan comment records for the EMF+ paint table and apply alpha/gradients to
/// the reconstructed elements (keyed by emission index).
fn apply_emfplus(scene: &mut Scene, file: &EmfFile) {
    use emf_core::emfplus::{decode, PlusPaint};
    use vector_ir::{GradientStop, LinearGradient};

    for rec in &file.records {
        let EmfRecord::Comment(data) = rec else {
            continue;
        };
        let Some(entries) = decode(data) else {
            continue;
        };
        for e in entries {
            let Some(el) = scene.elements.get_mut(e.index as usize) else {
                continue;
            };
            match e.paint {
                PlusPaint::Solid { r, g, b, a } => {
                    if let Some(fill) = el.fill.as_mut() {
                        fill.color = Rgb::new(r, g, b);
                        fill.alpha = a;
                    }
                }
                PlusPaint::StrokeSolid { r, g, b, a } => {
                    if let Some(stroke) = el.stroke.as_mut() {
                        stroke.color = Rgb::new(r, g, b);
                        stroke.alpha = a;
                    }
                }
                PlusPaint::Linear {
                    x1,
                    y1,
                    x2,
                    y2,
                    stops,
                } => {
                    if let Some(inv) = el.transform.invert() {
                        if let Some(fill) = el.fill.as_mut() {
                            // Entries store device-space endpoints; convert
                            // into this element's local space so the SVG
                            // userSpaceOnUse gradient lands correctly.
                            let p1 = inv.apply(vector_ir::Point::new(x1 as f64, y1 as f64));
                            let p2 = inv.apply(vector_ir::Point::new(x2 as f64, y2 as f64));
                            fill.gradient = Some(LinearGradient {
                                x1: p1.x,
                                y1: p1.y,
                                x2: p2.x,
                                y2: p2.y,
                                stops: stops
                                    .iter()
                                    .map(|s| GradientStop {
                                        offset: s.offset as f64,
                                        color: Rgb::new(s.r, s.g, s.b),
                                        alpha: s.a,
                                    })
                                    .collect(),
                            });
                        }
                    }
                }
            }
        }
    }
}

fn step(pl: &mut Player, rec: &EmfRecord) {
    match rec {
        EmfRecord::Header(_) | EmfRecord::Eof => {}

        // --- State ---
        EmfRecord::SetMapMode(m) => pl.dc.map_mode = *m,
        EmfRecord::SetWindowExtEx(s) => pl.dc.window_ext = *s,
        EmfRecord::SetWindowOrgEx(p) => pl.dc.window_org = *p,
        EmfRecord::SetViewportExtEx(s) => pl.dc.viewport_ext = *s,
        EmfRecord::SetViewportOrgEx(p) => pl.dc.viewport_org = *p,
        EmfRecord::SetPolyfillMode(m) => {
            pl.dc.fill_rule = if *m == WINDING {
                FillRule::NonZero
            } else {
                FillRule::EvenOdd
            };
        }
        EmfRecord::SetMiterLimit(v) => pl.dc.miter_limit = *v as f64,
        EmfRecord::SetArcDirection(d) => pl.dc.clockwise = *d == AD_CLOCKWISE,
        EmfRecord::MoveToEx(p) => {
            pl.dc.cur_pos = Player::lp(*p);
            if pl.in_path {
                pl.path.push(PathSeg::MoveTo(pl.dc.cur_pos));
                pl.started_subpath = true;
            }
        }
        EmfRecord::SaveDc => pl.dc_stack.push(pl.dc.clone()),
        EmfRecord::RestoreDc(n) => {
            // n is negative: -1 = most recently saved.
            let count = (-*n).max(0) as usize;
            for _ in 0..count {
                if let Some(dc) = pl.dc_stack.pop() {
                    pl.dc = dc;
                }
            }
        }

        // --- Transform ---
        EmfRecord::SetWorldTransform(x) => pl.dc.world = xform_to_matrix(x),
        EmfRecord::ModifyWorldTransform { xform, mode } => {
            let m = xform_to_matrix(xform);
            pl.dc.world = match *mode {
                MWT_IDENTITY => Matrix::IDENTITY,
                MWT_LEFTMULTIPLY => m.mul(&pl.dc.world),
                MWT_RIGHTMULTIPLY => pl.dc.world.mul(&m),
                MWT_SET => m,
                _ => pl.dc.world,
            };
        }

        // --- Objects ---
        EmfRecord::CreatePen {
            ih,
            style,
            width,
            color,
        } => {
            let pen = build_pen(*style, *width as u32, *color, false, &[]);
            pl.objects.insert(*ih, GdiObject::Pen(pen));
        }
        EmfRecord::ExtCreatePen {
            ih,
            style,
            width,
            color,
            style_entries,
            ..
        } => {
            let pen = build_pen(*style, *width, *color, true, style_entries);
            pl.objects.insert(*ih, GdiObject::Pen(pen));
        }
        EmfRecord::CreateBrushIndirect {
            ih, style, color, ..
        } => {
            let brush = if *style == BS_NULL {
                None
            } else {
                Some(Rgb::new(color.r, color.g, color.b))
            };
            pl.objects.insert(*ih, GdiObject::Brush(brush));
        }
        EmfRecord::SelectObject(ih) => select_object(pl, *ih),
        EmfRecord::DeleteObject(ih) => {
            pl.objects.remove(ih);
        }

        // --- Path bracket ---
        EmfRecord::BeginPath => {
            pl.in_path = true;
            pl.path.clear();
            pl.started_subpath = false;
            pl.path_ctm = pl.ctm();
        }
        EmfRecord::CloseFigure => {
            if pl.in_path {
                pl.path.push(PathSeg::Close);
            }
        }
        EmfRecord::EndPath => pl.in_path = false,
        EmfRecord::AbortPath => {
            pl.in_path = false;
            pl.path.clear();
            pl.started_subpath = false;
        }
        EmfRecord::FillPath(_) => flush_path(
            pl,
            PaintReq {
                stroke: false,
                fill: true,
            },
        ),
        EmfRecord::StrokePath(_) => flush_path(
            pl,
            PaintReq {
                stroke: true,
                fill: false,
            },
        ),
        EmfRecord::StrokeAndFillPath(_) => flush_path(
            pl,
            PaintReq {
                stroke: true,
                fill: true,
            },
        ),

        // --- Drawing (immediate mode or path accumulation) ---
        _ => draw_record(pl, rec),
    }
}

fn draw_record(pl: &mut Player, rec: &EmfRecord) {
    let cur = pl.dc.cur_pos;
    match rec {
        EmfRecord::Polyline(pts) => {
            shape_or_path(pl, Player::poly_to_segs(pts, false, false, cur), false)
        }
        EmfRecord::Polygon(pts) => {
            shape_or_path(pl, Player::poly_to_segs(pts, true, false, cur), true)
        }
        EmfRecord::PolylineTo(pts) => {
            let segs = Player::poly_to_segs(pts, false, true, cur);
            update_cur(pl, &segs);
            shape_or_path(pl, segs, false);
        }
        EmfRecord::Polybezier(pts) => {
            shape_or_path(pl, Player::bezier_to_segs(pts, false, cur), false)
        }
        EmfRecord::PolybezierTo(pts) => {
            let segs = Player::bezier_to_segs(pts, true, cur);
            update_cur(pl, &segs);
            shape_or_path(pl, segs, false);
        }
        EmfRecord::Polypolyline { counts, pts } => {
            let segs = polypoly_segs(counts, pts, false);
            shape_or_path(pl, segs, false);
        }
        EmfRecord::Polypolygon { counts, pts } => {
            let segs = polypoly_segs(counts, pts, true);
            shape_or_path(pl, segs, true);
        }
        EmfRecord::PolyDraw { pts, types } => {
            let segs = polydraw_segs(pts, types, cur);
            update_cur(pl, &segs);
            shape_or_path(pl, segs, false);
        }
        EmfRecord::LineTo(p) => {
            let end = Player::lp(*p);
            let segs = vec![PathSeg::MoveTo(cur), PathSeg::LineTo(end)];
            pl.dc.cur_pos = end;
            shape_or_path(pl, segs, false);
        }
        EmfRecord::Rectangle(r) => shape_or_path(pl, rect_segs(r), true),
        EmfRecord::Ellipse(r) => shape_or_path(pl, ellipse_segs(r), true),
        EmfRecord::RoundRect { rect, corner } => {
            shape_or_path(pl, roundrect_segs(rect, corner), true)
        }
        EmfRecord::Arc { rect, start, end } => {
            let segs = arc_segs(rect, start, end, pl.dc.clockwise, false, false);
            update_cur(pl, &segs);
            shape_or_path(pl, segs, false);
        }
        EmfRecord::ArcTo { rect, start, end } => {
            let segs = arc_segs(rect, start, end, pl.dc.clockwise, true, false);
            let mut segs2 = segs;
            if !pl.in_path {
                // ArcTo also draws a line from the current point to the arc start.
                if let Some(PathSeg::MoveTo(m)) = segs2.first().copied() {
                    segs2[0] = PathSeg::MoveTo(cur);
                    segs2.insert(1, PathSeg::LineTo(m));
                }
            }
            update_cur(pl, &segs2);
            shape_or_path(pl, segs2, false);
        }
        EmfRecord::Chord { rect, start, end } => {
            let mut segs = arc_segs(rect, start, end, pl.dc.clockwise, false, false);
            segs.push(PathSeg::Close);
            shape_or_path(pl, segs, true);
        }
        EmfRecord::Pie { rect, start, end } => {
            let center = Point::new(
                (rect.left + rect.right) as f64 / 2.0,
                (rect.top + rect.bottom) as f64 / 2.0,
            );
            let mut segs = arc_segs(rect, start, end, pl.dc.clockwise, false, false);
            if let Some(first) = segs.first().and_then(|s| s.end_point()) {
                segs.insert(0, PathSeg::MoveTo(center));
                segs.insert(1, PathSeg::LineTo(first));
                // Remove the now-duplicate MoveTo that followed.
                if segs.len() > 2 {
                    if let PathSeg::MoveTo(_) = segs[2] {
                        segs.remove(2);
                    }
                }
                segs.push(PathSeg::Close);
            }
            shape_or_path(pl, segs, true);
        }
        EmfRecord::AngleArc {
            center,
            radius,
            start_angle,
            sweep_angle,
        } => {
            let segs = anglearc_segs(
                *center,
                *radius,
                *start_angle,
                *sweep_angle,
                cur,
                pl.in_path,
            );
            update_cur(pl, &segs);
            shape_or_path(pl, segs, false);
        }
        EmfRecord::Unknown { itype, data } => {
            handle_unknown(pl, *itype, data);
        }
        _ => {}
    }
}

/// Handle records `emf-core` keeps as raw `Unknown`: font creation, text out
/// and text color, which we decode here to vectorize text.
fn handle_unknown(pl: &mut Player, itype: u32, data: &[u8]) {
    use crate::text;
    match itype {
        // SETTEXTCOLOR: COLORREF at payload start.
        24 => {
            if data.len() >= 4 {
                pl.dc.text_color = Rgb::new(data[0], data[1], data[2]);
            }
        }
        text::EMR_EXTCREATEFONTINDIRECTW => {
            if let Some((ih, spec)) = text::parse_font(data) {
                pl.objects.insert(ih, GdiObject::Font(spec));
            }
        }
        text::EMR_EXTTEXTOUTW | text::EMR_EXTTEXTOUTA => {
            let wide = itype == text::EMR_EXTTEXTOUTW;
            if let Some(t) = text::parse_text_out(data, wide) {
                let segs = text::vectorize(&t, &pl.dc.font);
                if segs.is_empty() {
                    pl.scene
                        .warnings
                        .push(format!("no font outline for text \"{}\"", t.text));
                } else {
                    let element = Element {
                        transform: pl.ctm(),
                        path: segs,
                        stroke: None,
                        fill: Some(Fill::solid(pl.dc.text_color, FillRule::NonZero)),
                    };
                    pl.scene.elements.push(element);
                }
            }
        }
        _ => {
            pl.skipped.insert(itype);
        }
    }
}

/// Route a shape either into the open path or emit it immediately.
fn shape_or_path(pl: &mut Player, segs: Vec<PathSeg>, closed_fill: bool) {
    if pl.in_path {
        pl.extend_path(segs);
    } else {
        // Immediate-mode primitive: stroke always, fill only closed shapes.
        pl.emit_shape(segs, true, closed_fill);
    }
}

fn update_cur(pl: &mut Player, segs: &[PathSeg]) {
    if let Some(p) = Player::last_point(segs) {
        pl.dc.cur_pos = p;
    }
}

fn flush_path(pl: &mut Player, req: PaintReq) {
    if pl.path.is_empty() {
        return;
    }
    let element = Element {
        transform: pl.path_ctm,
        path: std::mem::take(&mut pl.path),
        stroke: if req.stroke { pl.stroke_style() } else { None },
        fill: if req.fill { pl.fill_style() } else { None },
    };
    if element.stroke.is_some() || element.fill.is_some() {
        pl.scene.elements.push(element);
    }
    pl.started_subpath = false;
}

fn select_object(pl: &mut Player, ih: u32) {
    if ih & STOCK_OBJECT_FLAG != 0 {
        match ih & !STOCK_OBJECT_FLAG {
            WHITE_BRUSH => pl.dc.brush = Some(Rgb::new(255, 255, 255)),
            LTGRAY_BRUSH => pl.dc.brush = Some(Rgb::new(192, 192, 192)),
            GRAY_BRUSH => pl.dc.brush = Some(Rgb::new(128, 128, 128)),
            DKGRAY_BRUSH => pl.dc.brush = Some(Rgb::new(64, 64, 64)),
            BLACK_BRUSH => pl.dc.brush = Some(Rgb::new(0, 0, 0)),
            NULL_BRUSH => pl.dc.brush = None,
            WHITE_PEN => {
                pl.dc.pen = Some(cosmetic_pen(Rgb::new(255, 255, 255)));
            }
            BLACK_PEN => {
                pl.dc.pen = Some(cosmetic_pen(Rgb::new(0, 0, 0)));
            }
            NULL_PEN => pl.dc.pen = None,
            _ => {}
        }
        return;
    }
    match pl.objects.get(&ih) {
        Some(GdiObject::Pen(p)) => pl.dc.pen = p.clone(),
        Some(GdiObject::Brush(b)) => pl.dc.brush = *b,
        Some(GdiObject::Font(f)) => pl.dc.font = f.clone(),
        _ => {}
    }
}

fn cosmetic_pen(color: Rgb) -> PenDef {
    PenDef {
        color,
        width: 1.0,
        cosmetic: true,
        dash: None,
        cap: LineCap::Round,
        join: LineJoin::Round,
    }
}

fn build_pen(
    style: u32,
    width: u32,
    color: ColorRef,
    ext: bool,
    entries: &[u32],
) -> Option<PenDef> {
    let base = style & PS_STYLE_MASK;
    if base == PS_NULL {
        return None;
    }
    let cosmetic = if ext {
        (style & PS_TYPE_MASK) == PS_COSMETIC
    } else {
        width == 0
    };
    let cap = match style & PS_ENDCAP_MASK {
        PS_ENDCAP_SQUARE => LineCap::Square,
        PS_ENDCAP_FLAT => LineCap::Flat,
        _ => LineCap::Round,
    };
    let join = match style & PS_JOIN_MASK {
        PS_JOIN_BEVEL => LineJoin::Bevel,
        PS_JOIN_MITER => LineJoin::Miter,
        _ => LineJoin::Round,
    };
    let w = width.max(1) as f64;
    let dash = dash_pattern(base, w, entries);
    Some(PenDef {
        color: Rgb::new(color.r, color.g, color.b),
        width: w,
        cosmetic,
        dash,
        cap,
        join,
    })
}

fn dash_pattern(base: u32, w: f64, entries: &[u32]) -> Option<Vec<f64>> {
    match base {
        PS_DASH => Some(vec![w * 3.0, w * 3.0]),
        PS_DOT => Some(vec![w, w]),
        PS_DASHDOT => Some(vec![w * 3.0, w, w, w]),
        PS_DASHDOTDOT => Some(vec![w * 3.0, w, w, w, w, w]),
        PS_USERSTYLE if !entries.is_empty() => Some(entries.iter().map(|&e| e as f64).collect()),
        _ => None,
    }
}

fn rect_segs(r: &RectL) -> Vec<PathSeg> {
    vec![
        PathSeg::MoveTo(Point::new(r.left as f64, r.top as f64)),
        PathSeg::LineTo(Point::new(r.right as f64, r.top as f64)),
        PathSeg::LineTo(Point::new(r.right as f64, r.bottom as f64)),
        PathSeg::LineTo(Point::new(r.left as f64, r.bottom as f64)),
        PathSeg::Close,
    ]
}

fn ellipse_segs(r: &RectL) -> Vec<PathSeg> {
    let cx = (r.left + r.right) as f64 / 2.0;
    let cy = (r.top + r.bottom) as f64 / 2.0;
    let rx = (r.right - r.left).abs() as f64 / 2.0;
    let ry = (r.bottom - r.top).abs() as f64 / 2.0;
    vec![
        PathSeg::MoveTo(Point::new(cx + rx, cy)),
        PathSeg::Arc {
            rx,
            ry,
            rot: 0.0,
            large: false,
            sweep: true,
            end: Point::new(cx - rx, cy),
        },
        PathSeg::Arc {
            rx,
            ry,
            rot: 0.0,
            large: false,
            sweep: true,
            end: Point::new(cx + rx, cy),
        },
        PathSeg::Close,
    ]
}

fn roundrect_segs(r: &RectL, corner: &SizeL) -> Vec<PathSeg> {
    let rx = (corner.cx as f64 / 2.0).min((r.right - r.left).abs() as f64 / 2.0);
    let ry = (corner.cy as f64 / 2.0).min((r.bottom - r.top).abs() as f64 / 2.0);
    let (l, t, ri, b) = (r.left as f64, r.top as f64, r.right as f64, r.bottom as f64);
    if rx <= 0.0 || ry <= 0.0 {
        return rect_segs(r);
    }
    vec![
        PathSeg::MoveTo(Point::new(l + rx, t)),
        PathSeg::LineTo(Point::new(ri - rx, t)),
        PathSeg::Arc {
            rx,
            ry,
            rot: 0.0,
            large: false,
            sweep: true,
            end: Point::new(ri, t + ry),
        },
        PathSeg::LineTo(Point::new(ri, b - ry)),
        PathSeg::Arc {
            rx,
            ry,
            rot: 0.0,
            large: false,
            sweep: true,
            end: Point::new(ri - rx, b),
        },
        PathSeg::LineTo(Point::new(l + rx, b)),
        PathSeg::Arc {
            rx,
            ry,
            rot: 0.0,
            large: false,
            sweep: true,
            end: Point::new(l, b - ry),
        },
        PathSeg::LineTo(Point::new(l, t + ry)),
        PathSeg::Arc {
            rx,
            ry,
            rot: 0.0,
            large: false,
            sweep: true,
            end: Point::new(l + rx, t),
        },
        PathSeg::Close,
    ]
}

fn polypoly_segs(counts: &[u32], pts: &[PointL], close: bool) -> Vec<PathSeg> {
    let mut segs = Vec::new();
    let mut idx = 0usize;
    for &c in counts {
        let n = c as usize;
        if idx + n > pts.len() {
            break;
        }
        let sub = &pts[idx..idx + n];
        idx += n;
        if sub.is_empty() {
            continue;
        }
        segs.push(PathSeg::MoveTo(Player::lp(sub[0])));
        for p in &sub[1..] {
            segs.push(PathSeg::LineTo(Player::lp(*p)));
        }
        if close {
            segs.push(PathSeg::Close);
        }
    }
    segs
}

fn polydraw_segs(pts: &[PointL], types: &[u8], cur: Point) -> Vec<PathSeg> {
    let mut segs = vec![PathSeg::MoveTo(cur)];
    let mut i = 0usize;
    while i < pts.len() && i < types.len() {
        let t = types[i] & !PT_CLOSEFIGURE;
        match t {
            PT_MOVETO => {
                segs.push(PathSeg::MoveTo(Player::lp(pts[i])));
                i += 1;
            }
            PT_LINETO => {
                segs.push(PathSeg::LineTo(Player::lp(pts[i])));
                if types[i] & PT_CLOSEFIGURE != 0 {
                    segs.push(PathSeg::Close);
                }
                i += 1;
            }
            PT_BEZIERTO => {
                if i + 2 < pts.len() {
                    segs.push(PathSeg::CubicTo(
                        Player::lp(pts[i]),
                        Player::lp(pts[i + 1]),
                        Player::lp(pts[i + 2]),
                    ));
                    if types[i + 2] & PT_CLOSEFIGURE != 0 {
                        segs.push(PathSeg::Close);
                    }
                }
                i += 3;
            }
            _ => {
                i += 1;
            }
        }
    }
    segs
}

/// Build arc segments from an EMF bounding-box arc definition.
/// `radial` points define the start/end angles; `arc_to` means the arc
/// starts from the current point via a line (handled by the caller).
fn arc_segs(
    rect: &RectL,
    start: &PointL,
    end: &PointL,
    clockwise: bool,
    _arc_to: bool,
    _pie: bool,
) -> Vec<PathSeg> {
    let cx = (rect.left + rect.right) as f64 / 2.0;
    let cy = (rect.top + rect.bottom) as f64 / 2.0;
    let rx = (rect.right - rect.left).abs() as f64 / 2.0;
    let ry = (rect.bottom - rect.top).abs() as f64 / 2.0;
    if rx <= 0.0 || ry <= 0.0 {
        return Vec::new();
    }
    // Angles of the radial vectors (map ellipse to circle first).
    let a0 = ((start.y as f64 - cy) / ry).atan2((start.x as f64 - cx) / rx);
    let a1 = ((end.y as f64 - cy) / ry).atan2((end.x as f64 - cx) / rx);
    let p0 = Point::new(cx + rx * a0.cos(), cy + ry * a0.sin());
    let p1 = Point::new(cx + rx * a1.cos(), cy + ry * a1.sin());

    // EMF default arc direction is counterclockwise (in EMF's own Y-down
    // logical space that is a mathematically increasing angle).
    let mut sweep = a1 - a0;
    if clockwise {
        if sweep > 0.0 {
            sweep -= 2.0 * std::f64::consts::PI;
        }
    } else if sweep < 0.0 {
        sweep += 2.0 * std::f64::consts::PI;
    }
    let large = sweep.abs() > std::f64::consts::PI;
    // SVG sweep flag: 1 for positive-angle (counterclockwise here matches
    // EMF's convention because both share Y-down logical coordinates).
    let sweep_flag = sweep > 0.0;
    vec![
        PathSeg::MoveTo(p0),
        PathSeg::Arc {
            rx,
            ry,
            rot: 0.0,
            large,
            sweep: sweep_flag,
            end: p1,
        },
    ]
}

fn anglearc_segs(
    center: PointL,
    radius: u32,
    start_angle: f32,
    sweep_angle: f32,
    cur: Point,
    in_path: bool,
) -> Vec<PathSeg> {
    let r = radius as f64;
    let cx = center.x as f64;
    let cy = center.y as f64;
    let a0 = (start_angle as f64).to_radians();
    let a1 = (start_angle as f64 + sweep_angle as f64).to_radians();
    let p0 = Point::new(cx + r * a0.cos(), cy + r * a0.sin());
    let p1 = Point::new(cx + r * a1.cos(), cy + r * a1.sin());
    let large = sweep_angle.abs() as f64 > 180.0;
    let sweep_flag = sweep_angle > 0.0;
    let mut segs = Vec::new();
    // AngleArc always draws a line from the current point to the arc start.
    segs.push(PathSeg::MoveTo(cur));
    if in_path || (cur.x - p0.x).abs() > 1e-9 || (cur.y - p0.y).abs() > 1e-9 {
        segs.push(PathSeg::LineTo(p0));
    }
    segs.push(PathSeg::Arc {
        rx: r,
        ry: r,
        rot: 0.0,
        large,
        sweep: sweep_flag,
        end: p1,
    });
    segs
}
