//! EMF -> SVG conversion via the shared vector IR.

mod dc;
mod playback;
mod svg_writer;
mod text;

pub use playback::play;
pub use svg_writer::{fmt_num, scene_to_svg};

use base64::Engine;
use emf_core::types::OMNI_KIND_SVG;
use emf_core::{EmfError, EmfFile};
use vector_ir::Scene;

/// Marker id of the `<metadata>` element that carries the original EMF bytes
/// (base64) for the byte-lossless round trip.
pub const EMF_SOURCE_METADATA_ID: &str = "omnidoc-emf-source";

/// SVG text plus non-fatal compatibility information collected during EMF playback.
#[derive(Debug, Clone, PartialEq)]
pub struct EmfToSvgResult {
    pub svg: String,
    pub warnings: Vec<String>,
    pub recovered_embedded_source: bool,
}

/// Options for EMF -> SVG conversion.
#[derive(Debug, Clone, Copy, Default)]
pub struct Emf2SvgOptions {
    /// Byte-lossless mode: if the EMF embeds an original SVG document, return
    /// it verbatim; otherwise embed the original EMF bytes (base64) in the SVG
    /// so a later SVG -> EMF can reproduce this file exactly.
    pub lossless: bool,
}

/// Parse an EMF byte buffer into a vector [`Scene`].
pub fn emf_to_scene(data: &[u8]) -> Result<Scene, EmfError> {
    let file = EmfFile::parse(data)?;
    Ok(play(&file))
}

/// Convert an EMF byte buffer directly to an SVG document string.
pub fn emf_to_svg(data: &[u8]) -> Result<String, EmfError> {
    Ok(scene_to_svg(&emf_to_scene(data)?))
}

/// Convert an EMF byte buffer to SVG with options (incl. byte-lossless mode).
pub fn emf_to_svg_with(data: &[u8], opts: Emf2SvgOptions) -> Result<String, EmfError> {
    emf_to_svg_with_report(data, opts).map(|result| result.svg)
}

/// Convert EMF and retain warnings instead of hiding skipped record types.
pub fn emf_to_svg_with_report(
    data: &[u8],
    opts: Emf2SvgOptions,
) -> Result<EmfToSvgResult, EmfError> {
    let file = EmfFile::parse(data)?;

    if opts.lossless {
        // If this EMF was produced from an SVG in lossless mode, the original
        // SVG is embedded verbatim; return it for an exact round trip.
        for rec in &file.records {
            if let Some((OMNI_KIND_SVG, payload)) = rec.as_omni_payload() {
                if let Ok(svg) = String::from_utf8(payload.to_vec()) {
                    return Ok(EmfToSvgResult {
                        svg,
                        warnings: Vec::new(),
                        recovered_embedded_source: true,
                    });
                }
            }
        }
    }

    let scene = play(&file);
    let warnings = scene.warnings.clone();
    let svg = scene_to_svg(&scene);
    let svg = if opts.lossless {
        embed_emf_source(&svg, data)
    } else {
        svg
    };
    Ok(EmfToSvgResult {
        svg,
        warnings,
        recovered_embedded_source: false,
    })
}

/// Insert a `<metadata>` element holding the base64 of the original EMF right
/// after the opening `<svg ...>` tag.
fn embed_emf_source(svg: &str, emf: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(emf);
    let meta = format!(
        "\n  <metadata id=\"{EMF_SOURCE_METADATA_ID}\" encoding=\"base64\">{b64}</metadata>"
    );
    // Find the end of the opening <svg ...> tag.
    if let Some(open) = svg.find("<svg") {
        if let Some(gt) = svg[open..].find('>') {
            let pos = open + gt + 1;
            let mut out = String::with_capacity(svg.len() + meta.len());
            out.push_str(&svg[..pos]);
            out.push_str(&meta);
            out.push_str(&svg[pos..]);
            return out;
        }
    }
    svg.to_string()
}
