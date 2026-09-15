//! SVG -> EMF conversion via the shared vector IR.

mod css;
mod emf_emitter;
mod svg_parser;

pub use emf_emitter::{scene_to_emf, scene_to_records, EmitOptions};
pub use svg_parser::{svg_to_scene, SvgError};

use base64::Engine;
use emf_core::types::OMNI_KIND_SVG;
use emf_core::EmfRecord;
use vector_ir::Scene;

/// Marker id of the `<metadata>` element that carries the original EMF bytes
/// (base64), mirroring `emf2svg::EMF_SOURCE_METADATA_ID`.
pub const EMF_SOURCE_METADATA_ID: &str = "omnidoc-emf-source";

/// EMF bytes plus non-fatal compatibility information collected while parsing SVG.
#[derive(Debug, Clone, PartialEq)]
pub struct SvgToEmfResult {
    pub bytes: Vec<u8>,
    pub warnings: Vec<String>,
    pub recovered_embedded_source: bool,
}

/// Parse SVG text into a vector [`Scene`].
pub fn svg_to_ir(svg: &str) -> Result<Scene, SvgError> {
    svg_to_scene(svg)
}

/// Convert SVG text directly to an EMF byte buffer using default options.
pub fn svg_to_emf(svg: &str, opts: EmitOptions) -> Result<Vec<u8>, SvgError> {
    svg_to_emf_with_report(svg, opts).map(|result| result.bytes)
}

/// Convert SVG and retain warnings instead of silently discarding compatibility losses.
pub fn svg_to_emf_with_report(svg: &str, opts: EmitOptions) -> Result<SvgToEmfResult, SvgError> {
    // Byte-lossless fast path: if the SVG embeds an original EMF (produced by
    // `emf2svg` in lossless mode), reproduce it verbatim.
    if let Some(emf) = extract_embedded_emf(svg) {
        return Ok(SvgToEmfResult {
            bytes: emf,
            warnings: Vec::new(),
            recovered_embedded_source: true,
        });
    }

    let scene = svg_to_scene(svg)?;
    let warnings = scene.warnings.clone();
    let bytes = if opts.lossless {
        // Embed the original SVG in the EMF so EMF -> SVG can recover it.
        let (header, mut records) = scene_to_records(&scene, opts);
        let comment = EmfRecord::omni_comment(OMNI_KIND_SVG, svg.as_bytes());
        // Insert before the trailing EOF record.
        let pos = records.len().saturating_sub(1);
        records.insert(pos, comment);
        emf_core::write_emf(&header, &records)
    } else {
        scene_to_emf(&scene, opts)
    };
    Ok(SvgToEmfResult {
        bytes,
        warnings,
        recovered_embedded_source: false,
    })
}

#[cfg(test)]
mod report_tests {
    use super::*;

    #[test]
    fn reports_elements_that_cannot_be_preserved_as_vectors() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><image href="data:image/png;base64,AA==" width="10" height="10"/></svg>"#;
        let result = svg_to_emf_with_report(svg, EmitOptions::default()).unwrap();
        assert_eq!(result.warnings, vec!["skipped non-vector element <image>"]);
        assert!(!result.bytes.is_empty());
    }
}

/// Extract and decode an embedded EMF (base64) from an SVG `<metadata>` block,
/// if present.
pub fn extract_embedded_emf(svg: &str) -> Option<Vec<u8>> {
    let anchor = format!("id=\"{EMF_SOURCE_METADATA_ID}\"");
    let id_pos = svg.find(&anchor)?;
    // The metadata text is between the next '>' after the id and the closing tag.
    let after_tag = svg[id_pos..].find('>')? + id_pos + 1;
    let end = svg[after_tag..].find("</metadata>")? + after_tag;
    let b64: String = svg[after_tag..end]
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    base64::engine::general_purpose::STANDARD.decode(b64).ok()
}
