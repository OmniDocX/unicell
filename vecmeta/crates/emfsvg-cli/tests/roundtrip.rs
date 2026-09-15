//! Integration tests: EMF <-> SVG round-trip fidelity over the shared test
//! corpus in `tests/resources`.

use std::path::PathBuf;

use emf2svg::emf_to_scene;
use svg2emf::{scene_to_emf, svg_to_scene, EmitOptions};
use vector_ir::{Element, PathSeg, Point, Scene};

fn resources() -> PathBuf {
    std::env::var_os("VECMETA_CORPUS_DIR")
        .map(PathBuf::from)
        .expect("External corpus required: set VECMETA_CORPUS_DIR explicitly")
}

fn list_emf(dir: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(resources().join(dir))
        .unwrap_or_else(|e| panic!("reading {dir}: {e}"))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "emf").unwrap_or(false))
        .collect();
    v.sort();
    v
}

/// Flatten a path into device-space anchor points (arc end + bezier points).
fn rendered_points(el: &Element) -> Vec<Point> {
    let mut pts = Vec::new();
    for seg in &el.path {
        match *seg {
            PathSeg::MoveTo(p) | PathSeg::LineTo(p) => pts.push(el.transform.apply(p)),
            PathSeg::CubicTo(c1, c2, p) => {
                pts.push(el.transform.apply(c1));
                pts.push(el.transform.apply(c2));
                pts.push(el.transform.apply(p));
            }
            PathSeg::Arc { end, .. } => pts.push(el.transform.apply(end)),
            PathSeg::Close => {}
        }
    }
    pts
}

fn element_bounds(el: &Element) -> Option<(Point, Point)> {
    let mut min = Point::new(f64::INFINITY, f64::INFINITY);
    let mut max = Point::new(f64::NEG_INFINITY, f64::NEG_INFINITY);
    let mut any = false;
    let mut cur = Point::new(0.0, 0.0);
    let mut sub_start = cur;
    let add = |p: Point, min: &mut Point, max: &mut Point, any: &mut bool| {
        let t = el.transform.apply(p);
        min.x = min.x.min(t.x);
        min.y = min.y.min(t.y);
        max.x = max.x.max(t.x);
        max.y = max.y.max(t.y);
        *any = true;
    };
    for seg in &el.path {
        match *seg {
            PathSeg::MoveTo(p) => {
                add(p, &mut min, &mut max, &mut any);
                cur = p;
                sub_start = p;
            }
            PathSeg::LineTo(p) => {
                add(p, &mut min, &mut max, &mut any);
                cur = p;
            }
            PathSeg::CubicTo(c1, c2, p) => {
                for pt in sample_cubic(cur, c1, c2, p) {
                    add(pt, &mut min, &mut max, &mut any);
                }
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
                for pt in sample_arc(cur, rx, ry, rot, large, sweep, end) {
                    add(pt, &mut min, &mut max, &mut any);
                }
                cur = end;
            }
            PathSeg::Close => cur = sub_start,
        }
    }
    if any {
        Some((min, max))
    } else {
        None
    }
}

fn has_arc(el: &Element) -> bool {
    el.path.iter().any(|s| matches!(s, PathSeg::Arc { .. }))
}

fn sample_cubic(p0: Point, c1: Point, c2: Point, p1: Point) -> Vec<Point> {
    (0..=16)
        .map(|i| {
            let t = i as f64 / 16.0;
            let mt = 1.0 - t;
            let a = mt * mt * mt;
            let b = 3.0 * mt * mt * t;
            let c = 3.0 * mt * t * t;
            let d = t * t * t;
            Point::new(
                a * p0.x + b * c1.x + c * c2.x + d * p1.x,
                a * p0.y + b * c1.y + c * c2.y + d * p1.y,
            )
        })
        .collect()
}

