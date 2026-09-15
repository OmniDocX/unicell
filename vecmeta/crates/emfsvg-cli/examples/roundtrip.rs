use emf2svg::{emf_to_svg_with, Emf2SvgOptions};
use svg2emf::{svg_to_emf, EmitOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let source = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><circle cx="50" cy="50" r="25"/></svg>"#;
    let emf = svg_to_emf(
        source,
        EmitOptions {
            lossless: true,
            ..Default::default()
        },
    )?;
    let recovered = emf_to_svg_with(&emf, Emf2SvgOptions { lossless: true })?;
    assert_eq!(source, recovered);
    println!("Exact SVG source recovery passed ({} EMF bytes)", emf.len());
    Ok(())
}
