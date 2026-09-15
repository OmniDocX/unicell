//! Serialize a vector [`Scene`] to SVG 1.1 text.

use std::fmt::Write;

use vector_ir::{Element, FillRule, LineCap, LineJoin, LinearGradient, PathSeg, Scene};

/// Render a scene to an SVG document string.
pub fn scene_to_svg(scene: &Scene) -> String {
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?>\n");
    let _ = writeln!(
        out,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" \
         xmlns:xlink=\"http://www.w3.org/1999/xlink\" version=\"1.1\" \
         width=\"{}\" height=\"{}\" viewBox=\"{} {} {} {}\">",
        fmt_num(scene.width),
        fmt_num(scene.height),
        fmt_num(scene.min_x),
        fmt_num(scene.min_y),
        fmt_num(scene.width),
        fmt_num(scene.height),
    );
    // Emit gradient definitions for elements that carry one.
    let mut has_defs = false;
    for (i, el) in scene.elements.iter().enumerate() {
        if let Some(g) = el.fill.as_ref().and_then(|f| f.gradient.as_ref()) {
            if !has_defs {
                out.push_str("  <defs>\n");
                has_defs = true;
            }
            write_gradient_def(&mut out, i, g, &el.path);
        }
    }
    if has_defs {
        out.push_str("  </defs>\n");
    }
    for w in &scene.warnings {
        out.push_str("  <!-- ");
        out.push_str(&xml_comment_escape(w));
        out.push_str(" -->\n");
    }
    for (i, el) in scene.elements.iter().enumerate() {
        write_element(&mut out, el, i);
    }
    out.push_str("</svg>\n");
    out
}

/// Emit a `<linearGradient>`. Endpoints are converted to objectBoundingBox
/// fractions of the element's local-space bbox: tiny 0..1 numbers render
/// robustly even when the element carries an extreme fixed-point transform
/// (huge userSpaceOnUse coordinates break precision in some renderers).
fn write_gradient_def(out: &mut String, index: usize, g: &LinearGradient, path: &[PathSeg]) {
    let (bx0, by0, bx1, by1) = path_anchor_bbox(path);
    let w = (bx1 - bx0).abs().max(1e-12);
    let h = (by1 - by0).abs().max(1e-12);
    let fx1 = (g.x1 - bx0) / w;
    let fy1 = (g.y1 - by0) / h;
    let fx2 = (g.x2 - bx0) / w;
    let fy2 = (g.y2 - by0) / h;
    let _ = write!(
        out,
        "    <linearGradient id=\"omni-grad-{index}\" \
         x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\">",
        fmt_num(fx1),
        fmt_num(fy1),
        fmt_num(fx2),
        fmt_num(fy2),
    );
    out.push('\n');
    for s in &g.stops {
        let _ = write!(
            out,
            "      <stop offset=\"{}\" stop-color=\"{}\"",
            fmt_num(s.offset),
            s.color.to_hex()
        );
        if s.alpha != 255 {
            let _ = write!(out, " stop-opacity=\"{}\"", fmt_num(s.alpha as f64 / 255.0));
        }
        out.push_str("/>\n");
    }
    out.push_str("    </linearGradient>\n");
}

/// Local-space bbox of a path's anchor points.
fn path_anchor_bbox(path: &[PathSeg]) -> (f64, f64, f64, f64) {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    let mut add = |x: f64, y: f64| {
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    };
    for seg in path {
        match *seg {
            PathSeg::MoveTo(p) | PathSeg::LineTo(p) => add(p.x, p.y),
            PathSeg::CubicTo(a, b, p) => {
                add(a.x, a.y);
                add(b.x, b.y);
                add(p.x, p.y);
            }
            PathSeg::Arc { end, .. } => add(end.x, end.y),
            PathSeg::Close => {}
        }
    }
    if min_x.is_finite() {
        (min_x, min_y, max_x, max_y)
    } else {
        (0.0, 0.0, 1.0, 1.0)
    }
}