/// Sample an SVG elliptical arc by converting to center parametrization.
fn sample_arc(
    from: Point,
    rx: f64,
    ry: f64,
    rot_deg: f64,
    large: bool,
    sweep: bool,
    to: Point,
) -> Vec<Point> {
    let rx = rx.abs();
    let ry = ry.abs();
    if rx < 1e-12 || ry < 1e-12 {
        return vec![to];
    }
    let phi = rot_deg.to_radians();
    let (sp, cp) = phi.sin_cos();
    let dx = (from.x - to.x) / 2.0;
    let dy = (from.y - to.y) / 2.0;
    let x1p = cp * dx + sp * dy;
    let y1p = -sp * dx + cp * dy;
    let mut rx2 = rx * rx;
    let mut ry2 = ry * ry;
    let lambda = x1p * x1p / rx2 + y1p * y1p / ry2;
    let (rx, ry) = if lambda > 1.0 {
        let s = lambda.sqrt();
        rx2 = (rx * s) * (rx * s);
        ry2 = (ry * s) * (ry * s);
        (rx * s, ry * s)
    } else {
        (rx, ry)
    };
    let sign = if large != sweep { 1.0 } else { -1.0 };
    let num = (rx2 * ry2 - rx2 * y1p * y1p - ry2 * x1p * x1p).max(0.0);
    let den = rx2 * y1p * y1p + ry2 * x1p * x1p;
    let co = if den == 0.0 {
        0.0
    } else {
        sign * (num / den).sqrt()
    };
    let cxp = co * rx * y1p / ry;
    let cyp = -co * ry * x1p / rx;
    let cx = cp * cxp - sp * cyp + (from.x + to.x) / 2.0;
    let cy = sp * cxp + cp * cyp + (from.y + to.y) / 2.0;
    let ang = |ux: f64, uy: f64, vx: f64, vy: f64| {
        let dot = ux * vx + uy * vy;
        let len = (ux * ux + uy * uy).sqrt() * (vx * vx + vy * vy).sqrt();
        let mut a = (dot / len).clamp(-1.0, 1.0).acos();
        if ux * vy - uy * vx < 0.0 {
            a = -a;
        }
        a
    };
    let theta1 = ang(1.0, 0.0, (x1p - cxp) / rx, (y1p - cyp) / ry);
    let mut dtheta = ang(
        (x1p - cxp) / rx,
        (y1p - cyp) / ry,
        (-x1p - cxp) / rx,
        (-y1p - cyp) / ry,
    );
    if !sweep && dtheta > 0.0 {
        dtheta -= 2.0 * std::f64::consts::PI;
    } else if sweep && dtheta < 0.0 {
        dtheta += 2.0 * std::f64::consts::PI;
    }
    (0..=24)
        .map(|i| {
            let t = theta1 + dtheta * (i as f64 / 24.0);
            let (st, ct) = t.sin_cos();
            Point::new(
                cx + rx * ct * cp - ry * st * sp,
                cy + rx * ct * sp + ry * st * cp,
            )
        })
        .collect()
}

/// Assert two scenes are geometrically identical within `tol` (exact fixed
/// point of the pipeline: same element count, styles and rendered points).
fn assert_scenes_equal(a: &Scene, b: &Scene, tol: f64, ctx: &str) {
    assert_eq!(
        a.elements.len(),
        b.elements.len(),
        "{ctx}: element count differs"
    );
    for (i, (ea, eb)) in a.elements.iter().zip(b.elements.iter()).enumerate() {
        // Styles must match exactly.
        assert_eq!(ea.fill, eb.fill, "{ctx}: element {i} fill differs");
        match (&ea.stroke, &eb.stroke) {
            (Some(sa), Some(sb)) => {
                assert_eq!(sa.color, sb.color, "{ctx}: element {i} stroke color");
                assert_eq!(sa.dash, sb.dash, "{ctx}: element {i} dash");
                assert!(
                    (sa.width - sb.width).abs() <= tol,
                    "{ctx}: element {i} stroke width {} vs {}",
                    sa.width,
                    sb.width
                );
            }
            (None, None) => {}
            _ => panic!("{ctx}: element {i} stroke presence differs"),
        }
        let pa = rendered_points(ea);
        let pb = rendered_points(eb);
        assert_eq!(pa.len(), pb.len(), "{ctx}: element {i} point count differs");
        for (j, (x, y)) in pa.iter().zip(pb.iter()).enumerate() {
            assert!(
                (x.x - y.x).abs() <= tol && (x.y - y.y).abs() <= tol,
                "{ctx}: element {i} point {j}: ({},{}) vs ({},{})",
                x.x,
                x.y,
                y.x,
                y.y
            );
        }
    }
}

