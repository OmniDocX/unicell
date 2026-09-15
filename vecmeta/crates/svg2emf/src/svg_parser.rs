//! Parse a general vector subset of SVG into the shared [`Scene`] IR.
//!
//! Supported elements: `path`, `line`, `polyline`, `polygon`, `rect`
//! (incl. rounded), `circle`, `ellipse`, and `g` (transform + inherited
//! paint). Gradient/pattern paints are approximated by a representative solid
//! color; text, images and filters are skipped with a warning.

use std::collections::HashMap;
use std::str::FromStr;

use roxmltree::{Document, Node};
use svgtypes::{
    Color, LengthListParser, PathParser, PathSegment, TransformListParser, TransformListToken,
};
use vector_ir::{
    Element, Fill, FillRule, GradientStop, LineCap, LineJoin, LinearGradient, Matrix, PathSeg,
    Point, Rgb, Scene, Stroke,
};

/// Error type for SVG parsing.
#[derive(Debug, thiserror::Error)]
pub enum SvgError {
    #[error("invalid XML: {0}")]
    Xml(#[from] roxmltree::Error),
    #[error("missing root <svg> element")]
    NoRoot,
}

/// Inherited presentation state during traversal.
#[derive(Clone)]
struct Ctx {
    transform: Matrix,
    stroke: Option<Rgb>,
    stroke_width: f64,
    dash: Option<Vec<f64>>,
    cap: LineCap,
    join: LineJoin,
    miter: f64,
    fill: Option<Rgb>,
    fill_rule: FillRule,
    fill_opacity: f64,
    stroke_opacity: f64,
    group_opacity: f64,
    /// Id of a gradient paint server referenced by `fill`, if any.
    fill_gradient_id: Option<String>,
    font_family: String,
    font_size: f64,
    font_weight: u16,
    font_italic: bool,
    text_anchor: glyph2path::Anchor,
}

impl Default for Ctx {
    fn default() -> Self {
        // SVG initial values: fill black, stroke none, width 1.
        Self {
            transform: Matrix::IDENTITY,
            stroke: None,
            stroke_width: 1.0,
            dash: None,
            cap: LineCap::Flat,
            join: LineJoin::Miter,
            miter: 4.0,
            fill: Some(Rgb::new(0, 0, 0)),
            fill_rule: FillRule::NonZero,
            fill_opacity: 1.0,
            stroke_opacity: 1.0,
            group_opacity: 1.0,
            fill_gradient_id: None,
            font_family: "sans-serif".to_string(),
            font_size: 16.0,
            font_weight: 400,
            font_italic: false,
            text_anchor: glyph2path::Anchor::Start,
        }
    }
}

/// Parse SVG text into a [`Scene`].
pub fn svg_to_scene(svg: &str) -> Result<Scene, SvgError> {
    let doc = Document::parse(svg)?;
    let root = doc.root_element();
    if !root.has_tag_name("svg") {
        return Err(SvgError::NoRoot);
    }

    let mut scene = Scene::default();
    let (min_x, min_y, w, h) = svg_size(&root);
    scene.min_x = min_x;
    scene.min_y = min_y;
    scene.width = w;
    scene.height = h;

    // Resolve gradient/pattern paint servers to a representative solid color
    // up front so `url(#id)` references render instead of vanishing.
    let paints = collect_paint_servers(&root);

    // Parse any <style> sheets for the CSS cascade. `sheet` borrows `css_text`,
    // both live for the duration of the walk.
    let css_text = crate::css::collect_style_text(&root);
    let sheet = simplecss::StyleSheet::parse(&css_text);

    let ctx = Ctx::default();
    walk(root, &ctx, &sheet, &paints, &mut scene, 0);
    Ok(scene)
}

/// A parsed gradient paint server, with its stops and coordinate system.
struct GradientDef {
    /// True when coordinates are in objectBoundingBox units (0..1), the SVG
    /// default; false for userSpaceOnUse.
    object_bbox: bool,
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
    stops: Vec<GradientStop>,
    /// Average color, used as the classic-EMF solid fallback.
    average: Rgb,
}

/// Parse every `linearGradient` into a [`GradientDef`]. `radialGradient` is
/// approximated as a linear gradient across its stops.
fn collect_paint_servers(root: &Node) -> HashMap<String, GradientDef> {
    let mut map = HashMap::new();
    for node in root.descendants() {
        let tag = node.tag_name().name();
        if tag != "linearGradient" && tag != "radialGradient" {
            continue;
        }
        let Some(id) = node.attribute("id") else {
            continue;
        };
        let mut stops = Vec::new();
        let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
        for stop in node
            .children()
            .filter(|c| c.is_element() && c.tag_name().name() == "stop")
        {
            let style = stop.attribute("style").unwrap_or("");
            let sc = style_lookup(style, "stop-color")
                .or_else(|| stop.attribute("stop-color").map(|s| s.to_string()));
            let offset = style_lookup(style, "offset")
                .or_else(|| stop.attribute("offset").map(|s| s.to_string()))
                .map(|s| parse_offset(&s))
                .unwrap_or(0.0);
            let so = style_lookup(style, "stop-opacity")
                .or_else(|| stop.attribute("stop-opacity").map(|s| s.to_string()))
                .and_then(|s| s.trim().parse::<f64>().ok())
                .unwrap_or(1.0);
            if let Some(c) = sc.and_then(|v| Color::from_str(v.trim()).ok()) {
                r += c.red as u32;
                g += c.green as u32;
                b += c.blue as u32;
                n += 1;
                stops.push(GradientStop {
                    offset,
                    color: Rgb::new(c.red, c.green, c.blue),
                    alpha: (so.clamp(0.0, 1.0) * 255.0).round() as u8,
                });
            }
        }
        if stops.is_empty() {
            continue;
        }
        let average = match (r.checked_div(n), g.checked_div(n), b.checked_div(n)) {
            (Some(r), Some(g), Some(b)) => Rgb::new(r as u8, g as u8, b as u8),
            _ => stops[0].color,
        };
        let units = node
            .attribute("gradientUnits")
            .unwrap_or("objectBoundingBox");
        let object_bbox = units != "userSpaceOnUse";
        let coord = |name: &str, default: f64| -> f64 {
            node.attribute(name).map(parse_offset).unwrap_or(default)
        };
        map.insert(
            id.to_string(),
            GradientDef {
                object_bbox,
                x1: coord("x1", 0.0),
                y1: coord("y1", 0.0),
                x2: coord("x2", 1.0),
                y2: coord("y2", 0.0),
                stops,
                average,
            },
        );
    }
    map
}

/// Parse an offset/coordinate that may be a percentage ("100%") or a number.
fn parse_offset(s: &str) -> f64 {
    let s = s.trim();
    if let Some(p) = s.strip_suffix('%') {
        p.trim().parse::<f64>().unwrap_or(0.0) / 100.0
    } else {
        s.parse::<f64>().unwrap_or(0.0)
    }
}

fn svg_size(root: &Node) -> (f64, f64, f64, f64) {
    // viewBox gives both the origin and the extents when present.
    let mut view = None;
    if let Some(vb) = root.attribute("viewBox") {
        let nums: Vec<f64> = vb
            .split([' ', ','])
            .filter(|s| !s.is_empty())
            .filter_map(|s| f64::from_str(s).ok())
            .collect();
        if nums.len() == 4 {
            view = Some((nums[0], nums[1], nums[2], nums[3]));
        }
    }
    // Prefer explicit width/height for the size, keeping the viewBox origin.
    let w = root.attribute("width").and_then(parse_len);
    let h = root.attribute("height").and_then(parse_len);
    match (view, w, h) {
        (Some((vx, vy, vw, vh)), w, h) => (vx, vy, w.unwrap_or(vw), h.unwrap_or(vh)),
        (None, w, h) => (0.0, 0.0, w.unwrap_or(0.0), h.unwrap_or(0.0)),
    }
}

fn parse_len(s: &str) -> Option<f64> {
    let mut p = LengthListParser::from(s);
    p.next().and_then(|r| r.ok()).map(|l| l.number)
}

fn walk(
    node: Node,
    parent: &Ctx,
    sheet: &simplecss::StyleSheet,
    paints: &HashMap<String, GradientDef>,
    scene: &mut Scene,
    depth: u32,
) {
    for child in node.children() {
        if !child.is_element() {
            continue;
        }
        let mut ctx = parent.clone();
        apply_common(&child, sheet, paints, &mut ctx);
        render_node(child, &ctx, sheet, paints, scene, depth);
    }
}

/// Render a single element node with an already-resolved context.
fn render_node(
    node: Node,
    ctx: &Ctx,
    sheet: &simplecss::StyleSheet,
    paints: &HashMap<String, GradientDef>,
    scene: &mut Scene,
    depth: u32,
) {
    let tag = node.tag_name().name();
    match tag {
        "g" | "svg" | "a" => walk(node, ctx, sheet, paints, scene, depth),
        "defs" | "symbol" | "clipPath" | "mask" | "pattern" | "marker" | "style"
        | "linearGradient" | "radialGradient" => {
            // Definitions are not rendered directly.
        }
        "use" => {
            if depth > 48 {
                return; // guard against cyclic <use> references
            }
            let x = attr_f64(&node, "x");
            let y = attr_f64(&node, "y");
            let mut uctx = ctx.clone();
            uctx.transform = uctx.transform.mul(&Matrix::translate(x, y));
            if let Some(target) = resolve_href(&node) {
                let mut tctx = uctx.clone();
                apply_common(&target, sheet, paints, &mut tctx);
                render_node(target, &tctx, sheet, paints, scene, depth + 1);
            }
        }
        "text" => {
            let run = collect_text(&node);
            if !run.is_empty() {
                let x = attr_f64(&node, "x");
                let y = attr_f64(&node, "y");
                let style = glyph2path::TextStyle {
                    family: ctx.font_family.clone(),
                    size: ctx.font_size,
                    weight: ctx.font_weight,
                    italic: ctx.font_italic,
                };
                let segs = glyph2path::text_to_paths(&style, &run, x, y, ctx.text_anchor);
                if segs.is_empty() {
                    scene
                        .warnings
                        .push(format!("no font outline for text \"{run}\""));
                } else {
                    // Text is filled with the current fill (default black).
                    let mut tctx = ctx.clone();
                    if tctx.fill.is_none() && tctx.stroke.is_none() {
                        tctx.fill = Some(Rgb::new(0, 0, 0));
                    }
                    push_element(scene, &tctx, paints, segs);
                }
            }
        }
        "tspan" | "image" => {
            scene
                .warnings
                .push(format!("skipped non-vector element <{}>", tag));
        }
        "path" => {
            if let Some(segs) = node.attribute("d").map(parse_path_data) {
                push_element(scene, ctx, paints, segs);
            }
        }
        "line" => push_element(scene, ctx, paints, line_segs(&node)),
        "polyline" => push_element(scene, ctx, paints, poly_segs(&node, false)),
        "polygon" => push_element(scene, ctx, paints, poly_segs(&node, true)),
        "rect" => push_element(scene, ctx, paints, rect_segs(&node)),
        "circle" => push_element(scene, ctx, paints, circle_segs(&node)),
        "ellipse" => push_element(scene, ctx, paints, ellipse_segs(&node)),
        _ => {
            // Unknown element: descend in case it wraps drawables.
            walk(node, ctx, sheet, paints, scene, depth);
        }
    }
}

/// Resolve a `<use>`'s `href`/`xlink:href` target within the same document.
fn resolve_href<'a, 'input>(node: &Node<'a, 'input>) -> Option<Node<'a, 'input>> {
    let href = node
        .attribute("href")
        .or_else(|| node.attribute(("http://www.w3.org/1999/xlink", "href")))
        .or_else(|| node.attribute("xlink:href"))?;
    let id = href.trim().strip_prefix('#')?;
    node.document()
        .descendants()
        .find(|n| n.is_element() && n.attribute("id") == Some(id))
}

/// Apply transform + presentation attributes of `node` onto `ctx`. Priority
/// (low to high): CSS `<style>` rules, presentation attributes, inline
/// `style="..."` — the highest present value wins.
fn apply_common(
    node: &Node,
    sheet: &simplecss::StyleSheet,
    paints: &HashMap<String, GradientDef>,
    ctx: &mut Ctx,
) {
    if let Some(t) = node.attribute("transform") {
        ctx.transform = ctx.transform.mul(&parse_transform(t));
    }

    let css = crate::css::matched_properties(sheet, node);
    let inline = node.attribute("style").unwrap_or("");
    // inline style > presentation attribute > CSS rule.
    let get = |name: &str| -> Option<String> {
        style_lookup(inline, name)
            .or_else(|| node.attribute(name).map(|s| s.to_string()))
            .or_else(|| css.get(name).cloned())
    };

    if let Some(v) = get("stroke") {
        ctx.stroke = parse_paint(&v, paints);
    }
    if let Some(v) = get("fill") {
        // Record a gradient reference (for EMF+); the solid value is the
        // classic fallback.
        ctx.fill_gradient_id = paint_ref_id(&v).filter(|id| paints.contains_key(id));
        ctx.fill = parse_paint(&v, paints);
    }
    if let Some(v) = get("fill-opacity").and_then(|s| parse_opacity(&s)) {
        ctx.fill_opacity = v;
    }
    if let Some(v) = get("stroke-opacity").and_then(|s| parse_opacity(&s)) {
        ctx.stroke_opacity = v;
    }
    if let Some(v) = get("opacity").and_then(|s| parse_opacity(&s)) {
        // Group/element opacity composes multiplicatively (approximation).
        ctx.group_opacity *= v;
    }
    if let Some(v) = get("stroke-width").and_then(|s| parse_len(&s)) {
        ctx.stroke_width = v;
    }
    if let Some(v) = get("stroke-miterlimit").and_then(|s| f64::from_str(s.trim()).ok()) {
        ctx.miter = v;
    }
    if let Some(v) = get("stroke-linecap") {
        ctx.cap = match v.trim() {
            "round" => LineCap::Round,
            "square" => LineCap::Square,
            _ => LineCap::Flat,
        };
    }
    if let Some(v) = get("stroke-linejoin") {
        ctx.join = match v.trim() {
            "round" => LineJoin::Round,
            "bevel" => LineJoin::Bevel,
            _ => LineJoin::Miter,
        };
    }
    if let Some(v) = get("fill-rule") {
        ctx.fill_rule = if v.trim() == "evenodd" {
            FillRule::EvenOdd
        } else {
            FillRule::NonZero
        };
    }
    if let Some(v) = get("stroke-dasharray") {
        ctx.dash = parse_dash(&v);
    }
    // --- Font / text properties (inherited) ---
    if let Some(v) = get("font-family") {
        if let Some(first) = v.split(',').next() {
            let name = first.trim().trim_matches(|c| c == '"' || c == '\'').trim();
            if !name.is_empty() {
                ctx.font_family = name.to_string();
            }
        }
    }
    if let Some(v) = get("font-size").and_then(|s| parse_len(&s)) {
        ctx.font_size = v;
    }
    if let Some(v) = get("font-weight") {
        ctx.font_weight = match v.trim() {
            "normal" => 400,
            "bold" => 700,
            "bolder" => 700,
            "lighter" => 300,
            n => n.parse::<u16>().unwrap_or(400),
        };
    }
    if let Some(v) = get("font-style") {
        ctx.font_italic = matches!(v.trim(), "italic" | "oblique");
    }
    if let Some(v) = get("text-anchor") {
        ctx.text_anchor = match v.trim() {
            "middle" => glyph2path::Anchor::Middle,
            "end" => glyph2path::Anchor::End,
            _ => glyph2path::Anchor::Start,
        };
    }
}

/// Concatenate the text content of a `<text>`/`<tspan>` subtree, collapsing
/// runs of whitespace to single spaces (basic xml:space="default").
fn collect_text(node: &Node) -> String {
    let mut raw = String::new();
    for d in node.descendants() {
        if d.is_text() {
            if let Some(t) = d.text() {
                raw.push_str(t);
            }
        }
    }
    let mut out = String::new();
    let mut prev_ws = false;
    for c in raw.chars() {
        if c.is_whitespace() {
            if !prev_ws {
                out.push(' ');
            }
            prev_ws = true;
        } else {
            out.push(c);
            prev_ws = false;
        }
    }
    out.trim().to_string()
}

fn push_element(
    scene: &mut Scene,
    ctx: &Ctx,
    paints: &HashMap<String, GradientDef>,
    path: Vec<PathSeg>,
) {
    if path.is_empty() {
        return;
    }
    let stroke_alpha = opacity_to_alpha(ctx.stroke_opacity * ctx.group_opacity);
    let fill_alpha = opacity_to_alpha(ctx.fill_opacity * ctx.group_opacity);
    let stroke = ctx.stroke.map(|color| Stroke {
        color,
        width: ctx.stroke_width,
        dash: ctx.dash.clone(),
        cap: ctx.cap,
        join: ctx.join,
        miter_limit: ctx.miter,
        alpha: stroke_alpha,
    });
    let gradient = ctx
        .fill_gradient_id
        .as_ref()
        .and_then(|id| paints.get(id))
        .map(|def| resolve_gradient(def, &path));
    let fill = ctx.fill.map(|color| Fill {
        color,
        rule: ctx.fill_rule,
        alpha: fill_alpha,
        gradient,
    });
    if stroke.is_none() && fill.is_none() {
        return;
    }
    scene.elements.push(Element {
        transform: ctx.transform,
        path,
        stroke,
        fill,
    });
}

fn opacity_to_alpha(o: f64) -> u8 {
    (o.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Local-space bounding box of a path's anchor points.
fn path_bbox(path: &[PathSeg]) -> (f64, f64, f64, f64) {
    let mut min = Point::new(f64::INFINITY, f64::INFINITY);
    let mut max = Point::new(f64::NEG_INFINITY, f64::NEG_INFINITY);
    let mut add = |p: Point| {
        min.x = min.x.min(p.x);
        min.y = min.y.min(p.y);
        max.x = max.x.max(p.x);
        max.y = max.y.max(p.y);
    };
    for seg in path {
        match *seg {
            PathSeg::MoveTo(p) | PathSeg::LineTo(p) => add(p),
            PathSeg::CubicTo(a, b, p) => {
                add(a);
                add(b);
                add(p);
            }
            PathSeg::Arc { end, .. } => add(end),
            PathSeg::Close => {}
        }
    }
    if min.x.is_finite() {
        (min.x, min.y, max.x, max.y)
    } else {
        (0.0, 0.0, 1.0, 1.0)
    }
}

/// Resolve a gradient definition into local (userspace) coordinates for the
/// given path, expanding objectBoundingBox units against the path's bbox.
fn resolve_gradient(def: &GradientDef, path: &[PathSeg]) -> LinearGradient {
    let (x1, y1, x2, y2) = if def.object_bbox {
        let (bx0, by0, bx1, by1) = path_bbox(path);
        let w = bx1 - bx0;
        let h = by1 - by0;
        (
            bx0 + def.x1 * w,
            by0 + def.y1 * h,
            bx0 + def.x2 * w,
            by0 + def.y2 * h,
        )
    } else {
        (def.x1, def.y1, def.x2, def.y2)
    };
    LinearGradient {
        x1,
        y1,
        x2,
        y2,
        stops: def.stops.clone(),
    }
}

// --- Attribute helpers ------------------------------------------------------

fn attr_f64(node: &Node, name: &str) -> f64 {
    node.attribute(name).and_then(parse_len).unwrap_or(0.0)
}

fn attr_opt_f64(node: &Node, name: &str) -> Option<f64> {
    node.attribute(name).and_then(parse_len)
}

fn style_lookup(style: &str, name: &str) -> Option<String> {
    for decl in style.split(';') {
        let mut it = decl.splitn(2, ':');
        let k = it.next()?.trim();
        if k == name {
            return it.next().map(|v| v.trim().to_string());
        }
    }
    None
}

/// Parse a paint value into a solid color. `none` yields `None`; a paint
/// server reference `url(#id)` resolves to the gradient's average color (or
/// `None` if unknown).
fn parse_paint(v: &str, paints: &HashMap<String, GradientDef>) -> Option<Rgb> {
    let v = v.trim();
    if v.eq_ignore_ascii_case("none") || v.is_empty() {
        return None;
    }
    if let Some(id) = paint_ref_id(v) {
        // Fall back to any trailing solid color after the url(...) if the
        // reference is unknown (e.g. `url(#x) #ff0000`).
        return paints.get(&id).map(|def| def.average).or_else(|| {
            let rest = v.split(')').nth(1).unwrap_or("").trim();
            Color::from_str(rest)
                .ok()
                .map(|c| Rgb::new(c.red, c.green, c.blue))
        });
    }
    Color::from_str(v)
        .ok()
        .map(|c| Rgb::new(c.red, c.green, c.blue))
}

/// Parse an opacity value ("0.5" or "50%") into a 0..1 factor.
fn parse_opacity(v: &str) -> Option<f64> {
    let v = v.trim();
    if let Some(p) = v.strip_suffix('%') {
        p.trim().parse::<f64>().ok().map(|n| n / 100.0)
    } else {
        v.parse::<f64>().ok()
    }
    .map(|f| f.clamp(0.0, 1.0))
}

/// Extract the id from a `url(#id)` / `url('#id')` paint reference.
fn paint_ref_id(v: &str) -> Option<String> {
    let rest = v.trim().strip_prefix("url(")?;
    let inside = rest.split(')').next()?.trim();
    let inside = inside.trim_matches(|c| c == '"' || c == '\'');
    let id = inside.strip_prefix('#').unwrap_or(inside);
    Some(id.to_string())
}

fn parse_dash(v: &str) -> Option<Vec<f64>> {
    let v = v.trim();
    if v.eq_ignore_ascii_case("none") || v.is_empty() {
        return None;
    }
    let vals: Vec<f64> = LengthListParser::from(v)
        .filter_map(|r| r.ok())
        .map(|l| l.number)
        .collect();
    if vals.is_empty() {
        None
    } else {
        Some(vals)
    }
}

fn parse_transform(s: &str) -> Matrix {
    let mut m = Matrix::IDENTITY;
    for token in TransformListParser::from(s).flatten() {
        let t = match token {
            TransformListToken::Matrix { a, b, c, d, e, f } => Matrix::new(a, b, c, d, e, f),
            TransformListToken::Translate { tx, ty } => Matrix::translate(tx, ty),
            TransformListToken::Scale { sx, sy } => Matrix::scale(sx, sy),
            TransformListToken::Rotate { angle } => Matrix::rotate_deg(angle),
            TransformListToken::SkewX { angle } => Matrix::skew_x_deg(angle),
            TransformListToken::SkewY { angle } => Matrix::skew_y_deg(angle),
        };
        m = m.mul(&t);
    }
    m
}

// --- Shape -> segments ------------------------------------------------------

fn line_segs(node: &Node) -> Vec<PathSeg> {
    let x1 = attr_f64(node, "x1");
    let y1 = attr_f64(node, "y1");
    let x2 = attr_f64(node, "x2");
    let y2 = attr_f64(node, "y2");
    vec![
        PathSeg::MoveTo(Point::new(x1, y1)),
        PathSeg::LineTo(Point::new(x2, y2)),
    ]
}

fn poly_segs(node: &Node, close: bool) -> Vec<PathSeg> {
    let pts = node.attribute("points").unwrap_or("");
    let nums: Vec<f64> = LengthListParser::from(pts)
        .filter_map(|r| r.ok())
        .map(|l| l.number)
        .collect();
    let mut segs = Vec::new();
    let mut i = 0;
    while i + 1 < nums.len() {
        let p = Point::new(nums[i], nums[i + 1]);
        if i == 0 {
            segs.push(PathSeg::MoveTo(p));
        } else {
            segs.push(PathSeg::LineTo(p));
        }
        i += 2;
    }
    if close && !segs.is_empty() {
        segs.push(PathSeg::Close);
    }
    segs
}

fn rect_segs(node: &Node) -> Vec<PathSeg> {
    let x = attr_f64(node, "x");
    let y = attr_f64(node, "y");
    let w = attr_f64(node, "width");
    let h = attr_f64(node, "height");
    if w <= 0.0 || h <= 0.0 {
        return Vec::new();
    }
    let mut rx = attr_opt_f64(node, "rx");
    let mut ry = attr_opt_f64(node, "ry");
    // Per spec: a missing rx/ry mirrors the other.
    if rx.is_none() && ry.is_some() {
        rx = ry;
    }
    if ry.is_none() && rx.is_some() {
        ry = rx;
    }
    let rx = rx.unwrap_or(0.0).min(w / 2.0);
    let ry = ry.unwrap_or(0.0).min(h / 2.0);
    if rx <= 0.0 || ry <= 0.0 {
        return vec![
            PathSeg::MoveTo(Point::new(x, y)),
            PathSeg::LineTo(Point::new(x + w, y)),
            PathSeg::LineTo(Point::new(x + w, y + h)),
            PathSeg::LineTo(Point::new(x, y + h)),
            PathSeg::Close,
        ];
    }
    let arc = |end: Point| PathSeg::Arc {
        rx,
        ry,
        rot: 0.0,
        large: false,
        sweep: true,
        end,
    };
    vec![
        PathSeg::MoveTo(Point::new(x + rx, y)),
        PathSeg::LineTo(Point::new(x + w - rx, y)),
        arc(Point::new(x + w, y + ry)),
        PathSeg::LineTo(Point::new(x + w, y + h - ry)),
        arc(Point::new(x + w - rx, y + h)),
        PathSeg::LineTo(Point::new(x + rx, y + h)),
        arc(Point::new(x, y + h - ry)),
        PathSeg::LineTo(Point::new(x, y + ry)),
        arc(Point::new(x + rx, y)),
        PathSeg::Close,
    ]
}

fn circle_segs(node: &Node) -> Vec<PathSeg> {
    let cx = attr_f64(node, "cx");
    let cy = attr_f64(node, "cy");
    let r = attr_f64(node, "r");
    ellipse_from(cx, cy, r, r)
}

fn ellipse_segs(node: &Node) -> Vec<PathSeg> {
    let cx = attr_f64(node, "cx");
    let cy = attr_f64(node, "cy");
    let rx = attr_f64(node, "rx");
    let ry = attr_f64(node, "ry");
    ellipse_from(cx, cy, rx, ry)
}

fn ellipse_from(cx: f64, cy: f64, rx: f64, ry: f64) -> Vec<PathSeg> {
    if rx <= 0.0 || ry <= 0.0 {
        return Vec::new();
    }
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

/// Parse SVG path data into IR segments, expanding relative commands,
/// H/V lines, and quadratic/smooth curves.
fn parse_path_data(d: &str) -> Vec<PathSeg> {
    let mut segs = Vec::new();
    let mut cur = Point::new(0.0, 0.0);
    let mut start = Point::new(0.0, 0.0);
    // Reflection point of the previous cubic/quadratic control.
    let mut prev_cubic_ctrl: Option<Point> = None;
    let mut prev_quad_ctrl: Option<Point> = None;

    for token in PathParser::from(d).flatten() {
        match token {
            PathSegment::MoveTo { abs, x, y } => {
                cur = resolve(abs, cur, x, y);
                start = cur;
                segs.push(PathSeg::MoveTo(cur));
                prev_cubic_ctrl = None;
                prev_quad_ctrl = None;
            }
            PathSegment::LineTo { abs, x, y } => {
                cur = resolve(abs, cur, x, y);
                segs.push(PathSeg::LineTo(cur));
                prev_cubic_ctrl = None;
                prev_quad_ctrl = None;
            }
            PathSegment::HorizontalLineTo { abs, x } => {
                let nx = if abs { x } else { cur.x + x };
                cur = Point::new(nx, cur.y);
                segs.push(PathSeg::LineTo(cur));
                prev_cubic_ctrl = None;
                prev_quad_ctrl = None;
            }
            PathSegment::VerticalLineTo { abs, y } => {
                let ny = if abs { y } else { cur.y + y };
                cur = Point::new(cur.x, ny);
                segs.push(PathSeg::LineTo(cur));
                prev_cubic_ctrl = None;
                prev_quad_ctrl = None;
            }
            PathSegment::CurveTo {
                abs,
                x1,
                y1,
                x2,
                y2,
                x,
                y,
            } => {
                let c1 = resolve(abs, cur, x1, y1);
                let c2 = resolve(abs, cur, x2, y2);
                let end = resolve(abs, cur, x, y);
                segs.push(PathSeg::CubicTo(c1, c2, end));
                prev_cubic_ctrl = Some(c2);
                prev_quad_ctrl = None;
                cur = end;
            }
            PathSegment::SmoothCurveTo { abs, x2, y2, x, y } => {
                let c1 = reflect(prev_cubic_ctrl, cur);
                let c2 = resolve(abs, cur, x2, y2);
                let end = resolve(abs, cur, x, y);
                segs.push(PathSeg::CubicTo(c1, c2, end));
                prev_cubic_ctrl = Some(c2);
                prev_quad_ctrl = None;
                cur = end;
            }
            PathSegment::Quadratic { abs, x1, y1, x, y } => {
                let qc = resolve(abs, cur, x1, y1);
                let end = resolve(abs, cur, x, y);
                let (c1, c2) = quad_to_cubic(cur, qc, end);
                segs.push(PathSeg::CubicTo(c1, c2, end));
                prev_quad_ctrl = Some(qc);
                prev_cubic_ctrl = None;
                cur = end;
            }
            PathSegment::SmoothQuadratic { abs, x, y } => {
                let qc = reflect(prev_quad_ctrl, cur);
                let end = resolve(abs, cur, x, y);
                let (c1, c2) = quad_to_cubic(cur, qc, end);
                segs.push(PathSeg::CubicTo(c1, c2, end));
                prev_quad_ctrl = Some(qc);
                prev_cubic_ctrl = None;
                cur = end;
            }
            PathSegment::EllipticalArc {
                abs,
                rx,
                ry,
                x_axis_rotation,
                large_arc,
                sweep,
                x,
                y,
            } => {
                let end = resolve(abs, cur, x, y);
                segs.push(PathSeg::Arc {
                    rx,
                    ry,
                    rot: x_axis_rotation,
                    large: large_arc,
                    sweep,
                    end,
                });
                prev_cubic_ctrl = None;
                prev_quad_ctrl = None;
                cur = end;
            }
            PathSegment::ClosePath { .. } => {
                segs.push(PathSeg::Close);
                cur = start;
                prev_cubic_ctrl = None;
                prev_quad_ctrl = None;
            }
        }
    }
    segs
}

fn resolve(abs: bool, cur: Point, x: f64, y: f64) -> Point {
    if abs {
        Point::new(x, y)
    } else {
        Point::new(cur.x + x, cur.y + y)
    }
}

fn reflect(prev_ctrl: Option<Point>, cur: Point) -> Point {
    match prev_ctrl {
        Some(c) => Point::new(2.0 * cur.x - c.x, 2.0 * cur.y - c.y),
        None => cur,
    }
}

/// Elevate a quadratic Bezier to a cubic (exact).
fn quad_to_cubic(p0: Point, qc: Point, p1: Point) -> (Point, Point) {
    let c1 = Point::new(
        p0.x + 2.0 / 3.0 * (qc.x - p0.x),
        p0.y + 2.0 / 3.0 * (qc.y - p0.y),
    );
    let c2 = Point::new(
        p1.x + 2.0 / 3.0 * (qc.x - p1.x),
        p1.y + 2.0 / 3.0 * (qc.y - p1.y),
    );
    (c1, c2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rect_and_transform() {
        let svg = r##"<svg width="100" height="80">
            <g transform="translate(10,20)">
              <rect x="0" y="0" width="30" height="40" fill="#ff0000" stroke="#000000" stroke-width="2"/>
            </g>
        </svg>"##;
        let scene = svg_to_scene(svg).unwrap();
        assert_eq!(scene.elements.len(), 1);
        let el = &scene.elements[0];
        assert_eq!(el.transform, Matrix::translate(10.0, 20.0));
        assert_eq!(el.fill.as_ref().unwrap().color, Rgb::new(255, 0, 0));
        assert_eq!(el.stroke.as_ref().unwrap().width, 2.0);
    }

    #[test]
    fn parses_path_quadratic() {
        let svg = r#"<svg width="10" height="10"><path d="M0 0 Q5 5 10 0" stroke="black"/></svg>"#;
        let scene = svg_to_scene(svg).unwrap();
        let el = &scene.elements[0];
        assert!(matches!(el.path[0], PathSeg::MoveTo(_)));
        assert!(matches!(el.path[1], PathSeg::CubicTo(..)));
    }

    #[test]
    fn style_overrides_attribute() {
        let svg = r##"<svg width="10" height="10"><path d="M0 0L1 1" fill="red" style="fill:none;stroke:#00ff00"/></svg>"##;
        let scene = svg_to_scene(svg).unwrap();
        let el = &scene.elements[0];
        assert!(el.fill.is_none());
        assert_eq!(el.stroke.as_ref().unwrap().color, Rgb::new(0, 255, 0));
    }

    #[test]
    fn css_stylesheet_applies() {
        let svg = r##"<svg width="20" height="20"><style>.hot{fill:#ff0000} rect{stroke:#0000ff;stroke-width:3}</style><rect class="hot" x="0" y="0" width="10" height="10"/></svg>"##;
        let scene = svg_to_scene(svg).unwrap();
        assert_eq!(scene.elements.len(), 1);
        let el = &scene.elements[0];
        assert_eq!(el.fill.as_ref().unwrap().color, Rgb::new(255, 0, 0));
        assert_eq!(el.stroke.as_ref().unwrap().color, Rgb::new(0, 0, 255));
        assert_eq!(el.stroke.as_ref().unwrap().width, 3.0);
    }

    #[test]
    fn use_reference_is_expanded() {
        let svg = r##"<svg width="50" height="50"><defs><rect id="box" x="0" y="0" width="10" height="10" fill="#00ff00"/></defs><use href="#box" x="20" y="30"/></svg>"##;
        let scene = svg_to_scene(svg).unwrap();
        assert_eq!(scene.elements.len(), 1, "use should expand to one element");
        let el = &scene.elements[0];
        assert_eq!(el.fill.as_ref().unwrap().color, Rgb::new(0, 255, 0));
        // The rect's top-left (0,0) shifted by use x/y => (20,30).
        let p = el.transform.apply(Point::new(0.0, 0.0));
        assert!((p.x - 20.0).abs() < 1e-9 && (p.y - 30.0).abs() < 1e-9);
    }
}