fn write_element(out: &mut String, el: &Element, index: usize) {
    if el.path.is_empty() {
        return;
    }
    out.push_str("  <path d=\"");
    write_path_data(out, &el.path);
    out.push('"');

    if !el.transform.is_identity() {
        let m = &el.transform;
        let _ = write!(
            out,
            " transform=\"matrix({} {} {} {} {} {})\"",
            fmt_num(m.a),
            fmt_num(m.b),
            fmt_num(m.c),
            fmt_num(m.d),
            fmt_num(m.e),
            fmt_num(m.f),
        );
    }

    match &el.fill {
        Some(f) => {
            if f.gradient.is_some() {
                let _ = write!(out, " fill=\"url(#omni-grad-{index})\"");
            } else {
                let _ = write!(out, " fill=\"{}\"", f.color.to_hex());
            }
            if f.rule == FillRule::EvenOdd {
                out.push_str(" fill-rule=\"evenodd\"");
            }
            if f.alpha != 255 {
                let _ = write!(out, " fill-opacity=\"{}\"", fmt_num(f.alpha as f64 / 255.0));
            }
        }
        None => out.push_str(" fill=\"none\""),
    }

    match &el.stroke {
        Some(s) => {
            let _ = write!(out, " stroke=\"{}\"", s.color.to_hex());
            // width 0 means a cosmetic hairline; emit an explicit small width.
            let w = if s.width <= 0.0 { 1.0 } else { s.width };
            let _ = write!(out, " stroke-width=\"{}\"", fmt_num(w));
            if s.alpha != 255 {
                let _ = write!(
                    out,
                    " stroke-opacity=\"{}\"",
                    fmt_num(s.alpha as f64 / 255.0)
                );
            }
            if let Some(dash) = &s.dash {
                if !dash.is_empty() {
                    out.push_str(" stroke-dasharray=\"");
                    for (i, d) in dash.iter().enumerate() {
                        if i > 0 {
                            out.push(',');
                        }
                        out.push_str(&fmt_num(*d));
                    }
                    out.push('"');
                }
            }
            match s.cap {
                LineCap::Round => out.push_str(" stroke-linecap=\"round\""),
                LineCap::Square => out.push_str(" stroke-linecap=\"square\""),
                LineCap::Flat => {}
            }
            match s.join {
                LineJoin::Round => out.push_str(" stroke-linejoin=\"round\""),
                LineJoin::Bevel => out.push_str(" stroke-linejoin=\"bevel\""),
                LineJoin::Miter => {
                    let _ = write!(out, " stroke-miterlimit=\"{}\"", fmt_num(s.miter_limit));
                }
            }
        }
        None => out.push_str(" stroke=\"none\""),
    }

    out.push_str("/>\n");
}

fn write_path_data(out: &mut String, segs: &[PathSeg]) {
    let mut first = true;
    for seg in segs {
        if !first {
            out.push(' ');
        }
        first = false;
        match *seg {
            PathSeg::MoveTo(p) => {
                let _ = write!(out, "M {} {}", fmt_num(p.x), fmt_num(p.y));
            }
            PathSeg::LineTo(p) => {
                let _ = write!(out, "L {} {}", fmt_num(p.x), fmt_num(p.y));
            }
            PathSeg::CubicTo(c1, c2, p) => {
                let _ = write!(
                    out,
                    "C {} {} {} {} {} {}",
                    fmt_num(c1.x),
                    fmt_num(c1.y),
                    fmt_num(c2.x),
                    fmt_num(c2.y),
                    fmt_num(p.x),
                    fmt_num(p.y),
                );
            }
            PathSeg::Arc {
                rx,
                ry,
                rot,
                large,
                sweep,
                end,
            } => {
                let _ = write!(
                    out,
                    "A {} {} {} {} {} {} {}",
                    fmt_num(rx),
                    fmt_num(ry),
                    fmt_num(rot),
                    large as u8,
                    sweep as u8,
                    fmt_num(end.x),
                    fmt_num(end.y),
                );
            }
            PathSeg::Close => out.push('Z'),
        }
    }
}

/// Format a number with shortest round-trippable representation, dropping a
/// trailing `.0` for integral values.
pub fn fmt_num(v: f64) -> String {
    if v == 0.0 {
        return "0".to_string();
    }
    if v.fract() == 0.0 && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    let mut s = format!("{}", v);
    if s == "-0" {
        s = "0".to_string();
    }
    s
}

fn xml_comment_escape(s: &str) -> String {
    s.replace("--", "- -")
}

#[cfg(test)]
mod tests {
    use super::*;
    use vector_ir::{Fill, Matrix, Point, Rgb, Stroke};

    #[test]
    fn simple_line() {
        let scene = Scene {
            width: 10.0,
            height: 10.0,
            min_x: 0.0,
            min_y: 0.0,
            elements: vec![Element {
                transform: Matrix::IDENTITY,
                path: vec![
                    PathSeg::MoveTo(Point::new(0.0, 0.0)),
                    PathSeg::LineTo(Point::new(10.0, 10.0)),
                ],
                stroke: Some(Stroke {
                    color: Rgb::new(255, 0, 0),
                    width: 2.0,
                    ..Default::default()
                }),
                fill: None,
            }],
            warnings: vec![],
        };
        let svg = scene_to_svg(&scene);
        assert!(svg.contains("M 0 0 L 10 10"));
        assert!(svg.contains("stroke=\"#ff0000\""));
        assert!(svg.contains("fill=\"none\""));
    }

    #[test]
    fn fmt_num_integers() {
        assert_eq!(fmt_num(5.0), "5");
        assert_eq!(fmt_num(-3.0), "-3");
        assert_eq!(fmt_num(2.5), "2.5");
        assert_eq!(fmt_num(0.0), "0");
    }

    #[test]
    fn fill_rule_evenodd() {
        let scene = Scene {
            width: 4.0,
            height: 4.0,
            min_x: 0.0,
            min_y: 0.0,
            elements: vec![Element {
                transform: Matrix::IDENTITY,
                path: vec![PathSeg::MoveTo(Point::new(0.0, 0.0)), PathSeg::Close],
                stroke: None,
                fill: Some(Fill::solid(Rgb::new(0, 0, 0), FillRule::EvenOdd)),
            }],
            warnings: vec![],
        };
        let svg = scene_to_svg(&scene);
        assert!(svg.contains("fill-rule=\"evenodd\""));
    }
}