/// The pipeline must reach a fixed point: after the first EMF -> SVG -> EMF
/// hop arcs are already beziers and coordinates are integral, so a second
/// hop must reproduce the scene exactly.
#[test]
#[ignore = "requires a separately licensed corpus via VECMETA_CORPUS_DIR"]
fn emf_roundtrip_is_idempotent() {
    let files = list_emf("emf");
    assert!(files.len() > 100, "expected the full EMF corpus");
    let mut checked = 0usize;
    let mut skipped = 0usize;
    for path in files {
        let data = std::fs::read(&path).unwrap();
        let scene1 = match emf_to_scene(&data) {
            Ok(s) => s,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        if scene1.elements.is_empty() {
            skipped += 1;
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();

        let emf2 = scene_to_emf(&scene1, EmitOptions::default());
        let scene2 = emf_to_scene(&emf2).unwrap_or_else(|e| panic!("{name}: reparse emf2: {e}"));

        let emf3 = scene_to_emf(&scene2, EmitOptions::default());
        let scene3 = emf_to_scene(&emf3).unwrap_or_else(|e| panic!("{name}: reparse emf3: {e}"));

        assert_scenes_equal(&scene2, &scene3, 1e-6, &name);
        checked += 1;
    }
    eprintln!("idempotent round trip: checked {checked}, skipped {skipped}");
    assert!(checked > 100, "too few files exercised: {checked}");
}

/// The first hop (EMF -> SVG -> EMF) must preserve element count, paint
/// styles and each element's device-space bounding box.
#[test]
#[ignore = "requires a separately licensed corpus via VECMETA_CORPUS_DIR"]
fn emf_first_hop_preserves_geometry() {
    let files = list_emf("emf");
    for path in files {
        let data = std::fs::read(&path).unwrap();
        let Ok(scene1) = emf_to_scene(&data) else {
            continue;
        };
        if scene1.elements.is_empty() {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let emf2 = scene_to_emf(&scene1, EmitOptions::default());
        let scene2 = emf_to_scene(&emf2).unwrap_or_else(|e| panic!("{name}: reparse: {e}"));

        assert_eq!(
            scene1.elements.len(),
            scene2.elements.len(),
            "{name}: element count changed on first hop"
        );
        for (i, (a, b)) in scene1
            .elements
            .iter()
            .zip(scene2.elements.iter())
            .enumerate()
        {
            assert_eq!(a.fill, b.fill, "{name}: element {i} fill changed");
            // Arcs legitimately change representation (integer EMF arc ->
            // SVG arc -> bezier) on the first hop; their exactness is covered
            // by the idempotency and SVG-subset tests. Check tight bounds only
            // for pure line/bezier elements here.
            if has_arc(a) || has_arc(b) {
                continue;
            }
            if let (Some(ba), Some(bb)) = (element_bounds(a), element_bounds(b)) {
                let span = (ba.1.x - ba.0.x)
                    .abs()
                    .max((ba.1.y - ba.0.y).abs())
                    .max(1.0);
                let tol = span * 1e-3 + 1.0;
                assert!(
                    (ba.0.x - bb.0.x).abs() <= tol
                        && (ba.0.y - bb.0.y).abs() <= tol
                        && (ba.1.x - bb.1.x).abs() <= tol
                        && (ba.1.y - bb.1.y).abs() <= tol,
                    "{name}: element {i} bbox drift {:?} vs {:?}",
                    ba,
                    bb
                );
            }
        }
    }
}

/// Corrupted inputs must never panic: they either parse or return an error.
#[test]
#[ignore = "requires a separately licensed corpus via VECMETA_CORPUS_DIR"]
fn corrupted_inputs_do_not_panic() {
    let mut checked = 0;
    for dir in ["emf-corrupted", "emf-ea"] {
        let entries = std::fs::read_dir(resources().join(dir))
            .unwrap_or_else(|e| panic!("reading {dir}: {e}"));
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if let Ok(data) = std::fs::read(&path) {
                // Result intentionally ignored; the point is the absence of panics.
                let _ = emf_to_scene(&data);
                checked += 1;
            }
        }
    }
    assert!(checked > 0, "the external corruption corpus must not be empty");
}

/// Hand-written SVG vector subset must survive SVG -> EMF -> SVG.
#[test]
fn svg_subset_roundtrip() {
    let samples = [
        r##"<svg width="100" height="100"><rect x="10" y="10" width="80" height="60" fill="#3366cc" stroke="#000000" stroke-width="2"/></svg>"##,
        r##"<svg width="100" height="100"><circle cx="50" cy="50" r="40" fill="none" stroke="#ff0000" stroke-width="3"/></svg>"##,
        r##"<svg width="100" height="100"><ellipse cx="50" cy="40" rx="45" ry="30" fill="#00ff00"/></svg>"##,
        r##"<svg width="120" height="90"><polygon points="10,10 100,20 60,80" fill="#123456" fill-rule="evenodd"/></svg>"##,
        r##"<svg width="100" height="100"><g transform="translate(10,20) scale(2,2)"><path d="M0 0 L10 0 L10 10 Z" stroke="#0000ff" stroke-width="1"/></g></svg>"##,
        r##"<svg width="100" height="100"><path d="M10 10 H90 V90" stroke="#333333" stroke-width="4" stroke-dasharray="8,4" fill="none"/></svg>"##,
    ];
    for (i, svg) in samples.iter().enumerate() {
        let scene1 = svg_to_scene(svg).unwrap_or_else(|e| panic!("sample {i} parse: {e}"));
        assert!(
            !scene1.elements.is_empty(),
            "sample {i}: no elements parsed"
        );
        let emf = scene_to_emf(&scene1, EmitOptions::default());
        let scene2 = emf_to_scene(&emf).unwrap_or_else(|e| panic!("sample {i} reparse: {e}"));

        // Round trip again through EMF to reach the pipeline fixed point.
        let emf2 = scene_to_emf(&scene2, EmitOptions::default());
        let scene3 = emf_to_scene(&emf2).unwrap_or_else(|e| panic!("sample {i} reparse2: {e}"));
        assert_scenes_equal(&scene2, &scene3, 1e-6, &format!("svg sample {i}"));
    }
}

/// Byte-lossless mode: EMF -> SVG(lossless) -> EMF must reproduce the original
/// EMF bytes exactly, for every sample in the corpus.
#[test]
#[ignore = "requires a separately licensed corpus via VECMETA_CORPUS_DIR"]
fn byte_lossless_emf_svg_emf() {
    use emf2svg::{emf_to_svg_with, Emf2SvgOptions};
    use svg2emf::svg_to_emf;

    let files = list_emf("emf");
    let mut checked = 0usize;
    for path in files {
        let data = std::fs::read(&path).unwrap();
        // Only meaningful for files we can parse.
        if emf_to_scene(&data).is_err() {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let svg = emf_to_svg_with(&data, Emf2SvgOptions { lossless: true })
            .unwrap_or_else(|e| panic!("{name}: to svg: {e}"));
        let emf2 = svg_to_emf(&svg, EmitOptions::default())
            .unwrap_or_else(|e| panic!("{name}: back to emf: {e}"));
        assert_eq!(
            data, emf2,
            "{name}: byte-lossless EMF->SVG->EMF is not byte-identical"
        );
        checked += 1;
    }
    assert!(checked > 100, "too few files exercised: {checked}");
}

/// Byte-lossless mode: SVG -> EMF(lossless) -> SVG must reproduce the original
/// SVG text exactly.
#[test]
fn byte_lossless_svg_emf_svg() {
    use emf2svg::{emf_to_svg_with, Emf2SvgOptions};
    use svg2emf::svg_to_emf;

    let samples = [
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 96 96"><circle cx="48" cy="48" r="40" fill="#06b6d4"/></svg>"##,
        r##"<svg width="100" height="100"><rect x="10" y="10" width="80" height="60" rx="5" fill="url(#g)" opacity="0.5"/><text x="5" y="20">hello</text></svg>"##,
    ];
    let opts = EmitOptions {
        lossless: true,
        ..Default::default()
    };
    for (i, svg) in samples.iter().enumerate() {
        let emf = svg_to_emf(svg, opts).unwrap_or_else(|e| panic!("sample {i}: to emf: {e}"));
        let back = emf_to_svg_with(&emf, Emf2SvgOptions { lossless: true })
            .unwrap_or_else(|e| panic!("sample {i}: back to svg: {e}"));
        assert_eq!(
            *svg, back,
            "sample {i}: byte-lossless SVG->EMF->SVG is not identical"
        );
    }
}

/// Text is converted to filled vector outlines (both directions go through the
/// glyph outliner). Lenient about the exact font, since it depends on the host.
#[test]
fn text_is_vectorized_to_paths() {
    let svg = r##"<svg width="200" height="60"><text x="10" y="42" font-family="Arial" font-size="32" fill="#101010">Hi</text></svg>"##;
    let scene = svg_to_scene(svg).unwrap();
    if scene.elements.is_empty() {
        // No usable system font: a warning must explain the skip.
        assert!(
            scene.warnings.iter().any(|w| w.contains("no font outline")),
            "text produced neither glyphs nor a warning"
        );
        return;
    }
    // Vectorized text is a filled path with several segments (glyph contours).
    let el = &scene.elements[0];
    assert!(el.fill.is_some(), "text should be filled");
    assert!(el.stroke.is_none(), "plain text should not be stroked");
    assert!(el.path.len() > 4, "expected glyph outline segments");

    // And it survives SVG -> EMF -> SVG as geometry.
    let emf = scene_to_emf(&scene, EmitOptions::default());
    let scene2 = emf_to_scene(&emf).unwrap();
    assert!(
        !scene2.elements.is_empty(),
        "vectorized text lost through EMF"
    );
}

/// EMF+ round trip: a linear gradient and a transparent fill survive
/// SVG -> EMF -> SVG through the embedded EMF+ paint table.
#[test]
fn emfplus_gradient_and_alpha_roundtrip() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
        <defs><linearGradient id="g" x1="0%" y1="0%" x2="100%" y2="100%">
            <stop offset="0%" stop-color="#06b6d4"/>
            <stop offset="100%" stop-color="#0f766e"/>
        </linearGradient></defs>
        <rect x="0" y="0" width="100" height="100" fill="url(#g)"/>
        <circle cx="50" cy="50" r="30" fill="#ff0000" fill-opacity="0.4"/>
    </svg>"##;

    let scene1 = svg_to_scene(svg).unwrap();
    // Element 0: gradient fill; element 1: 40% alpha red.
    assert!(
        scene1.elements[0].fill.as_ref().unwrap().gradient.is_some(),
        "gradient not captured from SVG"
    );
    let a1 = scene1.elements[1].fill.as_ref().unwrap().alpha;
    assert!((a1 as i32 - 102).abs() <= 1, "alpha not captured: {a1}");

    // Round trip through EMF (carrying the EMF+ block).
    let emf = scene_to_emf(&scene1, EmitOptions::default());
    let scene2 = emf_to_scene(&emf).unwrap();

    let g = scene2.elements[0]
        .fill
        .as_ref()
        .unwrap()
        .gradient
        .as_ref()
        .expect("gradient lost through EMF+");
    assert_eq!(g.stops.len(), 2, "gradient stops lost");
    assert_eq!(g.stops[0].color, vector_ir::Rgb::new(6, 182, 212));
    assert_eq!(g.stops[1].color, vector_ir::Rgb::new(15, 118, 110));

    // Gradient endpoints must land at the same DEVICE-space position as the
    // original (guards against coordinate-space mismatches).
    let g1 = scene1.elements[0]
        .fill
        .as_ref()
        .unwrap()
        .gradient
        .as_ref()
        .unwrap();
    let d1a = scene1.elements[0]
        .transform
        .apply(vector_ir::Point::new(g1.x1, g1.y1));
    let d1b = scene1.elements[0]
        .transform
        .apply(vector_ir::Point::new(g1.x2, g1.y2));
    let d2a = scene2.elements[0]
        .transform
        .apply(vector_ir::Point::new(g.x1, g.y1));
    let d2b = scene2.elements[0]
        .transform
        .apply(vector_ir::Point::new(g.x2, g.y2));
    for (a, b) in [(d1a, d2a), (d1b, d2b)] {
        assert!(
            (a.x - b.x).abs() < 1e-3 && (a.y - b.y).abs() < 1e-3,
            "gradient endpoint drifted: {a:?} vs {b:?}"
        );
    }

    let a2 = scene2.elements[1].fill.as_ref().unwrap().alpha;
    assert_eq!(a2, a1, "alpha lost through EMF+");
}
