//! `emfsvg`: command-line EMF <-> SVG vector converter.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use svg2emf::EmitOptions;

#[derive(Parser)]
#[command(
    name = "emfsvg",
    about = "Lossless vector conversion between EMF and SVG",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Convert an EMF file to SVG.
    ToSvg {
        input: PathBuf,
        /// Output path (defaults to input with .svg extension).
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Print skipped-record warnings to stderr.
        #[arg(short, long)]
        verbose: bool,
        /// Byte-lossless mode: embed the original EMF so a later to-emf
        /// --lossless reproduces it exactly (or recover an embedded SVG).
        #[arg(long)]
        lossless: bool,
    },
    /// Convert an SVG file to EMF.
    ToEmf {
        input: PathBuf,
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Upper bound on the fixed-point precision factor. The emitter auto-
        /// selects the largest power of two under this that fits int32 range.
        #[arg(long, default_value_t = 16_777_216.0)]
        scale: f64,
        #[arg(short, long)]
        verbose: bool,
        /// Byte-lossless mode: embed the original SVG (or reproduce an
        /// embedded EMF exactly).
        #[arg(long)]
        lossless: bool,
    },
    /// EMF -> SVG -> EMF -> SVG and report geometric differences.
    Roundtrip {
        input: PathBuf,
        /// Maximum allowed per-coordinate difference.
        #[arg(long, default_value_t = 1e-6)]
        tolerance: f64,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::ToSvg {
            input,
            output,
            verbose,
            lossless,
        } => to_svg(&input, output.as_deref(), verbose, lossless),
        Command::ToEmf {
            input,
            output,
            scale,
            verbose,
            lossless,
        } => to_emf(&input, output.as_deref(), scale, verbose, lossless),
        Command::Roundtrip { input, tolerance } => roundtrip(&input, tolerance),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn default_output(input: &Path, ext: &str) -> PathBuf {
    input.with_extension(ext)
}

fn to_svg(
    input: &Path,
    output: Option<&Path>,
    verbose: bool,
    lossless: bool,
) -> Result<(), String> {
    let data = std::fs::read(input).map_err(|e| format!("reading {}: {e}", input.display()))?;
    if verbose {
        if let Ok(scene) = emf2svg::emf_to_scene(&data) {
            for w in &scene.warnings {
                eprintln!("warning: {w}");
            }
        }
    }
    let svg = emf2svg::emf_to_svg_with(&data, emf2svg::Emf2SvgOptions { lossless })
        .map_err(|e| e.to_string())?;
    let out = output
        .map(PathBuf::from)
        .unwrap_or_else(|| default_output(input, "svg"));
    std::fs::write(&out, svg).map_err(|e| format!("writing {}: {e}", out.display()))?;
    println!("wrote {}", out.display());
    Ok(())
}

fn to_emf(
    input: &Path,
    output: Option<&Path>,
    scale: f64,
    verbose: bool,
    lossless: bool,
) -> Result<(), String> {
    let svg =
        std::fs::read_to_string(input).map_err(|e| format!("reading {}: {e}", input.display()))?;
    if verbose {
        if let Ok(scene) = svg2emf::svg_to_scene(&svg) {
            for w in &scene.warnings {
                eprintln!("warning: {w}");
            }
        }
    }
    let opts = EmitOptions {
        scale,
        lossless,
        ..Default::default()
    };
    let bytes = svg2emf::svg_to_emf(&svg, opts).map_err(|e| e.to_string())?;
    let out = output
        .map(PathBuf::from)
        .unwrap_or_else(|| default_output(input, "emf"));
    std::fs::write(&out, bytes).map_err(|e| format!("writing {}: {e}", out.display()))?;
    println!("wrote {}", out.display());
    Ok(())
}

fn roundtrip(input: &Path, tolerance: f64) -> Result<(), String> {
    let data = std::fs::read(input).map_err(|e| format!("reading {}: {e}", input.display()))?;
    // The pipeline reaches a fixed point after the first EMF -> SVG -> EMF hop
    // (arcs become beziers, coordinates become integral). Compare that fixed
    // point against a further hop to measure true losslessness.
    let scene1 = emf2svg::emf_to_scene(&data).map_err(|e| e.to_string())?;
    let emf2 = svg2emf::scene_to_emf(&scene1, EmitOptions::default());
    let scene2 = emf2svg::emf_to_scene(&emf2).map_err(|e| e.to_string())?;
    let emf3 = svg2emf::scene_to_emf(&scene2, EmitOptions::default());
    let scene3 = emf2svg::emf_to_scene(&emf3).map_err(|e| e.to_string())?;

    let report = compare_scenes(&scene2, &scene3, tolerance);
    println!(
        "elements: {} (source) -> {} (normalized); fixed-point geometry: {}",
        scene1.elements.len(),
        scene2.elements.len(),
        if report.ok { "OK" } else { "MISMATCH" }
    );
    if !report.ok {
        return Err(format!(
            "round trip exceeded tolerance {tolerance}: max diff {}",
            report.max_diff
        ));
    }
    Ok(())
}

struct Report {
    ok: bool,
    max_diff: f64,
}

/// Compare two scenes by rendered (transformed) path anchor points.
fn compare_scenes(a: &vector_ir::Scene, b: &vector_ir::Scene, tol: f64) -> Report {
    let pa = rendered_points(a);
    let pb = rendered_points(b);
    if pa.len() != pb.len() {
        return Report {
            ok: false,
            max_diff: f64::INFINITY,
        };
    }
    let mut max_diff = 0.0f64;
    for (x, y) in pa.iter().zip(pb.iter()) {
        max_diff = max_diff.max((x.0 - y.0).abs()).max((x.1 - y.1).abs());
    }
    Report {
        ok: max_diff <= tol,
        max_diff,
    }
}

fn rendered_points(scene: &vector_ir::Scene) -> Vec<(f64, f64)> {
    use vector_ir::PathSeg;
    let mut pts = Vec::new();
    for el in &scene.elements {
        for seg in &el.path {
            let mut push = |p: vector_ir::Point| {
                let t = el.transform.apply(p);
                pts.push((t.x, t.y));
            };
            match *seg {
                PathSeg::MoveTo(p) | PathSeg::LineTo(p) => push(p),
                PathSeg::CubicTo(c1, c2, p) => {
                    push(c1);
                    push(c2);
                    push(p);
                }
                PathSeg::Arc { end, .. } => push(end),
                PathSeg::Close => {}
            }
        }
    }
    pts
}
