//! Loss-minimising DrawingML shape inspection and editing.
//!
//! The editor deliberately works on the original XML text instead of serialising a
//! new XML document.  Only the element (or attribute) addressed by an edit is
//! replaced, so extension lists, theme colour transforms, effects unknown to
//! UniCell and vendor-specific markup survive byte-for-byte.

use ironcalc::base::types::Theme;
use roxmltree::{Document, Node};
use serde_json::{Value, json};
use std::ops::Range;

const DRAWING_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const EMU_PER_POINT: f64 = 12_700.0;
const ANGLE_UNIT: f64 = 60_000.0;

/// Return the editable surface of the first shape in an anchor.
///
/// For a group shape, the first nested `sp`/`cxnSp` is selected.  This mirrors
/// Excel's depth-first selection order and, importantly, avoids flattening the
/// group transform.  If an empty `grpSp` has no child shape, its `grpSpPr` is
/// still exposed for transform/fill/effect edits.
pub fn parse_shape_model(anchor_xml: &str) -> Value {
    let Ok(doc) = Document::parse(anchor_xml) else {
        return json!({"error": "invalid DrawingML XML"});
    };
    let Some(shape) = find_target_shape(&doc) else {
        return json!({"error": "DrawingML anchor contains no shape"});
    };
    let properties = shape_properties(shape);

    let (rotation, flip_h, flip_v) = properties
        .and_then(|p| child(p, "xfrm"))
        .map(|xfrm| {
            (
                xfrm.attribute("rot")
                    .and_then(|v| v.parse::<f64>().ok())
                    .map(|v| v / ANGLE_UNIT)
                    .unwrap_or(0.0),
                xml_bool(xfrm.attribute("flipH")),
                xml_bool(xfrm.attribute("flipV")),
            )
        })
        .unwrap_or((0.0, false, false));

    let geometry = properties
        .and_then(|p| {
            child(p, "prstGeom")
                .and_then(|g| g.attribute("prst").map(str::to_string))
                .or_else(|| child(p, "custGeom").map(|_| "custom".to_string()))
        })
        .unwrap_or_default();

    json!({
        "text": shape_text(shape),
        "paragraphs": shape_paragraphs(shape),
        "geometry": geometry,
        "rotation": rotation,
        "flipH": flip_h,
        "flipV": flip_v,
        "fill": properties.map(parse_fill).unwrap_or_else(empty_fill),
        "line": properties.map(parse_line).unwrap_or(Value::Null),
        "effects": properties.map(parse_effects).unwrap_or_else(empty_effects),
    })
}

/// Apply a partial shape edit while retaining all unaddressed DrawingML.
///
/// Values use Excel-facing units: rotations and directions are degrees, line
/// widths/effect distances are points, and alpha/stop positions are fractions
/// in `[0, 1]` (percentage values in `[0, 100]` are accepted as input too).
pub fn apply_shape_edit(anchor_xml: &str, edit: &Value) -> Result<String, String> {
    if !edit.is_object() {
        return Err("shape edit must be a JSON object".to_string());
    }
    validate_anchor(anchor_xml)?;
    let mut xml = anchor_xml.to_string();

    if let Some(paragraphs) = edit.get("paragraphs") {
        xml = apply_text_structure(xml, paragraphs)?;
    } else if let Some(text) = edit.get("text") {
        let text = text.as_str().ok_or("shape text must be a string")?;
        xml = apply_text(xml, text)?;
    }
    if edit.get("rotation").is_some() || edit.get("flipH").is_some() || edit.get("flipV").is_some()
    {
        xml = apply_transform(xml, edit)?;
    }
    if let Some(geometry) = edit.get("geometry") {
        xml = apply_geometry(xml, geometry)?;
    }
    if let Some(fill) = edit.get("fill") {
        xml = apply_fill(xml, fill)?;
    }
    if let Some(line) = edit.get("line") {
        xml = apply_line(xml, line)?;
    }
    if let Some(effects) = edit.get("effects") {
        xml = apply_effects(xml, effects)?;
    }
    validate_anchor(&xml)?;
    Ok(xml)
}

fn validate_anchor(xml: &str) -> Result<(), String> {
    let doc = Document::parse(xml).map_err(|e| format!("invalid DrawingML XML: {e}"))?;
    if find_target_shape(&doc).is_none() {
        return Err("DrawingML anchor contains no editable sp/cxnSp/grpSp".to_string());
    }
    Ok(())
}

fn find_target_shape<'a, 'input>(doc: &'a Document<'input>) -> Option<Node<'a, 'input>> {
    // Prefer a real child shape.  A grpSp is a container and must not steal an
    // edit intended for its first nested child.
    doc.descendants()
        .find(|n| n.is_element() && matches!(n.tag_name().name(), "sp" | "cxnSp"))
        .or_else(|| {
            doc.descendants()
                .find(|n| n.is_element() && n.tag_name().name() == "grpSp")
        })
}

fn shape_properties<'a, 'input>(shape: Node<'a, 'input>) -> Option<Node<'a, 'input>> {
    let wanted = if shape.tag_name().name() == "grpSp" {
        "grpSpPr"
    } else {
        "spPr"
    };
    child(shape, wanted)
}

fn child<'a, 'input>(node: Node<'a, 'input>, local: &str) -> Option<Node<'a, 'input>> {
    node.children()
        .find(|n| n.is_element() && n.tag_name().name() == local)
}

fn xml_bool(value: Option<&str>) -> bool {
    matches!(value, Some("1" | "true" | "on"))
}

fn shape_text(shape: Node<'_, '_>) -> String {
    let Some(body) = child(shape, "txBody") else {
        return String::new();
    };
    let paragraphs: Vec<String> = body
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "p")
        .map(|p| {
            p.descendants()
                .filter(|n| n.is_element() && n.tag_name().name() == "t")
                .filter_map(|n| n.text())
                .collect::<String>()
        })
        .collect();
    paragraphs.join("\n")
}

fn paragraph_runs<'a, 'input>(paragraph: Node<'a, 'input>) -> Vec<Node<'a, 'input>> {
    paragraph
        .children()
        .filter(|node| node.is_element() && matches!(node.tag_name().name(), "r" | "fld"))
        .collect()
}

fn run_text(run: Node<'_, '_>) -> String {
    child(run, "t")
        .and_then(|node| node.text())
        .unwrap_or("")
        .to_string()
}

fn run_properties<'a, 'input>(run: Node<'a, 'input>) -> Option<Node<'a, 'input>> {
    child(run, "rPr")
}

fn run_font(properties: Node<'_, '_>) -> (Option<String>, Option<&'static str>) {
    for script in ["latin", "ea", "cs", "sym"] {
        if let Some(font) = child(properties, script) {
            if let Some(typeface) = font.attribute("typeface") {
                return (Some(typeface.to_string()), Some(script));
            }
        }
    }
    (None, None)
}

fn text_run_model(run: Node<'_, '_>) -> Value {
    let properties = run_properties(run);
    let (font, font_script) = properties.map(run_font).unwrap_or((None, None));
    let color = properties.and_then(fill_child).and_then(color_node);
    json!({
        "kind":run.tag_name().name(),
        "text":run_text(run),
        "font":font,
        "fontScript":font_script,
        "size":properties.and_then(|node| node.attribute("sz"))
            .and_then(|value| value.parse::<f64>().ok()).map(|value| value / 100.0),
        "bold":properties.and_then(|node| optional_xml_bool(node.attribute("b"))),
        "italic":properties.and_then(|node| optional_xml_bool(node.attribute("i"))),
        "underline":properties.and_then(|node| node.attribute("u")),
        "color":color.and_then(color_value),
        "colorSpec":color.map(drawing_color_spec),
        "alpha":color.map(color_alpha),
    })
}

fn text_run_model_with_source(run: Node<'_, '_>, source_index: usize) -> Value {
    let mut model = text_run_model(run);
    model
        .as_object_mut()
        .expect("text run model is an object")
        .insert("sourceIndex".to_string(), json!(source_index));
    model
}

fn text_paragraph_model(paragraph: Node<'_, '_>, source_index: usize) -> Value {
    let runs: Vec<Value> = paragraph_runs(paragraph)
        .into_iter()
        .enumerate()
        .map(|(run_index, run)| text_run_model_with_source(run, run_index))
        .collect();
    let text = runs
        .iter()
        .filter_map(|run| run.get("text").and_then(Value::as_str))
        .collect::<String>();
    json!({"sourceIndex":source_index, "text":text, "runs":runs})
}

fn shape_paragraphs(shape: Node<'_, '_>) -> Value {
    let Some(body) = child(shape, "txBody") else {
        return json!([]);
    };
    Value::Array(
        body.children()
            .filter(|node| node.is_element() && node.tag_name().name() == "p")
            .enumerate()
            .map(|(source_index, paragraph)| text_paragraph_model(paragraph, source_index))
            .collect(),
    )
}

fn empty_fill() -> Value {
    json!({
        "kind":"none", "color":Value::Null, "alpha":1.0, "angle":Value::Null,
        "stops":[], "directionType":Value::Null, "scaled":Value::Null,
        "path":Value::Null, "fillToRect":Value::Null, "tileRect":Value::Null,
        "flip":Value::Null, "rotWithShape":Value::Null,
    })
}

fn empty_effects() -> Value {
    json!({"shadow":{"enabled":false,"color":Value::Null,"alpha":1.0,"blur":0.0,"distance":0.0,"angle":0.0},"softEdge":0.0})
}

fn fill_child<'a, 'input>(properties: Node<'a, 'input>) -> Option<Node<'a, 'input>> {
    properties
        .children()
        .find(|n| n.is_element() && is_fill_name(n.tag_name().name()))
}

fn is_fill_name(name: &str) -> bool {
    matches!(
        name,
        "noFill" | "solidFill" | "gradFill" | "blipFill" | "pattFill" | "grpFill"
    )
}

fn color_node<'a, 'input>(container: Node<'a, 'input>) -> Option<Node<'a, 'input>> {
    container
        .descendants()
        .find(|n| n.is_element() && is_color_name(n.tag_name().name()))
}

fn is_color_name(name: &str) -> bool {
    matches!(
        name,
        "srgbClr" | "schemeClr" | "sysClr" | "scrgbClr" | "prstClr" | "hslClr"
    )
}

/// Lossless, editor-facing description of a DrawingML colour choice.
///
/// `color` remains the compact backwards-compatible token used by the edit API, while
/// `colorSpec` keeps the original colour space and the ordered transform pipeline.  Keeping
/// those two concepts separate is important: a theme colour is resolved for display only and
/// must not be written back as RGB (or have its transforms applied a second time).
pub(crate) fn drawing_color_spec(color: Node<'_, '_>) -> Value {
    let transforms: Vec<Value> = color
        .children()
        .filter(|node| node.is_element() && is_color_transform(node.tag_name().name()))
        .map(|node| {
            let mut model = serde_json::Map::new();
            model.insert(
                "type".to_string(),
                Value::String(node.tag_name().name().to_string()),
            );
            if let Some(raw) = node.attribute("val") {
                model.insert("raw".to_string(), Value::String(raw.to_string()));
                if let Some(number) = drawing_number(raw) {
                    model.insert("value".to_string(), json!(number));
                }
            }
            Value::Object(model)
        })
        .collect();
    let mut spec = serde_json::Map::new();
    spec.insert("transforms".to_string(), Value::Array(transforms));
    match color.tag_name().name() {
        "srgbClr" => {
            spec.insert("type".to_string(), json!("srgb"));
            spec.insert(
                "value".to_string(),
                color
                    .attribute("val")
                    .filter(|value| value.len() == 6)
                    .map(|value| json!(format!("#{}", value.to_ascii_uppercase())))
                    .unwrap_or(Value::Null),
            );
        }
        "schemeClr" => {
            spec.insert("type".to_string(), json!("scheme"));
            spec.insert(
                "value".to_string(),
                color
                    .attribute("val")
                    .map(Value::from)
                    .unwrap_or(Value::Null),
            );
        }
        "sysClr" => {
            spec.insert("type".to_string(), json!("system"));
            spec.insert(
                "value".to_string(),
                color
                    .attribute("val")
                    .map(Value::from)
                    .unwrap_or(Value::Null),
            );
            spec.insert(
                "lastColor".to_string(),
                color
                    .attribute("lastClr")
                    .filter(|value| value.len() == 6)
                    .map(|value| json!(format!("#{}", value.to_ascii_uppercase())))
                    .unwrap_or(Value::Null),
            );
        }
        "prstClr" => {
            spec.insert("type".to_string(), json!("preset"));
            spec.insert(
                "value".to_string(),
                color
                    .attribute("val")
                    .map(Value::from)
                    .unwrap_or(Value::Null),
            );
        }
        "scrgbClr" => {
            spec.insert("type".to_string(), json!("scrgb"));
            for component in ["r", "g", "b"] {
                spec.insert(
                    component.to_string(),
                    color
                        .attribute(component)
                        .and_then(drawing_number)
                        .map(Value::from)
                        .unwrap_or(Value::Null),
                );
            }
        }
        "hslClr" => {
            spec.insert("type".to_string(), json!("hsl"));
            for component in ["hue", "sat", "lum"] {
                spec.insert(
                    component.to_string(),
                    color
                        .attribute(component)
                        .and_then(drawing_number)
                        .map(Value::from)
                        .unwrap_or(Value::Null),
                );
            }
        }
        _ => {
            spec.insert("type".to_string(), json!("unknown"));
            spec.insert("value".to_string(), Value::Null);
        }
    }
    Value::Object(spec)
}

fn drawing_number(raw: &str) -> Option<f64> {
    raw.strip_suffix('%')
        .map(str::trim)
        .unwrap_or(raw)
        .parse::<f64>()
        .ok()
}

fn is_color_transform(name: &str) -> bool {
    matches!(
        name,
        "tint"
            | "shade"
            | "comp"
            | "inv"
            | "gray"
            | "alpha"
            | "alphaOff"
            | "alphaMod"
            | "hue"
            | "hueOff"
            | "hueMod"
            | "sat"
            | "satOff"
            | "satMod"
            | "lum"
            | "lumOff"
            | "lumMod"
            | "red"
            | "redOff"
            | "redMod"
            | "green"
            | "greenOff"
            | "greenMod"
            | "blue"
            | "blueOff"
            | "blueMod"
            | "gamma"
            | "invGamma"
    )
}

fn is_non_alpha_color_transform(name: &str) -> bool {
    is_color_transform(name) && !matches!(name, "alpha" | "alphaOff" | "alphaMod")
}

/// Resolve a DrawingML colour against the active workbook theme for previews only.
/// The caller must continue storing/exporting the original token and transform children.
pub(crate) fn resolve_drawing_color(color: Node<'_, '_>, theme: &Theme) -> Option<(String, f64)> {
    let mut rgb = match color.tag_name().name() {
        "srgbClr" => parse_hex_rgb(color.attribute("val")?)?,
        "schemeClr" => parse_hex_rgb(theme_scheme_color(theme, color.attribute("val")?)?)?,
        "sysClr" => color
            .attribute("lastClr")
            .and_then(parse_hex_rgb)
            .or_else(|| system_color(color.attribute("val")?).and_then(parse_hex_rgb))?,
        "prstClr" => preset_color(color.attribute("val")?)?,
        "scrgbClr" => ["r", "g", "b"].map(|component| {
            linear_to_srgb(
                color
                    .attribute(component)
                    .and_then(drawing_fraction)
                    .unwrap_or(0.0)
                    .clamp(0.0, 1.0),
            )
        }),
        "hslClr" => hsl_to_rgb([
            color
                .attribute("hue")
                .and_then(drawing_angle_fraction)
                .unwrap_or(0.0),
            color
                .attribute("sat")
                .and_then(drawing_fraction)
                .unwrap_or(0.0),
            color
                .attribute("lum")
                .and_then(drawing_fraction)
                .unwrap_or(0.0),
        ]),
        _ => return None,
    };
    let mut alpha = 1.0;
    for transform in color.children().filter(|node| node.is_element()) {
        let name = transform.tag_name().name();
        let amount = transform
            .attribute("val")
            .and_then(drawing_fraction)
            .unwrap_or(0.0);
        match name {
            // DrawingML tint is the fraction of the original colour which remains:
            // tint=0 is white, tint=100000 is unchanged.
            "tint" => rgb = rgb.map(|component| component * amount + 1.0 - amount),
            "shade" => rgb = rgb.map(|component| component * amount),
            "alpha" => alpha = amount,
            "alphaMod" => alpha *= amount,
            "alphaOff" => alpha += amount,
            "red" => rgb[0] = amount,
            "redMod" => rgb[0] *= amount,
            "redOff" => rgb[0] += amount,
            "green" => rgb[1] = amount,
            "greenMod" => rgb[1] *= amount,
            "greenOff" => rgb[1] += amount,
            "blue" => rgb[2] = amount,
            "blueMod" => rgb[2] *= amount,
            "blueOff" => rgb[2] += amount,
            "hue" | "hueMod" | "hueOff" | "sat" | "satMod" | "satOff" | "lum" | "lumMod"
            | "lumOff" => {
                let mut hsl = rgb_to_hsl(rgb);
                match name {
                    "hue" => {
                        hsl[0] = transform
                            .attribute("val")
                            .and_then(drawing_angle_fraction)
                            .unwrap_or(0.0)
                    }
                    "hueMod" => hsl[0] *= amount,
                    "hueOff" => {
                        hsl[0] += transform
                            .attribute("val")
                            .and_then(drawing_angle_fraction)
                            .unwrap_or(0.0)
                    }
                    "sat" => hsl[1] = amount,
                    "satMod" => hsl[1] *= amount,
                    "satOff" => hsl[1] += amount,
                    "lum" => hsl[2] = amount,
                    "lumMod" => hsl[2] *= amount,
                    "lumOff" => hsl[2] += amount,
                    _ => unreachable!(),
                }
                rgb = hsl_to_rgb(hsl);
            }
            "comp" => {
                let mut hsl = rgb_to_hsl(rgb);
                hsl[0] += 0.5;
                rgb = hsl_to_rgb(hsl);
            }
            "inv" => rgb = rgb.map(|component| 1.0 - component),
            "gray" => {
                let gray = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
                rgb = [gray, gray, gray];
            }
            "gamma" => rgb = rgb.map(|component| linear_to_srgb(component.clamp(0.0, 1.0))),
            "invGamma" => rgb = rgb.map(|component| srgb_to_linear(component.clamp(0.0, 1.0))),
            _ => {}
        }
        rgb = rgb.map(|component| component.clamp(0.0, 1.0));
        alpha = alpha.clamp(0.0, 1.0);
    }
    Some((rgb_hex(rgb), alpha))
}

fn parse_hex_rgb(value: &str) -> Option<[f64; 3]> {
    let value = value.trim().trim_start_matches('#');
    if value.len() != 6 || !value.chars().all(|character| character.is_ascii_hexdigit()) {
        return None;
    }
    Some([
        u8::from_str_radix(&value[0..2], 16).ok()? as f64 / 255.0,
        u8::from_str_radix(&value[2..4], 16).ok()? as f64 / 255.0,
        u8::from_str_radix(&value[4..6], 16).ok()? as f64 / 255.0,
    ])
}

fn rgb_hex(rgb: [f64; 3]) -> String {
    format!(
        "#{:02X}{:02X}{:02X}",
        (rgb[0].clamp(0.0, 1.0) * 255.0).round() as u8,
        (rgb[1].clamp(0.0, 1.0) * 255.0).round() as u8,
        (rgb[2].clamp(0.0, 1.0) * 255.0).round() as u8,
    )
}

fn drawing_fraction(raw: &str) -> Option<f64> {
    if let Some(percent) = raw.trim().strip_suffix('%') {
        percent
            .trim()
            .parse::<f64>()
            .ok()
            .map(|value| value / 100.0)
    } else {
        raw.trim()
            .parse::<f64>()
            .ok()
            .map(|value| value / 100_000.0)
    }
}

fn drawing_angle_fraction(raw: &str) -> Option<f64> {
    if let Some(degrees) = raw.trim().strip_suffix("deg") {
        degrees
            .trim()
            .parse::<f64>()
            .ok()
            .map(|value| value / 360.0)
    } else {
        raw.trim()
            .parse::<f64>()
            .ok()
            .map(|value| value / 60_000.0 / 360.0)
    }
}

fn theme_scheme_color<'a>(theme: &'a Theme, name: &str) -> Option<&'a str> {
    match name {
        "dk1" | "tx1" => Some(&theme.dk1),
        "lt1" | "bg1" => Some(&theme.lt1),
        "dk2" | "tx2" => Some(&theme.dk2),
        "lt2" | "bg2" => Some(&theme.lt2),
        "accent1" => Some(&theme.accent1),
        "accent2" => Some(&theme.accent2),
        "accent3" => Some(&theme.accent3),
        "accent4" => Some(&theme.accent4),
        "accent5" => Some(&theme.accent5),
        "accent6" => Some(&theme.accent6),
        "hlink" => Some(&theme.hlink),
        "folHlink" => Some(&theme.fol_hlink),
        // phClr is supplied by a style-matrix caller and cannot be guessed from the theme.
        _ => None,
    }
}

fn system_color(name: &str) -> Option<&'static str> {
    match name {
        "window" => Some("#FFFFFF"),
        "windowText" | "btnText" | "menuText" | "infoText" | "captionText" => Some("#000000"),
        "btnFace" | "menu" => Some("#F0F0F0"),
        "highlight" => Some("#3399FF"),
        "highlightText" => Some("#FFFFFF"),
        "grayText" => Some("#6D6D6D"),
        "infoBk" => Some("#FFFFE1"),
        "activeCaption" => Some("#99B4D1"),
        "inactiveCaption" => Some("#BFCDDB"),
        "inactiveCaptionText" => Some("#434E54"),
        "appWorkspace" => Some("#ABABAB"),
        "btnHighlight" | "threeDHighlight" => Some("#FFFFFF"),
        "btnShadow" => Some("#A0A0A0"),
        "threeDDkShadow" => Some("#696969"),
        "threeDLight" => Some("#E3E3E3"),
        "hotLight" => Some("#0066CC"),
        "scrollBar" => Some("#C8C8C8"),
        "background" => Some("#000000"),
        _ => None,
    }
}

fn preset_color(name: &str) -> Option<[f64; 3]> {
    let expanded = if let Some(rest) = name
        .strip_prefix("dk")
        .filter(|rest| rest.chars().next().map(char::is_uppercase).unwrap_or(false))
    {
        format!("dark{rest}")
    } else if let Some(rest) = name
        .strip_prefix("lt")
        .filter(|rest| rest.chars().next().map(char::is_uppercase).unwrap_or(false))
    {
        format!("light{rest}")
    } else if let Some(rest) = name
        .strip_prefix("med")
        .filter(|rest| rest.chars().next().map(char::is_uppercase).unwrap_or(false))
    {
        format!("medium{rest}")
    } else {
        name.to_string()
    };
    let parsed = expanded.parse::<svgtypes::Color>().ok()?;
    Some([
        parsed.red as f64 / 255.0,
        parsed.green as f64 / 255.0,
        parsed.blue as f64 / 255.0,
    ])
}

fn linear_to_srgb(value: f64) -> f64 {
    if value <= 0.003_130_8 {
        12.92 * value
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

fn srgb_to_linear(value: f64) -> f64 {
    if value <= 0.040_45 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn rgb_to_hsl([r, g, b]: [f64; 3]) -> [f64; 3] {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let lightness = (max + min) / 2.0;
    if (max - min).abs() < f64::EPSILON {
        return [0.0, 0.0, lightness];
    }
    let delta = max - min;
    let saturation = if lightness > 0.5 {
        delta / (2.0 - max - min)
    } else {
        delta / (max + min)
    };
    let mut hue = if (max - r).abs() < f64::EPSILON {
        (g - b) / delta + if g < b { 6.0 } else { 0.0 }
    } else if (max - g).abs() < f64::EPSILON {
        (b - r) / delta + 2.0
    } else {
        (r - g) / delta + 4.0
    };
    hue /= 6.0;
    [hue, saturation, lightness]
}

fn hsl_to_rgb([hue, saturation, lightness]: [f64; 3]) -> [f64; 3] {
    let hue = hue.rem_euclid(1.0);
    let saturation = saturation.clamp(0.0, 1.0);
    let lightness = lightness.clamp(0.0, 1.0);
    if saturation == 0.0 {
        return [lightness, lightness, lightness];
    }
    let q = if lightness < 0.5 {
        lightness * (1.0 + saturation)
    } else {
        lightness + saturation - lightness * saturation
    };
    let p = 2.0 * lightness - q;
    let channel = |offset: f64| {
        let value = (hue + offset).rem_euclid(1.0);
        if value < 1.0 / 6.0 {
            p + (q - p) * 6.0 * value
        } else if value < 0.5 {
            q
        } else if value < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - value) * 6.0
        } else {
            p
        }
    };
    [channel(1.0 / 3.0), channel(0.0), channel(-1.0 / 3.0)]
}

fn color_value(color: Node<'_, '_>) -> Option<String> {
    match color.tag_name().name() {
        "srgbClr" => color
            .attribute("val")
            .map(|v| format!("#{}", v.to_ascii_uppercase())),
        "schemeClr" => color.attribute("val").map(str::to_string),
        "sysClr" => color
            .attribute("lastClr")
            .map(|v| format!("#{}", v.to_ascii_uppercase()))
            .or_else(|| color.attribute("val").map(|v| format!("sys:{v}"))),
        "prstClr" => color.attribute("val").map(|v| format!("preset:{v}")),
        "scrgbClr" => Some(format!(
            "scrgb({},{},{})",
            color.attribute("r").unwrap_or("0"),
            color.attribute("g").unwrap_or("0"),
            color.attribute("b").unwrap_or("0")
        )),
        "hslClr" => Some(format!(
            "hsl({},{},{})",
            color.attribute("hue").unwrap_or("0"),
            color.attribute("sat").unwrap_or("0"),
            color.attribute("lum").unwrap_or("0")
        )),
        _ => color.attribute("val").map(str::to_string),
    }
}

fn color_alpha(color: Node<'_, '_>) -> f64 {
    let mut alpha = 1.0;
    for transform in color.children().filter(|node| node.is_element()) {
        let value = transform
            .attribute("val")
            .and_then(percentage_fraction)
            .unwrap_or(0.0);
        match transform.tag_name().name() {
            "alpha" => alpha = value,
            "alphaMod" => alpha *= value,
            "alphaOff" => alpha += value,
            _ => {}
        }
        alpha = alpha.clamp(0.0, 1.0);
    }
    alpha
}

fn optional_xml_bool(value: Option<&str>) -> Option<bool> {
    value.map(|value| matches!(value, "1" | "true" | "on" | "t"))
}

fn percentage_fraction(value: &str) -> Option<f64> {
    if let Some(percent) = value.strip_suffix('%') {
        percent
            .trim()
            .parse::<f64>()
            .ok()
            .map(|value| value / 100.0)
    } else {
        value.parse::<f64>().ok().map(|value| value / 100_000.0)
    }
}

fn relative_rect_model(rect: Option<Node<'_, '_>>) -> Value {
    let Some(rect) = rect else {
        return Value::Null;
    };
    json!({
        "l":rect.attribute("l").and_then(percentage_fraction),
        "t":rect.attribute("t").and_then(percentage_fraction),
        "r":rect.attribute("r").and_then(percentage_fraction),
        "b":rect.attribute("b").and_then(percentage_fraction),
    })
}

fn gradient_stop_model(stop: Node<'_, '_>, source_index: usize) -> Value {
    let color = color_node(stop);
    json!({
        // `sourceIndex` is an editor-only identity.  It lets a reordered/deleted
        // stop carry its original opaque DrawingML (theme transforms, extensions,
        // and vendor markup) instead of borrowing the XML from its new array slot.
        "sourceIndex":source_index,
        "position":stop.attribute("pos").and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0) / 100_000.0,
        "color":color.and_then(color_value),
        "colorSpec":color.map(drawing_color_spec),
        "alpha":color.map(color_alpha).unwrap_or(1.0),
    })
}

fn parse_fill(properties: Node<'_, '_>) -> Value {
    let Some(fill) = fill_child(properties) else {
        return empty_fill();
    };
    match fill.tag_name().name() {
        "noFill" => empty_fill(),
        "solidFill" => {
            let color = color_node(fill);
            json!({
                "kind":"solid",
                "color":color.and_then(color_value),
                "colorSpec":color.map(drawing_color_spec),
                "alpha":color.map(color_alpha).unwrap_or(1.0),
                "angle":Value::Null,
                "stops":[],
                "directionType":Value::Null,
                "scaled":Value::Null,
                "path":Value::Null,
                "fillToRect":Value::Null,
                "tileRect":Value::Null,
                "flip":Value::Null,
                "rotWithShape":Value::Null,
            })
        }
        "gradFill" => {
            let stops: Vec<Value> = fill
                .descendants()
                .filter(|n| n.is_element() && n.tag_name().name() == "gs")
                .enumerate()
                .map(|(source_index, stop)| gradient_stop_model(stop, source_index))
                .collect();
            let first = fill
                .descendants()
                .find(|n| n.is_element() && n.tag_name().name() == "gs")
                .and_then(color_node);
            let angle = child(fill, "lin")
                .and_then(|n| n.attribute("ang"))
                .and_then(|v| v.parse::<f64>().ok())
                .map(|v| v / ANGLE_UNIT);
            let linear = child(fill, "lin");
            let path = child(fill, "path");
            json!({
                "kind":"gradient",
                "color":first.and_then(color_value),
                "colorSpec":first.map(drawing_color_spec),
                "alpha":first.map(color_alpha).unwrap_or(1.0),
                "angle":angle,
                "stops":stops,
                "directionType":if linear.is_some() { Some("linear") } else if path.is_some() { Some("path") } else { None },
                "scaled":linear.and_then(|node| optional_xml_bool(node.attribute("scaled"))),
                "path":path.and_then(|node| node.attribute("path")),
                "fillToRect":relative_rect_model(path.and_then(|node| child(node, "fillToRect"))),
                "tileRect":relative_rect_model(child(fill, "tileRect")),
                "flip":fill.attribute("flip"),
                "rotWithShape":optional_xml_bool(fill.attribute("rotWithShape")),
            })
        }
        _ => {
            json!({
                "kind":"other", "color":Value::Null, "alpha":1.0,
                "angle":Value::Null, "stops":[], "directionType":Value::Null,
                "scaled":Value::Null, "path":Value::Null,
                "fillToRect":Value::Null, "tileRect":Value::Null,
                "flip":Value::Null, "rotWithShape":Value::Null,
            })
        }
    }
}

fn parse_line(properties: Node<'_, '_>) -> Value {
    let Some(line) = child(properties, "ln") else {
        return Value::Null;
    };
    let color = color_node(line);
    let dash = child(line, "prstDash")
        .and_then(|n| n.attribute("val"))
        .unwrap_or("solid");
    json!({
        "color":color.and_then(color_value),
        "colorSpec":color.map(drawing_color_spec),
        "alpha":color.map(color_alpha).unwrap_or(1.0),
        "width":line.attribute("w").and_then(|v| v.parse::<f64>().ok()).map(|v| v / EMU_PER_POINT).unwrap_or(0.0),
        "dash":dash,
    })
}

fn parse_effects(properties: Node<'_, '_>) -> Value {
    let effect_list = child(properties, "effectLst");
    let shadow = effect_list.and_then(|e| child(e, "outerShdw"));
    let soft_edge = effect_list.and_then(|e| child(e, "softEdge"));
    let shadow_color = shadow.and_then(color_node);
    json!({
        "shadow":{
            "enabled":shadow.is_some(),
            "color":shadow_color.and_then(color_value),
            "colorSpec":shadow_color.map(drawing_color_spec),
            "alpha":shadow_color.map(color_alpha).unwrap_or(1.0),
            "blur":shadow.and_then(|n| n.attribute("blurRad")).and_then(|v| v.parse::<f64>().ok()).map(|v| v / EMU_PER_POINT).unwrap_or(0.0),
            "distance":shadow.and_then(|n| n.attribute("dist")).and_then(|v| v.parse::<f64>().ok()).map(|v| v / EMU_PER_POINT).unwrap_or(0.0),
            "angle":shadow.and_then(|n| n.attribute("dir")).and_then(|v| v.parse::<f64>().ok()).map(|v| v / ANGLE_UNIT).unwrap_or(0.0),
        },
        "softEdge":soft_edge.and_then(|n| n.attribute("rad")).and_then(|v| v.parse::<f64>().ok()).map(|v| v / EMU_PER_POINT).unwrap_or(0.0),
    })
}

fn replace(mut xml: String, range: Range<usize>, value: &str) -> String {
    xml.replace_range(range, value);
    xml
}

fn apply_many(mut xml: String, mut patches: Vec<(Range<usize>, String)>) -> Result<String, String> {
    patches.sort_by(|a, b| {
        b.0.start
            .cmp(&a.0.start)
            .then_with(|| b.0.end.cmp(&a.0.end))
    });
    let mut last_start = xml.len() + 1;
    for (range, value) in patches {
        if range.end > xml.len() || range.end > last_start {
            return Err("overlapping DrawingML edits".to_string());
        }
        last_start = range.start;
        xml.replace_range(range, &value);
    }
    Ok(xml)
}

fn start_qname(fragment: &str) -> Option<&str> {
    let start = fragment.find('<')? + 1;
    let end = fragment[start..]
        .find(|c: char| c.is_ascii_whitespace() || matches!(c, '>' | '/'))?
        + start;
    Some(&fragment[start..end])
}

fn prefix_of(fragment: &str, fallback: &str) -> String {
    start_qname(fragment)
        .and_then(|q| q.split_once(':').map(|(p, _)| p.to_string()))
        .unwrap_or_else(|| fallback.to_string())
}

fn set_start_attribute(fragment: &str, name: &str, value: &str) -> String {
    let Some(tag_end) = fragment.find('>') else {
        return fragment.to_string();
    };
    let mut result = fragment.to_string();
    for quote in ['\"', '\''] {
        let needle = format!(" {name}={quote}");
        if let Some(start) = result[..tag_end].find(&needle) {
            let value_start = start + needle.len();
            if let Some(relative_end) = result[value_start..tag_end].find(quote) {
                result.replace_range(value_start..value_start + relative_end, value);
                return result;
            }
        }
    }
    let insert_at = if tag_end > 0 && result.as_bytes()[tag_end - 1] == b'/' {
        tag_end - 1
    } else {
        tag_end
    };
    result.insert_str(
        insert_at,
        &format!(" {name}=\"{}\"", xml_escape_attr(value)),
    );
    result
}

fn remove_start_attribute(fragment: &str, name: &str) -> String {
    let Some(tag_end) = fragment.find('>') else {
        return fragment.to_string();
    };
    let bytes = fragment.as_bytes();
    let name_bytes = name.as_bytes();
    let mut cursor = 1usize;
    while cursor + name_bytes.len() < tag_end {
        let Some(relative) = fragment[cursor..tag_end].find(name) else {
            break;
        };
        let name_start = cursor + relative;
        let name_end = name_start + name_bytes.len();
        let before_ok = name_start > 0 && bytes[name_start - 1].is_ascii_whitespace();
        let mut after = name_end;
        while after < tag_end && bytes[after].is_ascii_whitespace() {
            after += 1;
        }
        if !before_ok || after >= tag_end || bytes[after] != b'=' {
            cursor = name_end;
            continue;
        }
        after += 1;
        while after < tag_end && bytes[after].is_ascii_whitespace() {
            after += 1;
        }
        if after >= tag_end || !matches!(bytes[after], b'\'' | b'"') {
            cursor = name_end;
            continue;
        }
        let quote = bytes[after];
        after += 1;
        while after < tag_end && bytes[after] != quote {
            after += 1;
        }
        if after >= tag_end {
            return fragment.to_string();
        }
        after += 1;
        let mut remove_start = name_start;
        while remove_start > 0 && bytes[remove_start - 1].is_ascii_whitespace() {
            remove_start -= 1;
        }
        let mut result = fragment.to_string();
        result.replace_range(remove_start..after, "");
        return result;
    }
    fragment.to_string()
}

fn append_to_fragment(fragment: &str, child_xml: &str) -> Result<String, String> {
    let mut result = fragment.to_string();
    let Some(qname) = start_qname(fragment).map(str::to_string) else {
        return Err("malformed XML element".to_string());
    };
    let Some(tag_end) = fragment.find('>') else {
        return Err("malformed XML start tag".to_string());
    };
    if tag_end > 0 && fragment.as_bytes()[tag_end - 1] == b'/' {
        result.replace_range(tag_end - 1..=tag_end, &format!(">{child_xml}</{qname}>"));
        return Ok(result);
    }
    let close = format!("</{qname}>");
    let Some(at) = result.rfind(&close) else {
        return Err("malformed XML closing tag".to_string());
    };
    result.insert_str(at, child_xml);
    Ok(result)
}

fn append_child_patch(
    xml: &str,
    parent: Node<'_, '_>,
    child_xml: &str,
) -> Result<(Range<usize>, String), String> {
    let range = parent.range();
    Ok((range.clone(), append_to_fragment(&xml[range], child_xml)?))
}

fn ordered_child_patch(
    xml: &str,
    parent: Node<'_, '_>,
    desired_order: usize,
    child_xml: &str,
) -> Result<(Range<usize>, String), String> {
    if let Some(next) = parent
        .children()
        .filter(|n| n.is_element())
        .find(|n| property_order(n.tag_name().name()) > desired_order)
    {
        return Ok((
            next.range().start..next.range().start,
            child_xml.to_string(),
        ));
    }
    append_child_patch(xml, parent, child_xml)
}

fn property_order(name: &str) -> usize {
    match name {
        "xfrm" => 0,
        "prstGeom" | "custGeom" => 1,
        "noFill" | "solidFill" | "gradFill" | "blipFill" | "pattFill" | "grpFill" => 2,
        "ln" => 3,
        "effectLst" | "effectDag" => 4,
        "scene3d" => 5,
        "sp3d" => 6,
        "extLst" => 7,
        _ => usize::MAX - 1,
    }
}

fn xml_escape_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn xml_escape_attr(value: &str) -> String {
    xml_escape_text(value)
        .replace('\"', "&quot;")
        .replace('\'', "&apos;")
}

fn text_element(fragment: &str, value: &str) -> Result<String, String> {
    let qname = start_qname(fragment)
        .ok_or("malformed DrawingML text element")?
        .to_string();
    let Some(open_end) = fragment.find('>') else {
        return Err("malformed DrawingML text element".to_string());
    };
    let mut start = fragment[..=open_end].to_string();
    if start.ends_with("/>") {
        start.truncate(start.len() - 2);
        start.push('>');
    }
    if value.starts_with(char::is_whitespace) || value.ends_with(char::is_whitespace) {
        start = set_start_attribute(&start, "xml:space", "preserve");
    }
    Ok(format!("{start}{}</{qname}>", xml_escape_text(value)))
}

fn distribute_text(value: &str, old_lengths: &[usize]) -> Vec<String> {
    if old_lengths.is_empty() {
        return Vec::new();
    }
    let chars: Vec<char> = value.chars().collect();
    let mut offset = 0usize;
    let mut result = Vec::with_capacity(old_lengths.len());
    for (index, old_len) in old_lengths.iter().enumerate() {
        let take = if index + 1 == old_lengths.len() {
            chars.len().saturating_sub(offset)
        } else {
            (*old_len).min(chars.len().saturating_sub(offset))
        };
        result.push(chars[offset..offset + take].iter().collect());
        offset += take;
    }
    result
}

fn apply_text(xml: String, new_text: &str) -> Result<String, String> {
    let doc = Document::parse(&xml).map_err(|e| e.to_string())?;
    let shape = find_target_shape(&doc).ok_or("shape disappeared while editing text")?;
    let shape_prefix = prefix_of(&xml[shape.range()], "xdr");
    let a_prefix = drawing_prefix(&xml, shape);
    let lines: Vec<&str> = new_text.split('\n').collect();
    let Some(body) = child(shape, "txBody") else {
        let paragraphs = lines
            .iter()
            .map(|line| minimal_paragraph(&a_prefix, line))
            .collect::<String>();
        let body_xml = format!(
            "<{shape_prefix}:txBody><{a_prefix}:bodyPr/><{a_prefix}:lstStyle/>{paragraphs}</{shape_prefix}:txBody>"
        );
        let patch = append_child_patch(&xml, shape, &body_xml)?;
        drop(doc);
        return apply_many(xml, vec![patch]);
    };

    let paragraphs: Vec<Node<'_, '_>> = body
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "p")
        .collect();
    let mut patches = Vec::new();
    for (index, paragraph) in paragraphs.iter().enumerate() {
        let line = lines.get(index).copied().unwrap_or("");
        let texts: Vec<Node<'_, '_>> = paragraph
            .descendants()
            .filter(|n| n.is_element() && n.tag_name().name() == "t")
            .collect();
        if texts.is_empty() {
            let run = format!(
                "<{a_prefix}:r><{a_prefix}:t>{}</{a_prefix}:t></{a_prefix}:r>",
                xml_escape_text(line)
            );
            patches.push(append_child_patch(&xml, *paragraph, &run)?);
            continue;
        }
        let lengths: Vec<usize> = texts
            .iter()
            .map(|n| n.text().unwrap_or("").chars().count())
            .collect();
        let pieces = distribute_text(line, &lengths);
        for (text_node, piece) in texts.into_iter().zip(pieces) {
            patches.push((
                text_node.range(),
                text_element(&xml[text_node.range()], &piece)?,
            ));
        }
    }
    if lines.len() > paragraphs.len() {
        let extra = lines[paragraphs.len()..]
            .iter()
            .map(|line| minimal_paragraph(&a_prefix, line))
            .collect::<String>();
        patches.push(append_child_patch(&xml, body, &extra)?);
    }
    drop(doc);
    apply_many(xml, patches)
}

fn minimal_paragraph(prefix: &str, value: &str) -> String {
    let preserve = if value.starts_with(char::is_whitespace) || value.ends_with(char::is_whitespace)
    {
        " xml:space=\"preserve\""
    } else {
        ""
    };
    format!(
        "<{prefix}:p><{prefix}:r><{prefix}:t{preserve}>{}</{prefix}:t></{prefix}:r></{prefix}:p>",
        xml_escape_text(value)
    )
}

fn field_changed(edit: &serde_json::Map<String, Value>, current: &Value, field: &str) -> bool {
    edit.get(field)
        .map(|value| value != &current[field])
        .unwrap_or(false)
}

fn optional_string<'a>(value: &'a Value, field: &str) -> Result<Option<&'a str>, String> {
    if value.is_null() {
        Ok(None)
    } else {
        value
            .as_str()
            .map(Some)
            .ok_or_else(|| format!("{field} must be a string or null"))
    }
}

fn optional_bool(value: &Value, field: &str) -> Result<Option<bool>, String> {
    if value.is_null() {
        Ok(None)
    } else {
        value
            .as_bool()
            .map(Some)
            .ok_or_else(|| format!("{field} must be boolean or null"))
    }
}

fn optional_number(value: &Value, field: &str) -> Result<Option<f64>, String> {
    if value.is_null() {
        Ok(None)
    } else {
        value
            .as_f64()
            .filter(|value| value.is_finite())
            .map(Some)
            .ok_or_else(|| format!("{field} must be a finite number or null"))
    }
}

fn underline_value(value: &Value) -> Result<Option<String>, String> {
    if value.is_null() {
        return Ok(None);
    }
    if let Some(value) = value.as_bool() {
        return Ok(Some(if value { "sng" } else { "none" }.to_string()));
    }
    let value = value
        .as_str()
        .ok_or("text run underline must be a string, boolean or null")?;
    if !valid_token(value) {
        return Err("invalid text run underline".to_string());
    }
    Ok(Some(value.to_string()))
}

fn patched_text_color_fragment(
    xml: &str,
    color: Node<'_, '_>,
    color_edit: Option<Option<&str>>,
    alpha_edit: Option<Option<f64>>,
) -> Result<String, String> {
    patch_existing_color_fragment(xml, color, color_edit.flatten(), alpha_edit)
}

fn patch_existing_color_fragment(
    xml: &str,
    color: Node<'_, '_>,
    new_color: Option<&str>,
    alpha_edit: Option<Option<f64>>,
) -> Result<String, String> {
    let range = color.range();
    let mut fragment = xml[range.clone()].to_string();
    let alpha_node = color
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == "alpha");
    let mut patches = Vec::new();
    if new_color.is_some() {
        for transform in color.children().filter(|node| {
            node.is_element() && is_non_alpha_color_transform(node.tag_name().name())
        }) {
            patches.push((
                transform.range().start - range.start..transform.range().end - range.start,
                String::new(),
            ));
        }
    }
    if let Some(alpha) = alpha_edit {
        for transform in color.children().filter(|node| {
            node.is_element() && matches!(node.tag_name().name(), "alpha" | "alphaOff" | "alphaMod")
        }) {
            let replacement = if transform.tag_name().name() == "alpha" {
                let prefix = prefix_of(&xml[transform.range()], "a");
                alpha
                    .map(|value| format!("<{prefix}:alpha val=\"{}\"/>", alpha_ooxml(value)))
                    .unwrap_or_default()
            } else {
                String::new()
            };
            patches.push((
                transform.range().start - range.start..transform.range().end - range.start,
                replacement,
            ));
        }
    }
    fragment = apply_many(fragment, patches)?;
    if let (Some(Some(alpha)), None) = (alpha_edit, alpha_node) {
        let prefix = prefix_of(&fragment, "a");
        fragment = append_to_fragment(
            &fragment,
            &format!("<{prefix}:alpha val=\"{}\"/>", alpha_ooxml(alpha)),
        )?;
    }
    if let Some(new_color) = new_color {
        let (kind, value) = color_kind_value(new_color)?;
        fragment = replace_qname(&fragment, kind)?;
        // Colour-choice elements do not share an attribute schema.  In
        // particular, scrgbClr uses r/g/b, hslClr uses hue/sat/lum, and
        // sysClr may carry lastClr.  Leaving those attributes behind after
        // renaming the node to srgbClr/schemeClr makes the DrawingML invalid
        // and can make Excel repair the drawing part on open.
        for incompatible in ["r", "g", "b", "hue", "sat", "lum", "lastClr"] {
            fragment = remove_start_attribute(&fragment, incompatible);
        }
        fragment = set_start_attribute(&fragment, "val", &value);
    }
    Ok(fragment)
}

fn text_font_node<'a, 'input>(properties: Node<'a, 'input>) -> Option<Node<'a, 'input>> {
    properties.children().find(|node| {
        node.is_element() && matches!(node.tag_name().name(), "latin" | "ea" | "cs" | "sym")
    })
}

fn make_text_run_properties(
    prefix: &str,
    edit: &serde_json::Map<String, Value>,
) -> Result<Option<String>, String> {
    let mut attrs = String::new();
    if let Some(value) = edit.get("size") {
        if let Some(value) = optional_number(value, "text run size")? {
            if value < 0.0 {
                return Err("text run size cannot be negative".to_string());
            }
            attrs.push_str(&format!(" sz=\"{}\"", round_i64(value * 100.0)));
        }
    }
    for (field, attribute) in [("bold", "b"), ("italic", "i")] {
        if let Some(value) = edit.get(field) {
            if let Some(value) = optional_bool(value, &format!("text run {field}"))? {
                attrs.push_str(&format!(" {attribute}=\"{}\"", if value { 1 } else { 0 }));
            }
        }
    }
    if let Some(value) = edit.get("underline") {
        if let Some(value) = underline_value(value)? {
            attrs.push_str(&format!(" u=\"{}\"", xml_escape_attr(&value)));
        }
    }
    let color = edit
        .get("color")
        .map(|value| optional_string(value, "text run color"))
        .transpose()?
        .flatten();
    let alpha = edit
        .get("alpha")
        .map(|value| optional_number(value, "text run alpha"))
        .transpose()?
        .flatten();
    let mut children = String::new();
    if color.is_some() || alpha.is_some() {
        children.push_str(&format!(
            "<{prefix}:solidFill>{}</{prefix}:solidFill>",
            make_color(prefix, color.unwrap_or("#000000"), alpha)?
        ));
    }
    if let Some(value) = edit.get("font") {
        if let Some(value) = optional_string(value, "text run font")? {
            children.push_str(&format!(
                "<{prefix}:latin typeface=\"{}\"/>",
                xml_escape_attr(value)
            ));
        }
    }
    if attrs.is_empty() && children.is_empty() {
        return Ok(None);
    }
    Ok(Some(format!(
        "<{prefix}:rPr{attrs}>{children}</{prefix}:rPr>"
    )))
}

fn patch_text_run_properties(
    xml: &str,
    properties: Node<'_, '_>,
    edit: &serde_json::Map<String, Value>,
    current: &Value,
    prefix: &str,
) -> Result<String, String> {
    let range = properties.range();
    let mut fragment = xml[range.clone()].to_string();
    let mut patches: Vec<(Range<usize>, String)> = Vec::new();
    let mut tail = String::new();

    if field_changed(edit, current, "font") {
        let desired = optional_string(&edit["font"], "text run font")?;
        if let Some(font) = text_font_node(properties) {
            let replacement = if let Some(desired) = desired {
                set_start_attribute(&xml[font.range()], "typeface", desired)
            } else {
                remove_start_attribute(&xml[font.range()], "typeface")
            };
            patches.push((
                font.range().start - range.start..font.range().end - range.start,
                replacement,
            ));
        } else if let Some(desired) = desired {
            tail.push_str(&format!(
                "<{prefix}:latin typeface=\"{}\"/>",
                xml_escape_attr(desired)
            ));
        }
    }

    let color_changed = field_changed(edit, current, "color");
    let alpha_changed = field_changed(edit, current, "alpha");
    if color_changed || alpha_changed {
        let desired_color = if color_changed {
            Some(optional_string(&edit["color"], "text run color")?)
        } else {
            None
        };
        let desired_alpha = if alpha_changed {
            Some(optional_number(&edit["alpha"], "text run alpha")?)
        } else {
            None
        };
        let fill = fill_child(properties);
        if color_changed && desired_color == Some(None) {
            if let Some(fill) = fill {
                patches.push((
                    fill.range().start - range.start..fill.range().end - range.start,
                    String::new(),
                ));
            }
        } else if let Some(solid) = fill.filter(|node| node.tag_name().name() == "solidFill") {
            if let Some(color) = color_node(solid) {
                patches.push((
                    color.range().start - range.start..color.range().end - range.start,
                    patched_text_color_fragment(xml, color, desired_color, desired_alpha)?,
                ));
            } else {
                let color = desired_color
                    .flatten()
                    .or_else(|| current["color"].as_str())
                    .unwrap_or("#000000");
                patches.push(append_child_patch(
                    xml,
                    solid,
                    &make_color(prefix, color, desired_alpha.flatten())?,
                )?);
                if let Some(last) = patches.last_mut() {
                    last.0 = last.0.start - range.start..last.0.end - range.start;
                }
            }
        } else {
            let color = desired_color
                .flatten()
                .or_else(|| current["color"].as_str())
                .unwrap_or("#000000");
            let replacement = format!(
                "<{prefix}:solidFill>{}</{prefix}:solidFill>",
                make_color(prefix, color, desired_alpha.flatten())?
            );
            if let Some(fill) = fill {
                patches.push((
                    fill.range().start - range.start..fill.range().end - range.start,
                    replacement,
                ));
            } else if let Some(font) = text_font_node(properties) {
                let at = font.range().start - range.start;
                patches.push((at..at, replacement));
            } else {
                tail.insert_str(0, &replacement);
            }
        }
    }

    if !tail.is_empty() {
        // Office commonly serializes direct run properties as a self-closing
        // `<a:rPr .../>`.  Font and colour edits need child elements, so expand
        // that form before applying any child-range patches.
        fragment = append_to_fragment(&fragment, &tail)?;
    }
    patches.sort_by(|a, b| {
        b.0.start
            .cmp(&a.0.start)
            .then_with(|| b.0.end.cmp(&a.0.end))
    });
    for (patch, replacement) in patches {
        fragment.replace_range(patch, &replacement);
    }

    if field_changed(edit, current, "size") {
        fragment = if let Some(size) = optional_number(&edit["size"], "text run size")? {
            if size < 0.0 {
                return Err("text run size cannot be negative".to_string());
            }
            set_start_attribute(&fragment, "sz", &round_i64(size * 100.0).to_string())
        } else {
            remove_start_attribute(&fragment, "sz")
        };
    }
    for (field, attribute) in [("bold", "b"), ("italic", "i")] {
        if field_changed(edit, current, field) {
            fragment =
                if let Some(value) = optional_bool(&edit[field], &format!("text run {field}"))? {
                    set_start_attribute(&fragment, attribute, if value { "1" } else { "0" })
                } else {
                    remove_start_attribute(&fragment, attribute)
                };
        }
    }
    if field_changed(edit, current, "underline") {
        fragment = if let Some(value) = underline_value(&edit["underline"])? {
            set_start_attribute(&fragment, "u", &value)
        } else {
            remove_start_attribute(&fragment, "u")
        };
    }
    Ok(fragment)
}

fn make_structured_run(
    prefix: &str,
    value: &serde_json::Map<String, Value>,
) -> Result<String, String> {
    let text = value
        .get("text")
        .and_then(Value::as_str)
        .ok_or("text run text must be a string")?;
    let properties = make_text_run_properties(prefix, value)?.unwrap_or_default();
    let preserve = if text.starts_with(char::is_whitespace) || text.ends_with(char::is_whitespace) {
        " xml:space=\"preserve\""
    } else {
        ""
    };
    Ok(format!(
        "<{prefix}:r>{properties}<{prefix}:t{preserve}>{}</{prefix}:t></{prefix}:r>",
        xml_escape_text(text)
    ))
}

fn patch_structured_run(
    xml: &str,
    run: Node<'_, '_>,
    edit: &serde_json::Map<String, Value>,
    prefix: &str,
) -> Result<String, String> {
    let current = text_run_model(run);
    let range = run.range();
    let mut fragment = xml[range.clone()].to_string();
    let mut patches: Vec<(Range<usize>, String)> = Vec::new();
    if field_changed(edit, &current, "text") {
        let text = edit["text"]
            .as_str()
            .ok_or("text run text must be a string")?;
        if let Some(node) = child(run, "t") {
            patches.push((
                node.range().start - range.start..node.range().end - range.start,
                text_element(&xml[node.range()], text)?,
            ));
        } else {
            let preserve =
                if text.starts_with(char::is_whitespace) || text.ends_with(char::is_whitespace) {
                    " xml:space=\"preserve\""
                } else {
                    ""
                };
            let close = fragment.rfind("</").ok_or("malformed DrawingML text run")?;
            patches.push((
                close..close,
                format!(
                    "<{prefix}:t{preserve}>{}</{prefix}:t>",
                    xml_escape_text(text)
                ),
            ));
        }
    }
    let style_changed = [
        "font",
        "size",
        "bold",
        "italic",
        "underline",
        "color",
        "alpha",
    ]
    .iter()
    .any(|field| field_changed(edit, &current, field));
    if style_changed {
        if let Some(properties) = run_properties(run) {
            patches.push((
                properties.range().start - range.start..properties.range().end - range.start,
                patch_text_run_properties(xml, properties, edit, &current, prefix)?,
            ));
        } else if let Some(properties) = make_text_run_properties(prefix, edit)? {
            let at = run
                .children()
                .find(|node| node.is_element())
                .map(|node| node.range().start - range.start)
                .unwrap_or_else(|| fragment.rfind("</").unwrap_or(fragment.len()));
            patches.push((at..at, properties));
        }
    }
    patches.sort_by(|a, b| {
        b.0.start
            .cmp(&a.0.start)
            .then_with(|| b.0.end.cmp(&a.0.end))
    });
    for (patch, replacement) in patches {
        fragment.replace_range(patch, &replacement);
    }
    Ok(fragment)
}

fn paragraph_run_values<'a>(
    paragraph: &'a serde_json::Map<String, Value>,
) -> Result<&'a Vec<Value>, String> {
    paragraph
        .get("runs")
        .and_then(Value::as_array)
        .ok_or_else(|| "text paragraph runs must be an array".to_string())
}

fn text_run_semantically_matches(
    requested: &serde_json::Map<String, Value>,
    current: &Value,
) -> bool {
    let mut compared = 0usize;
    for field in [
        "kind",
        "text",
        "font",
        "fontScript",
        "size",
        "bold",
        "italic",
        "underline",
        "color",
        "alpha",
    ] {
        let Some(value) = requested.get(field) else {
            continue;
        };
        compared += 1;
        if current.get(field) != Some(value) {
            return false;
        }
    }
    compared > 0
}

fn text_paragraph_semantically_matches(
    requested: &serde_json::Map<String, Value>,
    current: &Value,
) -> bool {
    let mut compared = 0usize;
    if let Some(text) = requested.get("text") {
        compared += 1;
        if current.get("text") != Some(text) {
            return false;
        }
    }
    if let Some(runs) = requested.get("runs") {
        compared += 1;
        let Some(requested_runs) = runs.as_array() else {
            return false;
        };
        let Some(current_runs) = current.get("runs").and_then(Value::as_array) else {
            return false;
        };
        if requested_runs.len() != current_runs.len()
            || !requested_runs
                .iter()
                .zip(current_runs)
                .all(|(requested, current)| {
                    requested
                        .as_object()
                        .is_some_and(|requested| text_run_semantically_matches(requested, current))
                })
        {
            return false;
        }
    }
    compared > 0
}

fn match_text_source_indices(
    requested: &[&serde_json::Map<String, Value>],
    existing: &[Value],
    label: &str,
    semantic_match: fn(&serde_json::Map<String, Value>, &Value) -> bool,
) -> Result<Vec<Option<usize>>, String> {
    let has_explicit_identity = requested
        .iter()
        .any(|item| item.contains_key("sourceIndex"));
    let mut used = vec![false; existing.len()];
    let mut assignments = Vec::with_capacity(requested.len());
    for (requested_index, item) in requested.iter().enumerate() {
        let explicit_index = item
            .get("sourceIndex")
            .map(|value| {
                value
                    .as_u64()
                    .and_then(|value| usize::try_from(value).ok())
                    .ok_or_else(|| format!("{label}.sourceIndex must be a non-negative integer"))
            })
            .transpose()?;
        if let Some(index) = explicit_index {
            if index >= existing.len() {
                return Err(format!("{label}.sourceIndex {index} is out of range"));
            }
            if used[index] {
                return Err(format!("duplicate {label}.sourceIndex {index}"));
            }
        }
        let matched = if let Some(index) = explicit_index {
            Some(index)
        } else if has_explicit_identity {
            // In a modern payload every existing item has an identity.  An
            // identity-less item is therefore new and must not inherit XML
            // from a deleted source slot.
            None
        } else {
            existing
                .iter()
                .enumerate()
                .filter(|(index, _)| !used[*index])
                .find(|(_, current)| semantic_match(item, current))
                .map(|(index, _)| index)
                .or_else(|| {
                    (!used.get(requested_index).copied().unwrap_or(true))
                        .then_some(requested_index)
                        .or_else(|| used.iter().position(|item| !*item))
                })
        };
        if let Some(index) = matched {
            used[index] = true;
        }
        assignments.push(matched);
    }
    Ok(assignments)
}

fn make_structured_paragraph(
    prefix: &str,
    paragraph: &serde_json::Map<String, Value>,
) -> Result<String, String> {
    let runs = paragraph_run_values(paragraph)?
        .iter()
        .map(|run| {
            run.as_object()
                .ok_or_else(|| "text run must be an object".to_string())
                .and_then(|run| make_structured_run(prefix, run))
        })
        .collect::<Result<String, String>>()?;
    Ok(format!("<{prefix}:p>{runs}</{prefix}:p>"))
}

fn patch_structured_paragraph(
    xml: &str,
    paragraph: Node<'_, '_>,
    edit: &serde_json::Map<String, Value>,
    prefix: &str,
) -> Result<String, String> {
    let requested = paragraph_run_values(edit)?;
    let requested: Vec<&serde_json::Map<String, Value>> = requested
        .iter()
        .map(|run| run.as_object().ok_or("text run must be an object"))
        .collect::<Result<_, _>>()?;
    let existing = paragraph_runs(paragraph);
    let existing_models: Vec<Value> = existing
        .iter()
        .enumerate()
        .map(|(index, run)| text_run_model_with_source(*run, index))
        .collect();
    let assignments = match_text_source_indices(
        &requested,
        &existing_models,
        "text run",
        text_run_semantically_matches,
    )?;
    let replacements = requested
        .iter()
        .zip(&assignments)
        .map(|(edit, source)| {
            source
                .map(|index| patch_structured_run(xml, existing[index], edit, prefix))
                .unwrap_or_else(|| make_structured_run(prefix, edit))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let range = paragraph.range();
    let mut fragment = xml[range.clone()].to_string();
    let mut patches: Vec<(Range<usize>, String)> = Vec::new();
    let identity_order = replacements.len() == existing.len()
        && assignments
            .iter()
            .enumerate()
            .all(|(index, source)| *source == Some(index));
    if identity_order {
        for (run, replacement) in existing.iter().zip(&replacements) {
            patches.push((
                run.range().start - range.start..run.range().end - range.start,
                replacement.clone(),
            ));
        }
    } else if !existing.is_empty() {
        let replacement = replacements.concat();
        for (index, run) in existing.iter().enumerate() {
            patches.push((
                run.range().start - range.start..run.range().end - range.start,
                if index == 0 {
                    replacement.clone()
                } else {
                    String::new()
                },
            ));
        }
    } else if !replacements.is_empty() {
        let insertion = paragraph
            .children()
            .find(|node| node.is_element() && node.tag_name().name() == "endParaRPr")
            .and_then(|node| fragment.find(&xml[node.range()]))
            .unwrap_or(fragment.rfind("</").ok_or("malformed text paragraph")?);
        fragment.insert_str(insertion, &replacements.concat());
    }
    patches.sort_by(|a, b| b.0.start.cmp(&a.0.start));
    for (patch, replacement) in patches {
        fragment.replace_range(patch, &replacement);
    }
    Ok(fragment)
}

fn apply_text_structure(xml: String, paragraphs: &Value) -> Result<String, String> {
    let requested = paragraphs
        .as_array()
        .ok_or("shape paragraphs must be an array")?;
    if requested.is_empty() {
        return Err("shape paragraphs must contain at least one paragraph".to_string());
    }
    let requested: Vec<&serde_json::Map<String, Value>> = requested
        .iter()
        .map(|paragraph| {
            paragraph
                .as_object()
                .ok_or("text paragraph must be an object")
        })
        .collect::<Result<_, _>>()?;
    let doc = Document::parse(&xml).map_err(|error| error.to_string())?;
    let shape = find_target_shape(&doc).ok_or("shape disappeared while editing text")?;
    let shape_prefix = prefix_of(&xml[shape.range()], "xdr");
    let prefix = drawing_prefix(&xml, shape);
    let Some(body) = child(shape, "txBody") else {
        let paragraphs = requested
            .iter()
            .map(|paragraph| make_structured_paragraph(&prefix, paragraph))
            .collect::<Result<String, String>>()?;
        let body_xml = format!(
            "<{shape_prefix}:txBody><{prefix}:bodyPr/><{prefix}:lstStyle/>{paragraphs}</{shape_prefix}:txBody>"
        );
        let patch = append_child_patch(&xml, shape, &body_xml)?;
        drop(doc);
        return apply_many(xml, vec![patch]);
    };

    let existing: Vec<Node<'_, '_>> = body
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "p")
        .collect();
    let existing_models: Vec<Value> = existing
        .iter()
        .enumerate()
        .map(|(index, paragraph)| text_paragraph_model(*paragraph, index))
        .collect();
    let assignments = match_text_source_indices(
        &requested,
        &existing_models,
        "text paragraph",
        text_paragraph_semantically_matches,
    )?;
    let replacements = requested
        .iter()
        .zip(&assignments)
        .map(|(edit, source)| {
            source
                .map(|index| patch_structured_paragraph(&xml, existing[index], edit, &prefix))
                .unwrap_or_else(|| make_structured_paragraph(&prefix, edit))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let body_range = body.range();
    let mut fragment = xml[body_range.clone()].to_string();
    let mut patches: Vec<(Range<usize>, String)> = Vec::new();
    let identity_order = replacements.len() == existing.len()
        && assignments
            .iter()
            .enumerate()
            .all(|(index, source)| *source == Some(index));
    if identity_order {
        for (paragraph, replacement) in existing.iter().zip(&replacements) {
            patches.push((
                paragraph.range().start - body_range.start
                    ..paragraph.range().end - body_range.start,
                replacement.clone(),
            ));
        }
    } else if !existing.is_empty() {
        let replacement = replacements.concat();
        for (index, paragraph) in existing.iter().enumerate() {
            patches.push((
                paragraph.range().start - body_range.start
                    ..paragraph.range().end - body_range.start,
                if index == 0 {
                    replacement.clone()
                } else {
                    String::new()
                },
            ));
        }
    } else if !replacements.is_empty() {
        let following = body
            .children()
            .filter(|node| node.is_element())
            .find(|node| !matches!(node.tag_name().name(), "bodyPr" | "lstStyle" | "p"));
        let insertion = following
            .and_then(|node| fragment.find(&xml[node.range()]))
            .unwrap_or(fragment.rfind("</").ok_or("malformed shape text body")?);
        fragment.insert_str(insertion, &replacements.concat());
    }
    patches.sort_by(|a, b| b.0.start.cmp(&a.0.start));
    for (patch, replacement) in patches {
        fragment.replace_range(patch, &replacement);
    }
    let range = body.range();
    drop(doc);
    Ok(replace(xml, range, &fragment))
}

fn drawing_prefix(xml: &str, shape: Node<'_, '_>) -> String {
    shape
        .descendants()
        .find(|n| n.is_element() && n.tag_name().namespace() == Some(DRAWING_NS))
        .map(|n| prefix_of(&xml[n.range()], "a"))
        .unwrap_or_else(|| "a".to_string())
}

fn json_number(value: &Value, field: &str) -> Result<f64, String> {
    value
        .as_f64()
        .ok_or_else(|| format!("{field} must be a number"))
}

fn apply_transform(xml: String, edit: &Value) -> Result<String, String> {
    let doc = Document::parse(&xml).map_err(|e| e.to_string())?;
    let shape = find_target_shape(&doc).ok_or("shape disappeared while editing transform")?;
    let properties = shape_properties(shape).ok_or("shape has no properties")?;
    let prefix = drawing_prefix(&xml, shape);
    if let Some(xfrm) = child(properties, "xfrm") {
        let mut fragment = xml[xfrm.range()].to_string();
        if let Some(value) = edit.get("rotation") {
            let degrees = json_number(value, "rotation")?;
            fragment = set_start_attribute(
                &fragment,
                "rot",
                &round_i64(degrees * ANGLE_UNIT).to_string(),
            );
        }
        for (json_name, xml_name) in [("flipH", "flipH"), ("flipV", "flipV")] {
            if let Some(value) = edit.get(json_name) {
                let enabled = value
                    .as_bool()
                    .ok_or_else(|| format!("{json_name} must be boolean"))?;
                fragment =
                    set_start_attribute(&fragment, xml_name, if enabled { "1" } else { "0" });
            }
        }
        let range = xfrm.range();
        drop(doc);
        return Ok(replace(xml, range, &fragment));
    }
    let mut attrs = String::new();
    if let Some(value) = edit.get("rotation") {
        attrs.push_str(&format!(
            " rot=\"{}\"",
            round_i64(json_number(value, "rotation")? * ANGLE_UNIT)
        ));
    }
    for name in ["flipH", "flipV"] {
        if let Some(value) = edit.get(name) {
            let enabled = value
                .as_bool()
                .ok_or_else(|| format!("{name} must be boolean"))?;
            attrs.push_str(&format!(" {name}=\"{}\"", if enabled { 1 } else { 0 }));
        }
    }
    let patch = ordered_child_patch(&xml, properties, 0, &format!("<{prefix}:xfrm{attrs}/>"))?;
    drop(doc);
    apply_many(xml, vec![patch])
}

fn geometry_name(value: &Value) -> Result<&str, String> {
    if let Some(value) = value.as_str() {
        return Ok(value);
    }
    value
        .get("preset")
        .and_then(Value::as_str)
        .ok_or_else(|| "geometry must be a preset string".to_string())
}

fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn apply_geometry(xml: String, geometry: &Value) -> Result<String, String> {
    let geometry = geometry_name(geometry)?;
    if geometry != "custom" && !valid_token(geometry) {
        return Err("invalid preset geometry".to_string());
    }
    let doc = Document::parse(&xml).map_err(|e| e.to_string())?;
    let shape = find_target_shape(&doc).ok_or("shape disappeared while editing geometry")?;
    let properties = shape_properties(shape).ok_or("shape has no properties")?;
    if properties.tag_name().name() == "grpSpPr" {
        return Err("an empty group shape has no editable geometry".to_string());
    }
    let prefix = drawing_prefix(&xml, shape);
    let current = child(properties, "prstGeom").or_else(|| child(properties, "custGeom"));
    if geometry == "custom" {
        if current.map(|n| n.tag_name().name()) == Some("custGeom") {
            return Ok(xml);
        }
        return Err("cannot synthesise custom geometry without path data".to_string());
    }
    if let Some(current) = current {
        let replacement = if current.tag_name().name() == "prstGeom" {
            set_start_attribute(&xml[current.range()], "prst", geometry)
        } else {
            format!(
                "<{prefix}:prstGeom prst=\"{}\"><{prefix}:avLst/></{prefix}:prstGeom>",
                xml_escape_attr(geometry)
            )
        };
        let range = current.range();
        drop(doc);
        return Ok(replace(xml, range, &replacement));
    }
    let replacement = format!(
        "<{prefix}:prstGeom prst=\"{}\"><{prefix}:avLst/></{prefix}:prstGeom>",
        xml_escape_attr(geometry)
    );
    let patch = ordered_child_patch(&xml, properties, 1, &replacement)?;
    drop(doc);
    apply_many(xml, vec![patch])
}

fn alpha_ooxml(value: f64) -> i64 {
    let fraction = if value > 1.0 { value / 100.0 } else { value };
    round_i64(fraction.clamp(0.0, 1.0) * 100_000.0)
}

fn position_ooxml(value: f64) -> i64 {
    let fraction = if value > 100.0 {
        value / 100_000.0
    } else if value > 1.0 {
        value / 100.0
    } else {
        value
    };
    round_i64(fraction.clamp(0.0, 1.0) * 100_000.0)
}

fn round_i64(value: f64) -> i64 {
    if value.is_finite() {
        value.round() as i64
    } else {
        0
    }
}

fn color_spec(value: Option<&Value>) -> Result<Option<String>, String> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_str()
            .map(|s| Some(s.to_string()))
            .ok_or_else(|| "color must be a string".to_string()),
    }
}

fn color_kind_value(color: &str) -> Result<(&'static str, String), String> {
    let clean = color.trim();
    let hex = clean.strip_prefix('#').unwrap_or(clean);
    if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Ok(("srgbClr", hex.to_ascii_uppercase()));
    }
    let scheme = clean.strip_prefix("scheme:").unwrap_or(clean);
    if valid_token(scheme) {
        return Ok(("schemeClr", scheme.to_string()));
    }
    Err("unsupported DrawingML color".to_string())
}

fn make_color(prefix: &str, color: &str, alpha: Option<f64>) -> Result<String, String> {
    let (kind, value) = color_kind_value(color)?;
    let alpha = alpha
        .map(|v| format!("<{prefix}:alpha val=\"{}\"/>", alpha_ooxml(v)))
        .unwrap_or_default();
    Ok(format!(
        "<{prefix}:{kind} val=\"{}\">{alpha}</{prefix}:{kind}>",
        xml_escape_attr(&value)
    ))
}

fn replace_qname(fragment: &str, new_local: &str) -> Result<String, String> {
    let old = start_qname(fragment)
        .ok_or("malformed color element")?
        .to_string();
    let prefix = old
        .split_once(':')
        .map(|(p, _)| format!("{p}:"))
        .unwrap_or_default();
    let new = format!("{prefix}{new_local}");
    let mut result = fragment.to_string();
    if let Some(start) = result.find(&format!("<{old}")) {
        result.replace_range(start + 1..start + 1 + old.len(), &new);
    }
    if let Some(start) = result.rfind(&format!("</{old}")) {
        result.replace_range(start + 2..start + 2 + old.len(), &new);
    }
    Ok(result)
}

fn patched_color_fragment(
    xml: &str,
    color: Node<'_, '_>,
    new_color: Option<&str>,
    new_alpha: Option<f64>,
) -> Result<String, String> {
    patch_existing_color_fragment(xml, color, new_color, new_alpha.map(Some))
}

fn make_solid_fill(prefix: &str, edit: &serde_json::Map<String, Value>) -> Result<String, String> {
    let color = color_spec(edit.get("color"))?.unwrap_or_else(|| "#000000".to_string());
    let alpha = edit
        .get("alpha")
        .map(|v| json_number(v, "fill.alpha"))
        .transpose()?;
    Ok(format!(
        "<{prefix}:solidFill>{}</{prefix}:solidFill>",
        make_color(prefix, &color, alpha)?
    ))
}

fn make_gradient_fill(
    prefix: &str,
    edit: &serde_json::Map<String, Value>,
) -> Result<String, String> {
    let stops = gradient_stops(prefix, edit)?;
    let mut attrs = String::new();
    match edit.get("rotWithShape") {
        Some(Value::Null) => {}
        Some(value) => attrs.push_str(&format!(
            " rotWithShape=\"{}\"",
            if value
                .as_bool()
                .ok_or("fill.rotWithShape must be boolean or null")?
            {
                1
            } else {
                0
            }
        )),
        None => attrs.push_str(" rotWithShape=\"1\""),
    }
    if let Some(value) = edit.get("flip") {
        if !value.is_null() {
            let flip = gradient_flip(value)?;
            attrs.push_str(&format!(" flip=\"{}\"", xml_escape_attr(flip)));
        }
    }
    let direction = match gradient_direction_target(edit)? {
        GradientDirectionTarget::Remove => String::new(),
        GradientDirectionTarget::Path => make_path_direction(prefix, edit)?,
        GradientDirectionTarget::Unchanged | GradientDirectionTarget::Linear => {
            make_linear_direction(prefix, edit)?
        }
    };
    let tile_rect = match edit.get("tileRect") {
        Some(value) => {
            make_relative_rect(prefix, "tileRect", value, "fill.tileRect")?.unwrap_or_default()
        }
        None => String::new(),
    };
    Ok(format!(
        "<{prefix}:gradFill{attrs}>{stops}{direction}{tile_rect}</{prefix}:gradFill>"
    ))
}

fn gradient_stops(prefix: &str, edit: &serde_json::Map<String, Value>) -> Result<String, String> {
    let fallback_color = color_spec(edit.get("color"))?.unwrap_or_else(|| "#000000".to_string());
    let fallback_alpha = edit
        .get("alpha")
        .map(|v| json_number(v, "fill.alpha"))
        .transpose()?;
    let stops = edit.get("stops").and_then(Value::as_array);
    let mut body = String::new();
    if let Some(stops) = stops {
        if stops.is_empty() {
            return Err("gradient needs at least one stop".to_string());
        }
        for stop in stops {
            let stop = stop.as_object().ok_or("gradient stop must be an object")?;
            let position = stop
                .get("position")
                .map(|v| json_number(v, "fill.stop.position"))
                .transpose()?
                .unwrap_or(0.0);
            let color = color_spec(stop.get("color"))?.unwrap_or_else(|| fallback_color.clone());
            let alpha = stop
                .get("alpha")
                .map(|v| json_number(v, "fill.stop.alpha"))
                .transpose()?
                .or(fallback_alpha);
            body.push_str(&format!(
                "<{prefix}:gs pos=\"{}\">{}</{prefix}:gs>",
                position_ooxml(position),
                make_color(prefix, &color, alpha)?
            ));
        }
    } else {
        body.push_str(&format!(
            "<{prefix}:gs pos=\"0\">{}</{prefix}:gs>",
            make_color(prefix, &fallback_color, fallback_alpha)?
        ));
        body.push_str(&format!(
            "<{prefix}:gs pos=\"100000\">{}</{prefix}:gs>",
            make_color(prefix, &fallback_color, fallback_alpha)?
        ));
    }
    Ok(format!("<{prefix}:gsLst>{body}</{prefix}:gsLst>"))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GradientDirectionTarget {
    Unchanged,
    Remove,
    Linear,
    Path,
}

fn gradient_direction_target(
    edit: &serde_json::Map<String, Value>,
) -> Result<GradientDirectionTarget, String> {
    if let Some(value) = edit.get("directionType") {
        if value.is_null() {
            return Ok(GradientDirectionTarget::Remove);
        }
        return match value.as_str() {
            Some("linear") => Ok(GradientDirectionTarget::Linear),
            Some("path") => Ok(GradientDirectionTarget::Path),
            _ => Err("fill.directionType must be linear, path or null".to_string()),
        };
    }
    if edit.contains_key("path") || edit.contains_key("pathType") || edit.contains_key("fillToRect")
    {
        return Ok(GradientDirectionTarget::Path);
    }
    if edit.contains_key("angle") || edit.contains_key("scaled") {
        return Ok(GradientDirectionTarget::Linear);
    }
    Ok(GradientDirectionTarget::Unchanged)
}

fn gradient_flip(value: &Value) -> Result<&str, String> {
    let value = value
        .as_str()
        .ok_or("fill.flip must be none, x, y, xy or null")?;
    if matches!(value, "none" | "x" | "y" | "xy") {
        Ok(value)
    } else {
        Err("fill.flip must be none, x, y, xy or null".to_string())
    }
}

fn path_type_edit<'a>(
    edit: &'a serde_json::Map<String, Value>,
) -> Result<Option<Option<&'a str>>, String> {
    let value = edit.get("path").or_else(|| edit.get("pathType"));
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(Some(None));
    }
    let value = value
        .as_str()
        .ok_or("fill.path must be circle, rect, shape or null")?;
    if !matches!(value, "circle" | "rect" | "shape") {
        return Err("fill.path must be circle, rect, shape or null".to_string());
    }
    Ok(Some(Some(value)))
}

fn relative_rect_ooxml(value: f64) -> i64 {
    let magnitude = value.abs();
    let fraction = if magnitude > 100.0 {
        value / 100_000.0
    } else if magnitude > 1.0 {
        value / 100.0
    } else {
        value
    };
    round_i64(fraction * 100_000.0)
}

fn patch_relative_rect(fragment: &str, value: &Value, field: &str) -> Result<String, String> {
    let edit = value
        .as_object()
        .ok_or_else(|| format!("{field} must be an object or null"))?;
    let mut result = fragment.to_string();
    for side in ["l", "t", "r", "b"] {
        let Some(value) = edit.get(side) else {
            continue;
        };
        if value.is_null() {
            result = remove_start_attribute(&result, side);
        } else {
            let number = json_number(value, &format!("{field}.{side}"))?;
            result = set_start_attribute(&result, side, &relative_rect_ooxml(number).to_string());
        }
    }
    Ok(result)
}

fn make_relative_rect(
    prefix: &str,
    local: &str,
    value: &Value,
    field: &str,
) -> Result<Option<String>, String> {
    if value.is_null() {
        return Ok(None);
    }
    let empty = format!("<{prefix}:{local}/>");
    Ok(Some(patch_relative_rect(&empty, value, field)?))
}

fn make_linear_direction(
    prefix: &str,
    edit: &serde_json::Map<String, Value>,
) -> Result<String, String> {
    let mut direction = format!("<{prefix}:lin/>");
    match edit.get("angle") {
        Some(Value::Null) => {}
        Some(value) => {
            direction = set_start_attribute(
                &direction,
                "ang",
                &round_i64(json_number(value, "fill.angle")? * ANGLE_UNIT).to_string(),
            );
        }
        None => direction = set_start_attribute(&direction, "ang", "0"),
    }
    match edit.get("scaled") {
        Some(Value::Null) => {}
        Some(value) => {
            let scaled = value
                .as_bool()
                .ok_or("fill.scaled must be boolean or null")?;
            direction = set_start_attribute(&direction, "scaled", if scaled { "1" } else { "0" });
        }
        None => direction = set_start_attribute(&direction, "scaled", "1"),
    }
    Ok(direction)
}

fn make_path_direction(
    prefix: &str,
    edit: &serde_json::Map<String, Value>,
) -> Result<String, String> {
    let path = path_type_edit(edit)?.flatten().unwrap_or("shape");
    let body = match edit.get("fillToRect") {
        Some(value) => {
            make_relative_rect(prefix, "fillToRect", value, "fill.fillToRect")?.unwrap_or_default()
        }
        None => String::new(),
    };
    Ok(format!(
        "<{prefix}:path path=\"{}\">{body}</{prefix}:path>",
        xml_escape_attr(path)
    ))
}

fn patch_linear_direction(
    fragment: &str,
    edit: &serde_json::Map<String, Value>,
) -> Result<String, String> {
    let mut result = fragment.to_string();
    if let Some(value) = edit.get("angle") {
        result = if value.is_null() {
            remove_start_attribute(&result, "ang")
        } else {
            set_start_attribute(
                &result,
                "ang",
                &round_i64(json_number(value, "fill.angle")? * ANGLE_UNIT).to_string(),
            )
        };
    }
    if let Some(value) = edit.get("scaled") {
        result = if value.is_null() {
            remove_start_attribute(&result, "scaled")
        } else {
            let scaled = value
                .as_bool()
                .ok_or("fill.scaled must be boolean or null")?;
            set_start_attribute(&result, "scaled", if scaled { "1" } else { "0" })
        };
    }
    Ok(result)
}

fn patch_path_direction(
    xml: &str,
    path: Node<'_, '_>,
    edit: &serde_json::Map<String, Value>,
    prefix: &str,
) -> Result<String, String> {
    let range = path.range();
    let mut fragment = xml[range.clone()].to_string();
    if let Some(value) = edit.get("fillToRect") {
        if let Some(current) = child(path, "fillToRect") {
            let relative = current.range().start - range.start..current.range().end - range.start;
            let replacement = if value.is_null() {
                String::new()
            } else {
                patch_relative_rect(&xml[current.range()], value, "fill.fillToRect")?
            };
            fragment.replace_range(relative, &replacement);
        } else if let Some(replacement) =
            make_relative_rect(prefix, "fillToRect", value, "fill.fillToRect")?
        {
            fragment = append_to_fragment(&fragment, &replacement)?;
        }
    }
    if let Some(path_type) = path_type_edit(edit)? {
        fragment = if let Some(path_type) = path_type {
            set_start_attribute(&fragment, "path", path_type)
        } else {
            remove_start_attribute(&fragment, "path")
        };
    }
    Ok(fragment)
}

fn patch_gradient_stop(
    xml: &str,
    stop: Node<'_, '_>,
    edit: &serde_json::Map<String, Value>,
    prefix: &str,
    fallback_color: &str,
    fallback_alpha: Option<f64>,
) -> Result<String, String> {
    let range = stop.range();
    let mut fragment = xml[range.clone()].to_string();
    let new_color = color_spec(edit.get("color"))?;
    let new_alpha = edit
        .get("alpha")
        .map(|value| json_number(value, "fill.stop.alpha"))
        .transpose()?;
    if edit.contains_key("color") || edit.contains_key("alpha") {
        if let Some(color) = color_node(stop) {
            let relative = color.range().start - range.start..color.range().end - range.start;
            fragment.replace_range(
                relative,
                &patched_color_fragment(
                    xml,
                    color,
                    new_color.as_deref(),
                    new_alpha.or(fallback_alpha),
                )?,
            );
        } else {
            let color = new_color.as_deref().unwrap_or(fallback_color);
            fragment = append_to_fragment(
                &fragment,
                &make_color(prefix, color, new_alpha.or(fallback_alpha))?,
            )?;
        }
    }
    if let Some(position) = edit.get("position") {
        fragment = if position.is_null() {
            remove_start_attribute(&fragment, "pos")
        } else {
            set_start_attribute(
                &fragment,
                "pos",
                &position_ooxml(json_number(position, "fill.stop.position")?).to_string(),
            )
        };
    }
    Ok(fragment)
}

fn patch_gradient_stop_list(
    xml: &str,
    list: Node<'_, '_>,
    edit: &serde_json::Map<String, Value>,
    prefix: &str,
) -> Result<String, String> {
    let requested = edit
        .get("stops")
        .and_then(Value::as_array)
        .ok_or("fill.stops must be an array")?;
    if requested.is_empty() {
        return Err("gradient needs at least one stop".to_string());
    }
    let requested: Vec<&serde_json::Map<String, Value>> = requested
        .iter()
        .map(|stop| stop.as_object().ok_or("gradient stop must be an object"))
        .collect::<Result<_, _>>()?;
    let existing: Vec<Node<'_, '_>> = list
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "gs")
        .collect();
    let fallback_color = color_spec(edit.get("color"))?.unwrap_or_else(|| "#000000".to_string());
    let fallback_alpha = edit
        .get("alpha")
        .map(|value| json_number(value, "fill.alpha"))
        .transpose()?;
    let list_range = list.range();
    let mut fragment = xml[list_range.clone()].to_string();
    let existing_models: Vec<Value> = existing
        .iter()
        .enumerate()
        .map(|(index, stop)| gradient_stop_model(*stop, index))
        .collect();
    let has_explicit_identity = requested
        .iter()
        .any(|stop| stop.contains_key("sourceIndex"));
    let mut used = vec![false; existing.len()];
    let mut replacements = Vec::with_capacity(requested.len());

    for (requested_index, requested_stop) in requested.iter().enumerate() {
        let explicit_index = requested_stop
            .get("sourceIndex")
            .map(|value| {
                value
                    .as_u64()
                    .and_then(|value| usize::try_from(value).ok())
                    .ok_or_else(|| {
                        "fill.stop.sourceIndex must be a non-negative integer".to_string()
                    })
            })
            .transpose()?;
        if let Some(index) = explicit_index {
            if index >= existing.len() {
                return Err(format!("fill.stop.sourceIndex {index} is out of range"));
            }
            if used[index] {
                return Err(format!("duplicate fill.stop.sourceIndex {index}"));
            }
        }

        // Modern editor payloads carry sourceIndex.  A stop without one in
        // such a payload is newly inserted and must not inherit opaque XML
        // from an unrelated old slot.  For legacy/API payloads with no
        // identities, first perform semantic matching so pure reorders still
        // move the original fragments, then fall back to the historical
        // positional behaviour for genuinely edited stops.
        let matched = if let Some(index) = explicit_index {
            Some(index)
        } else if has_explicit_identity {
            None
        } else {
            let semantic_match = |model: &Value| -> bool {
                let mut compared = 0;
                for field in ["position", "color", "alpha"] {
                    let Some(requested_value) = requested_stop.get(field) else {
                        continue;
                    };
                    compared += 1;
                    let equal = match field {
                        "position" | "alpha" => requested_value
                            .as_f64()
                            .zip(model.get(field).and_then(Value::as_f64))
                            .is_some_and(|(left, right)| (left - right).abs() < 0.000_000_1),
                        _ => model.get(field) == Some(requested_value),
                    };
                    if !equal {
                        return false;
                    }
                }
                compared > 0
            };
            let best = existing_models
                .iter()
                .enumerate()
                .filter(|(index, _)| !used[*index])
                .find(|(_, model)| semantic_match(model))
                .map(|(index, _)| index);
            best.or_else(|| {
                (!used.get(requested_index).copied().unwrap_or(true))
                    .then_some(requested_index)
                    .or_else(|| used.iter().position(|item| !*item))
            })
        };

        if let Some(index) = matched {
            used[index] = true;
            let current = &existing_models[index];
            let mut differential = serde_json::Map::new();
            for field in ["position", "color", "alpha"] {
                let Some(value) = requested_stop.get(field) else {
                    continue;
                };
                let equal = match field {
                    "position" | "alpha" => value
                        .as_f64()
                        .zip(current.get(field).and_then(Value::as_f64))
                        .is_some_and(|(left, right)| (left - right).abs() < 0.000_000_1),
                    _ => current.get(field) == Some(value),
                };
                if !equal {
                    differential.insert(field.to_string(), value.clone());
                }
            }
            replacements.push(patch_gradient_stop(
                xml,
                existing[index],
                &differential,
                prefix,
                &fallback_color,
                fallback_alpha,
            )?);
        } else {
            let position = requested_stop
                .get("position")
                .map(|value| json_number(value, "fill.stop.position"))
                .transpose()?
                .unwrap_or(0.0);
            let color =
                color_spec(requested_stop.get("color"))?.unwrap_or_else(|| fallback_color.clone());
            let alpha = requested_stop
                .get("alpha")
                .map(|value| json_number(value, "fill.stop.alpha"))
                .transpose()?
                .or(fallback_alpha);
            replacements.push(format!(
                "<{prefix}:gs pos=\"{}\">{}</{prefix}:gs>",
                position_ooxml(position),
                make_color(prefix, &color, alpha)?
            ));
        }
    }

    let replacement = replacements.concat();
    if !existing.is_empty() {
        let mut patches = Vec::with_capacity(existing.len());
        for (index, stop) in existing.iter().enumerate() {
            let relative =
                stop.range().start - list_range.start..stop.range().end - list_range.start;
            patches.push((
                relative,
                if index == 0 {
                    replacement.clone()
                } else {
                    String::new()
                },
            ));
        }
        patches.sort_by(|a, b| b.0.start.cmp(&a.0.start));
        for (range, replacement) in patches {
            fragment.replace_range(range, &replacement);
        }
    } else {
        let insertion = if let Some(next) = list.children().find(Node::is_element) {
            fragment
                .find(&xml[next.range()])
                .unwrap_or(fragment.rfind("</").ok_or("malformed gradient stop list")?)
        } else {
            fragment.rfind("</").ok_or("malformed gradient stop list")?
        };
        fragment.insert_str(insertion, &replacement);
    }
    Ok(fragment)
}

fn apply_fill(xml: String, fill_edit: &Value) -> Result<String, String> {
    let edit = fill_edit.as_object().ok_or("fill edit must be an object")?;
    if edit.is_empty() {
        return Ok(xml);
    }
    let doc = Document::parse(&xml).map_err(|e| e.to_string())?;
    let shape = find_target_shape(&doc).ok_or("shape disappeared while editing fill")?;
    let properties = shape_properties(shape).ok_or("shape has no properties")?;
    let prefix = drawing_prefix(&xml, shape);
    let current = fill_child(properties);
    let requested_kind = edit.get("kind").and_then(Value::as_str);
    if edit.get("kind").is_some() && requested_kind.is_none() {
        return Err("fill.kind must be a string".to_string());
    }
    let inferred = requested_kind.unwrap_or_else(|| {
        current
            .map(|n| match n.tag_name().name() {
                "solidFill" => "solid",
                "gradFill" => "gradient",
                "noFill" => "none",
                _ => "other",
            })
            .unwrap_or(
                if [
                    "stops",
                    "angle",
                    "scaled",
                    "directionType",
                    "path",
                    "pathType",
                    "fillToRect",
                    "tileRect",
                    "flip",
                    "rotWithShape",
                ]
                .iter()
                .any(|field| edit.contains_key(*field))
                {
                    "gradient"
                } else {
                    "solid"
                },
            )
    });
    if !matches!(inferred, "none" | "solid" | "gradient") {
        return Err("fill.kind must be none, solid or gradient".to_string());
    }

    let same_kind = current
        .map(|n| match inferred {
            "none" => n.tag_name().name() == "noFill",
            "solid" => n.tag_name().name() == "solidFill",
            "gradient" => n.tag_name().name() == "gradFill",
            _ => false,
        })
        .unwrap_or(false);

    if !same_kind {
        let replacement = match inferred {
            "none" => format!("<{prefix}:noFill/>"),
            "solid" => make_solid_fill(&prefix, edit)?,
            "gradient" => make_gradient_fill(&prefix, edit)?,
            _ => unreachable!(),
        };
        let patch = if let Some(current) = current {
            (current.range(), replacement)
        } else {
            ordered_child_patch(&xml, properties, 2, &replacement)?
        };
        drop(doc);
        return apply_many(xml, vec![patch]);
    }
    if inferred == "none" {
        return Ok(xml);
    }

    let current = current.unwrap();
    let current_range = current.range();
    let mut fragment = xml[current_range.clone()].to_string();
    let mut local_patches: Vec<(Range<usize>, String)> = Vec::new();
    if inferred == "gradient" && edit.contains_key("stops") {
        if let Some(list) = child(current, "gsLst") {
            local_patches.push((
                list.range().start - current_range.start..list.range().end - current_range.start,
                patch_gradient_stop_list(&xml, list, edit, &prefix)?,
            ));
        } else {
            let replacement = gradient_stops(&prefix, edit)?;
            let open = fragment.find('>').ok_or("malformed gradFill")? + 1;
            local_patches.push((open..open, replacement));
        }
    } else if edit.contains_key("color") || edit.contains_key("alpha") {
        let new_color = color_spec(edit.get("color"))?;
        let new_alpha = edit
            .get("alpha")
            .map(|v| json_number(v, "fill.alpha"))
            .transpose()?;
        if let Some(color) = color_node(current) {
            local_patches.push((
                color.range().start - current_range.start..color.range().end - current_range.start,
                patched_color_fragment(&xml, color, new_color.as_deref(), new_alpha)?,
            ));
        }
    }

    let mut tail_additions = String::new();
    if inferred == "gradient" {
        let direction_target = gradient_direction_target(edit)?;
        let direction = current
            .children()
            .find(|node| node.is_element() && matches!(node.tag_name().name(), "lin" | "path"));
        match direction_target {
            GradientDirectionTarget::Unchanged => {}
            GradientDirectionTarget::Remove => {
                if let Some(direction) = direction {
                    local_patches.push((
                        direction.range().start - current_range.start
                            ..direction.range().end - current_range.start,
                        String::new(),
                    ));
                }
            }
            GradientDirectionTarget::Linear => {
                let replacement = if direction.map(|node| node.tag_name().name()) == Some("lin") {
                    patch_linear_direction(&xml[direction.unwrap().range()], edit)?
                } else {
                    make_linear_direction(&prefix, edit)?
                };
                if let Some(direction) = direction {
                    local_patches.push((
                        direction.range().start - current_range.start
                            ..direction.range().end - current_range.start,
                        replacement,
                    ));
                } else if let Some(tile_rect) = child(current, "tileRect") {
                    let at = tile_rect.range().start - current_range.start;
                    local_patches.push((at..at, replacement));
                } else {
                    tail_additions.push_str(&replacement);
                }
            }
            GradientDirectionTarget::Path => {
                let replacement =
                    if let Some(path) = direction.filter(|node| node.tag_name().name() == "path") {
                        patch_path_direction(&xml, path, edit, &prefix)?
                    } else {
                        make_path_direction(&prefix, edit)?
                    };
                if let Some(direction) = direction {
                    local_patches.push((
                        direction.range().start - current_range.start
                            ..direction.range().end - current_range.start,
                        replacement,
                    ));
                } else if let Some(tile_rect) = child(current, "tileRect") {
                    let at = tile_rect.range().start - current_range.start;
                    local_patches.push((at..at, replacement));
                } else {
                    tail_additions.push_str(&replacement);
                }
            }
        }

        if let Some(value) = edit.get("tileRect") {
            if let Some(tile_rect) = child(current, "tileRect") {
                let relative = tile_rect.range().start - current_range.start
                    ..tile_rect.range().end - current_range.start;
                let replacement = if value.is_null() {
                    String::new()
                } else {
                    patch_relative_rect(&xml[tile_rect.range()], value, "fill.tileRect")?
                };
                local_patches.push((relative, replacement));
            } else if let Some(replacement) =
                make_relative_rect(&prefix, "tileRect", value, "fill.tileRect")?
            {
                tail_additions.push_str(&replacement);
            }
        }
        if !tail_additions.is_empty() {
            let close = fragment.rfind("</").ok_or("malformed gradFill")?;
            local_patches.push((close..close, tail_additions));
        }
    }

    local_patches.sort_by(|a, b| {
        b.0.start
            .cmp(&a.0.start)
            .then_with(|| b.0.end.cmp(&a.0.end))
    });
    for (range, replacement) in local_patches {
        fragment.replace_range(range, &replacement);
    }
    if inferred == "gradient" {
        if let Some(value) = edit.get("flip") {
            fragment = if value.is_null() {
                remove_start_attribute(&fragment, "flip")
            } else {
                set_start_attribute(&fragment, "flip", gradient_flip(value)?)
            };
        }
        if let Some(value) = edit.get("rotWithShape") {
            fragment = if value.is_null() {
                remove_start_attribute(&fragment, "rotWithShape")
            } else {
                let enabled = value
                    .as_bool()
                    .ok_or("fill.rotWithShape must be boolean or null")?;
                set_start_attribute(&fragment, "rotWithShape", if enabled { "1" } else { "0" })
            };
        }
    }
    drop(doc);
    Ok(replace(xml, current_range, &fragment))
}

fn make_line(prefix: &str, edit: &serde_json::Map<String, Value>) -> Result<String, String> {
    let mut attrs = String::new();
    if let Some(width) = edit.get("width") {
        attrs.push_str(&format!(
            " w=\"{}\"",
            round_i64(json_number(width, "line.width")? * EMU_PER_POINT)
        ));
    }
    let color = color_spec(edit.get("color"))?.unwrap_or_else(|| "#000000".to_string());
    let alpha = edit
        .get("alpha")
        .map(|v| json_number(v, "line.alpha"))
        .transpose()?;
    let dash = edit.get("dash").and_then(Value::as_str).unwrap_or("solid");
    if !valid_token(dash) {
        return Err("invalid line dash".to_string());
    }
    Ok(format!(
        "<{prefix}:ln{attrs}><{prefix}:solidFill>{}</{prefix}:solidFill><{prefix}:prstDash val=\"{}\"/></{prefix}:ln>",
        make_color(prefix, &color, alpha)?,
        xml_escape_attr(dash)
    ))
}

fn apply_line(xml: String, line_edit: &Value) -> Result<String, String> {
    let edit = line_edit.as_object().ok_or("line edit must be an object")?;
    if edit.is_empty() {
        return Ok(xml);
    }
    let doc = Document::parse(&xml).map_err(|e| e.to_string())?;
    let shape = find_target_shape(&doc).ok_or("shape disappeared while editing line")?;
    let properties = shape_properties(shape).ok_or("shape has no properties")?;
    if properties.tag_name().name() == "grpSpPr" {
        return Err("group properties do not support a line".to_string());
    }
    let prefix = drawing_prefix(&xml, shape);
    let Some(line) = child(properties, "ln") else {
        let line_xml = make_line(&prefix, edit)?;
        let patch = ordered_child_patch(&xml, properties, 3, &line_xml)?;
        drop(doc);
        return apply_many(xml, vec![patch]);
    };
    let line_range = line.range();
    let mut fragment = xml[line_range.clone()].to_string();
    let mut patches = Vec::new();
    if edit.contains_key("color") || edit.contains_key("alpha") {
        let new_color = color_spec(edit.get("color"))?;
        let new_alpha = edit
            .get("alpha")
            .map(|v| json_number(v, "line.alpha"))
            .transpose()?;
        let fill = fill_child(line);
        if let Some(solid) = fill.filter(|n| n.tag_name().name() == "solidFill") {
            if let Some(color) = color_node(solid) {
                patches.push((
                    color.range().start - line_range.start..color.range().end - line_range.start,
                    patched_color_fragment(&xml, color, new_color.as_deref(), new_alpha)?,
                ));
            }
        } else {
            let color = new_color.unwrap_or_else(|| "#000000".to_string());
            let replacement = format!(
                "<{prefix}:solidFill>{}</{prefix}:solidFill>",
                make_color(&prefix, &color, new_alpha)?
            );
            if let Some(fill) = fill {
                patches.push((
                    fill.range().start - line_range.start..fill.range().end - line_range.start,
                    replacement,
                ));
            } else {
                let open = fragment.find('>').ok_or("malformed line")? + 1;
                patches.push((open..open, replacement));
            }
        }
    }
    if let Some(dash) = edit.get("dash") {
        let dash = dash.as_str().ok_or("line.dash must be a string")?;
        if !valid_token(dash) {
            return Err("invalid line dash".to_string());
        }
        let replacement = format!("<{prefix}:prstDash val=\"{}\"/>", xml_escape_attr(dash));
        if let Some(current) = child(line, "prstDash") {
            patches.push((
                current.range().start - line_range.start..current.range().end - line_range.start,
                replacement,
            ));
        } else {
            let close = fragment.rfind("</").ok_or("malformed line")?;
            patches.push((close..close, replacement));
        }
    }
    patches.sort_by(|a, b| b.0.start.cmp(&a.0.start));
    for (range, replacement) in patches {
        fragment.replace_range(range, &replacement);
    }
    if let Some(width) = edit.get("width") {
        fragment = set_start_attribute(
            &fragment,
            "w",
            &round_i64(json_number(width, "line.width")? * EMU_PER_POINT).to_string(),
        );
    }
    drop(doc);
    Ok(replace(xml, line_range, &fragment))
}

fn apply_effects(mut xml: String, effects_edit: &Value) -> Result<String, String> {
    let edit = effects_edit
        .as_object()
        .ok_or("effects edit must be an object")?;
    if let Some(shadow) = edit.get("shadow") {
        xml = apply_shadow(xml, shadow)?;
    }
    if let Some(soft_edge) = edit.get("softEdge") {
        xml = apply_soft_edge(xml, soft_edge)?;
    }
    Ok(xml)
}

fn make_shadow(prefix: &str, edit: &serde_json::Map<String, Value>) -> Result<String, String> {
    let blur = edit
        .get("blur")
        .map(|v| json_number(v, "shadow.blur"))
        .transpose()?
        .unwrap_or(4.0);
    let distance = edit
        .get("distance")
        .map(|v| json_number(v, "shadow.distance"))
        .transpose()?
        .unwrap_or(3.0);
    let angle = edit
        .get("angle")
        .map(|v| json_number(v, "shadow.angle"))
        .transpose()?
        .unwrap_or(45.0);
    let color = color_spec(edit.get("color"))?.unwrap_or_else(|| "#000000".to_string());
    let alpha = edit
        .get("alpha")
        .map(|v| json_number(v, "shadow.alpha"))
        .transpose()?
        .or(Some(0.5));
    Ok(format!(
        "<{prefix}:outerShdw blurRad=\"{}\" dist=\"{}\" dir=\"{}\" algn=\"ctr\" rotWithShape=\"0\">{}</{prefix}:outerShdw>",
        round_i64(blur * EMU_PER_POINT),
        round_i64(distance * EMU_PER_POINT),
        round_i64(angle * ANGLE_UNIT),
        make_color(prefix, &color, alpha)?
    ))
}

fn apply_shadow(xml: String, shadow_edit: &Value) -> Result<String, String> {
    let edit = shadow_edit
        .as_object()
        .ok_or("effects.shadow must be an object")?;
    let enabled = edit
        .get("enabled")
        .map(|v| v.as_bool().ok_or("shadow.enabled must be boolean"))
        .transpose()?
        .unwrap_or(true);
    let doc = Document::parse(&xml).map_err(|e| e.to_string())?;
    let shape = find_target_shape(&doc).ok_or("shape disappeared while editing shadow")?;
    let properties = shape_properties(shape).ok_or("shape has no properties")?;
    let prefix = drawing_prefix(&xml, shape);
    let effect_list = child(properties, "effectLst");
    let shadow = effect_list.and_then(|e| child(e, "outerShdw"));
    if !enabled {
        let Some(shadow) = shadow else {
            return Ok(xml);
        };
        let range = shadow.range();
        drop(doc);
        return Ok(replace(xml, range, ""));
    }
    let Some(shadow) = shadow else {
        let shadow_xml = make_shadow(&prefix, edit)?;
        let patch = if let Some(effect_list) = effect_list {
            append_child_patch(&xml, effect_list, &shadow_xml)?
        } else {
            ordered_child_patch(
                &xml,
                properties,
                4,
                &format!("<{prefix}:effectLst>{shadow_xml}</{prefix}:effectLst>"),
            )?
        };
        drop(doc);
        return apply_many(xml, vec![patch]);
    };
    let shadow_range = shadow.range();
    let mut fragment = xml[shadow_range.clone()].to_string();
    // Patch descendants before changing the start-tag length.  The descendant ranges below are
    // measured against the original fragment; applying blur/dist/dir first can shift those byte
    // offsets and splice the colour XML into the middle of an attribute (Office commonly emits
    // short values such as dist="0", while our edited EMU value can be several digits longer).
    if edit.contains_key("color") || edit.contains_key("alpha") {
        let new_color = color_spec(edit.get("color"))?;
        let new_alpha = edit
            .get("alpha")
            .map(|v| json_number(v, "shadow.alpha"))
            .transpose()?;
        if let Some(color) = color_node(shadow) {
            let relative =
                color.range().start - shadow_range.start..color.range().end - shadow_range.start;
            fragment.replace_range(
                relative,
                &patched_color_fragment(&xml, color, new_color.as_deref(), new_alpha)?,
            );
        } else {
            let color = new_color.unwrap_or_else(|| "#000000".to_string());
            fragment = append_to_fragment(&fragment, &make_color(&prefix, &color, new_alpha)?)?;
        }
    }
    for (json_name, xml_name, factor) in [
        ("blur", "blurRad", EMU_PER_POINT),
        ("distance", "dist", EMU_PER_POINT),
        ("angle", "dir", ANGLE_UNIT),
    ] {
        if let Some(value) = edit.get(json_name) {
            fragment = set_start_attribute(
                &fragment,
                xml_name,
                &round_i64(json_number(value, &format!("shadow.{json_name}"))? * factor)
                    .to_string(),
            );
        }
    }
    drop(doc);
    Ok(replace(xml, shadow_range, &fragment))
}

fn apply_soft_edge(xml: String, value: &Value) -> Result<String, String> {
    let radius = json_number(value, "effects.softEdge")?;
    let doc = Document::parse(&xml).map_err(|e| e.to_string())?;
    let shape = find_target_shape(&doc).ok_or("shape disappeared while editing soft edge")?;
    let properties = shape_properties(shape).ok_or("shape has no properties")?;
    let prefix = drawing_prefix(&xml, shape);
    let effect_list = child(properties, "effectLst");
    let soft = effect_list.and_then(|e| child(e, "softEdge"));
    if radius <= 0.0 {
        let Some(soft) = soft else {
            return Ok(xml);
        };
        let range = soft.range();
        drop(doc);
        return Ok(replace(xml, range, ""));
    }
    let soft_xml = format!(
        "<{prefix}:softEdge rad=\"{}\"/>",
        round_i64(radius * EMU_PER_POINT)
    );
    let patch = if let Some(soft) = soft {
        (soft.range(), soft_xml)
    } else if let Some(effect_list) = effect_list {
        append_child_patch(&xml, effect_list, &soft_xml)?
    } else {
        ordered_child_patch(
            &xml,
            properties,
            4,
            &format!("<{prefix}:effectLst>{soft_xml}</{prefix}:effectLst>"),
        )?
    };
    drop(doc);
    apply_many(xml, vec![patch])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient_stop_fragments(xml: &str) -> Vec<String> {
        let document = Document::parse(xml).unwrap();
        document
            .descendants()
            .filter(|node| node.is_element() && node.tag_name().name() == "gs")
            .map(|node| xml[node.range()].to_string())
            .collect()
    }

    fn text_fragment_by_keep(xml: &str, local_name: &str, keep: &str) -> String {
        let document = Document::parse(xml).unwrap();
        let element = document
            .descendants()
            .find(|node| {
                node.is_element()
                    && node.tag_name().name() == local_name
                    && node.attribute(("urn:unicell:text-test", "keep")) == Some(keep)
            })
            .unwrap();
        xml[element.range()].to_string()
    }

    const ANCHOR: &str = r#"<xdr:absoluteAnchor xmlns:xdr="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:u="urn:unicell:test"><xdr:pos x="100" y="200"/><xdr:ext cx="300" cy="400"/><xdr:sp macro=""><xdr:nvSpPr><xdr:cNvPr id="4" name="GradientShape"/><xdr:cNvSpPr/></xdr:nvSpPr><xdr:spPr bwMode="auto"><a:xfrm rot="900000" flipH="0"><a:off x="11" y="22"/><a:ext cx="33" cy="44"/></a:xfrm><a:prstGeom prst="roundRect"><a:avLst/><a:gdLst><a:gd name="keepGeom" fmla="val 1"/></a:gdLst></a:prstGeom><a:gradFill rotWithShape="1"><a:gsLst><a:gs pos="0"><a:schemeClr val="accent1"><a:tint val="35000"/><a:alpha val="80000"/></a:schemeClr></a:gs><a:gs pos="45000"><a:srgbClr val="44AAEE"><a:alpha val="65000"/></a:srgbClr></a:gs><a:gs pos="100000"><a:schemeClr val="accent2"><a:shade val="30000"/><a:satMod val="125000"/></a:schemeClr></a:gs></a:gsLst><a:path path="circle"><a:fillToRect l="20000" t="10000" r="30000" b="40000"/></a:path><a:tileRect l="1" t="2" r="3" b="4"/></a:gradFill><a:ln w="25400" cap="rnd"><a:solidFill><a:schemeClr val="accent4"><a:lumMod val="70000"/><a:alpha val="90000"/></a:schemeClr></a:solidFill><a:prstDash val="dash"/><a:headEnd type="triangle"/></a:ln><a:effectLst><a:outerShdw blurRad="50800" dist="38100" dir="2700000" algn="ctr" rotWithShape="0"><a:schemeClr val="dk1"><a:alpha val="50000"/><a:tint val="10000"/></a:schemeClr></a:outerShdw><a:softEdge rad="12700"/><a:glow rad="999"><a:srgbClr val="ABCDEF"/></a:glow></a:effectLst><a:extLst><a:ext uri="{keep-me}"><u:sentinel foo="bar"/></a:ext></a:extLst></xdr:spPr><xdr:style><a:lnRef idx="2"><a:schemeClr val="accent1"/></a:lnRef></xdr:style><xdr:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:rPr b="1"/><a:t>Bold</a:t></a:r><a:r><a:rPr i="1"/><a:t>Italic</a:t></a:r></a:p><a:p><a:r><a:rPr lang="zh-CN"/><a:t>Second</a:t></a:r></a:p></xdr:txBody></xdr:sp><xdr:clientData/></xdr:absoluteAnchor>"#;

    const LINEAR_ANCHOR: &str = r#"<xdr:absoluteAnchor xmlns:xdr="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:u="urn:unicell:test"><xdr:pos x="0" y="0"/><xdr:ext cx="1" cy="1"/><xdr:sp><xdr:nvSpPr><xdr:cNvPr id="8" name="LinearGradient"/><xdr:cNvSpPr/></xdr:nvSpPr><xdr:spPr><a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:gradFill flip="xy" rotWithShape="0" u:keep="gradient"><a:gsLst u:keep="list"><a:gs pos="0"><a:schemeClr val="accent1"><a:tint val="23000"/><a:alpha val="77000"/><u:colorExtension marker="keep"/></a:schemeClr></a:gs><a:gs pos="100000"><a:srgbClr val="ABCDEF"/></a:gs><u:listExtension marker="keep"/></a:gsLst><a:lin ang="2700000" scaled="0" u:keep="linear"/><a:tileRect l="-10000" t="20000" r="30000" b="40000" u:keep="tile"/><u:gradientExtension marker="keep"/></a:gradFill></xdr:spPr></xdr:sp><xdr:clientData/></xdr:absoluteAnchor>"#;

    const RICH_TEXT_ANCHOR: &str = r#"<xdr:absoluteAnchor xmlns:xdr="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:u="urn:unicell:text-test"><xdr:pos x="0" y="0"/><xdr:ext cx="1" cy="1"/><xdr:sp><xdr:nvSpPr><xdr:cNvPr id="12" name="RichTextShape"/><xdr:cNvSpPr/></xdr:nvSpPr><xdr:spPr><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></xdr:spPr><xdr:txBody u:keep="body"><a:bodyPr/><a:lstStyle/><a:p u:keep="paragraph"><a:pPr algn="ctr"><u:paragraphOpaque marker="keep"/></a:pPr><a:r u:keep="run-one"><a:rPr lang="en-US" sz="1200" b="1" i="0" u="sng" dirty="0" u:keep="properties"><a:solidFill><a:schemeClr val="accent2"><a:tint val="23000"/><a:alpha val="65000"/><u:colorTransform marker="keep"/></a:schemeClr></a:solidFill><a:latin typeface="Aptos" pitchFamily="34"/><u:runPropertiesOpaque marker="keep"/></a:rPr><a:t>First</a:t><u:runOpaque marker="keep"/></a:r><a:r u:keep="run-two"><a:rPr lang="zh-CN" i="1"><a:ea typeface="宋体" charset="134"/></a:rPr><a:t>第二</a:t></a:r><a:endParaRPr lang="en-US"/></a:p><a:p u:keep="paragraph-two"><a:r><a:rPr lang="en-US"/><a:t>Tail</a:t></a:r></a:p><u:bodyOpaque marker="keep"/></xdr:txBody></xdr:sp><xdr:clientData/></xdr:absoluteAnchor>"#;

    #[test]
    fn parses_deep_shape_surface_in_excel_units() {
        let model = parse_shape_model(ANCHOR);
        assert_eq!(model["text"], "BoldItalic\nSecond");
        assert_eq!(model["geometry"], "roundRect");
        assert_eq!(model["rotation"], 15.0);
        assert_eq!(model["flipH"], false);
        assert_eq!(model["fill"]["kind"], "gradient");
        assert_eq!(model["fill"]["stops"].as_array().unwrap().len(), 3);
        assert_eq!(model["fill"]["stops"][0]["color"], "accent1");
        assert_eq!(model["fill"]["stops"][0]["alpha"], 0.8);
        assert_eq!(model["fill"]["directionType"], "path");
        assert_eq!(model["fill"]["path"], "circle");
        assert_eq!(model["fill"]["fillToRect"]["l"], 0.2);
        assert_eq!(model["fill"]["fillToRect"]["b"], 0.4);
        assert_eq!(model["fill"]["tileRect"]["r"], 0.00003);
        assert_eq!(model["fill"]["rotWithShape"], true);
        assert_eq!(model["line"]["width"], 2.0);
        assert_eq!(model["line"]["dash"], "dash");
        assert_eq!(model["effects"]["shadow"]["blur"], 4.0);
        assert_eq!(model["effects"]["shadow"]["distance"], 3.0);
        assert_eq!(model["effects"]["shadow"]["angle"], 45.0);
        assert_eq!(model["effects"]["softEdge"], 1.0);
    }

    #[test]
    fn color_specs_keep_color_space_and_transform_order() {
        let model = parse_shape_model(ANCHOR);
        assert_eq!(model["fill"]["stops"][0]["colorSpec"]["type"], "scheme");
        assert_eq!(model["fill"]["stops"][0]["colorSpec"]["value"], "accent1");
        assert_eq!(
            model["fill"]["stops"][0]["colorSpec"]["transforms"][0]["type"],
            "tint"
        );
        assert_eq!(
            model["fill"]["stops"][0]["colorSpec"]["transforms"][0]["value"],
            35_000.0
        );
        assert_eq!(
            model["fill"]["stops"][0]["colorSpec"]["transforms"][1]["type"],
            "alpha"
        );
        assert_eq!(
            model["line"]["colorSpec"]["transforms"][0]["type"],
            "lumMod"
        );

        let choices = format!(
            r#"<root xmlns:a="{DRAWING_NS}"><a:scrgbClr r="10000" g="50000" b="100000"><a:satMod val="80000"/></a:scrgbClr><a:hslClr hue="7200000" sat="60000" lum="40000"><a:hueOff val="60000"/><a:lumOff val="10000"/></a:hslClr><a:sysClr val="windowText" lastClr="112233"/><a:prstClr val="dkSeaGreen"/></root>"#
        );
        let document = Document::parse(&choices).unwrap();
        let specs: Vec<Value> = document
            .root_element()
            .children()
            .filter(|node| node.is_element())
            .map(drawing_color_spec)
            .collect();
        assert_eq!(specs[0]["type"], "scrgb");
        assert_eq!(specs[0]["r"], 10_000.0);
        assert_eq!(specs[0]["transforms"][0]["type"], "satMod");
        assert_eq!(specs[1]["type"], "hsl");
        assert_eq!(specs[1]["hue"], 7_200_000.0);
        assert_eq!(specs[1]["transforms"][0]["type"], "hueOff");
        assert_eq!(specs[2]["type"], "system");
        assert_eq!(specs[2]["lastColor"], "#112233");
        assert_eq!(specs[3]["type"], "preset");
        assert_eq!(specs[3]["value"], "dkSeaGreen");
    }

    #[test]
    fn resolves_theme_system_preset_scrgb_hsl_and_ordered_transforms() {
        let mut theme = Theme::default();
        theme.accent1 = "#808080".to_string();
        let xml = format!(
            r#"<root xmlns:a="{DRAWING_NS}"><a:schemeClr val="accent1"><a:tint val="50000"/><a:shade val="50000"/></a:schemeClr><a:schemeClr val="accent1"><a:lumMod val="50000"/><a:lumOff val="10000"/></a:schemeClr><a:sysClr val="windowText" lastClr="123456"/><a:prstClr val="dkSeaGreen"/><a:scrgbClr r="50000" g="50000" b="50000"/><a:hslClr hue="7200000" sat="100000" lum="50000"/><a:srgbClr val="336699"><a:alpha val="80000"/><a:alphaMod val="50000"/><a:alphaOff val="10000"/></a:srgbClr><a:schemeClr val="phClr"/></root>"#
        );
        let document = Document::parse(&xml).unwrap();
        let colors: Vec<_> = document
            .root_element()
            .children()
            .filter(|node| node.is_element())
            .collect();
        assert_eq!(
            resolve_drawing_color(colors[0], &theme).unwrap().0,
            "#606060"
        );
        assert_eq!(
            resolve_drawing_color(colors[1], &theme).unwrap().0,
            "#5A5A5A"
        );
        assert_eq!(
            resolve_drawing_color(colors[2], &theme).unwrap().0,
            "#123456"
        );
        assert_eq!(
            resolve_drawing_color(colors[3], &theme).unwrap().0,
            "#8FBC8F"
        );
        assert_eq!(
            resolve_drawing_color(colors[4], &theme).unwrap().0,
            "#BCBCBC"
        );
        assert_eq!(
            resolve_drawing_color(colors[5], &theme).unwrap().0,
            "#00FF00"
        );
        assert!((resolve_drawing_color(colors[6], &theme).unwrap().1 - 0.5).abs() < 1e-12);
        assert!(resolve_drawing_color(colors[7], &theme).is_none());
    }

    #[test]
    fn parses_paragraphs_runs_and_direct_run_properties() {
        let model = parse_shape_model(RICH_TEXT_ANCHOR);
        assert_eq!(model["text"], "First第二\nTail");
        assert_eq!(model["paragraphs"].as_array().unwrap().len(), 2);
        assert_eq!(model["paragraphs"][0]["text"], "First第二");
        assert_eq!(model["paragraphs"][0]["sourceIndex"], 0);
        assert_eq!(model["paragraphs"][1]["sourceIndex"], 1);
        assert_eq!(model["paragraphs"][0]["runs"].as_array().unwrap().len(), 2);
        let run = &model["paragraphs"][0]["runs"][0];
        assert_eq!(run["sourceIndex"], 0);
        assert_eq!(run["kind"], "r");
        assert_eq!(run["text"], "First");
        assert_eq!(run["font"], "Aptos");
        assert_eq!(run["fontScript"], "latin");
        assert_eq!(run["size"], 12.0);
        assert_eq!(run["bold"], true);
        assert_eq!(run["italic"], false);
        assert_eq!(run["underline"], "sng");
        assert_eq!(run["color"], "accent2");
        assert_eq!(run["alpha"], 0.65);
        assert_eq!(model["paragraphs"][0]["runs"][1]["font"], "宋体");
        assert_eq!(model["paragraphs"][0]["runs"][1]["fontScript"], "ea");
        assert_eq!(model["paragraphs"][0]["runs"][1]["sourceIndex"], 1);
    }

    #[test]
    fn deleting_first_run_moves_the_surviving_run_with_its_opaque_xml() {
        let surviving = text_fragment_by_keep(RICH_TEXT_ANCHOR, "r", "run-two");
        let mut paragraphs = parse_shape_model(RICH_TEXT_ANCHOR)["paragraphs"].clone();
        let runs = paragraphs[0]["runs"].as_array_mut().unwrap();
        runs.remove(0);
        paragraphs[0]["text"] = json!(runs[0]["text"].as_str().unwrap());
        let edited = apply_shape_edit(RICH_TEXT_ANCHOR, &json!({"paragraphs":paragraphs})).unwrap();
        Document::parse(&edited).unwrap();
        assert!(!edited.contains("u:keep=\"run-one\""));
        assert!(!edited.contains("<u:runOpaque marker=\"keep\"/>"));
        assert_eq!(text_fragment_by_keep(&edited, "r", "run-two"), surviving);
        let reparsed = parse_shape_model(&edited);
        assert_eq!(
            reparsed["paragraphs"][0]["runs"].as_array().unwrap().len(),
            1
        );
        assert_eq!(reparsed["paragraphs"][0]["runs"][0]["text"], "第二");
    }

    #[test]
    fn deleting_first_paragraph_moves_the_survivor_without_inheriting_opaque_xml() {
        let surviving = text_fragment_by_keep(RICH_TEXT_ANCHOR, "p", "paragraph-two");
        let mut paragraphs = parse_shape_model(RICH_TEXT_ANCHOR)["paragraphs"]
            .as_array()
            .unwrap()
            .clone();
        paragraphs.remove(0);
        let edited = apply_shape_edit(RICH_TEXT_ANCHOR, &json!({"paragraphs":paragraphs})).unwrap();
        Document::parse(&edited).unwrap();
        assert!(!edited.contains("u:keep=\"paragraph\""));
        assert!(!edited.contains("<u:paragraphOpaque marker=\"keep\"/>"));
        assert_eq!(
            text_fragment_by_keep(&edited, "p", "paragraph-two"),
            surviving
        );
        let reparsed = parse_shape_model(&edited);
        assert_eq!(reparsed["paragraphs"].as_array().unwrap().len(), 1);
        assert_eq!(reparsed["paragraphs"][0]["text"], "Tail");
    }

    #[test]
    fn legacy_text_payload_semantically_matches_surviving_run_after_delete() {
        let surviving = text_fragment_by_keep(RICH_TEXT_ANCHOR, "r", "run-two");
        let mut paragraphs = parse_shape_model(RICH_TEXT_ANCHOR)["paragraphs"].clone();
        for paragraph in paragraphs.as_array_mut().unwrap() {
            paragraph.as_object_mut().unwrap().remove("sourceIndex");
            for run in paragraph["runs"].as_array_mut().unwrap() {
                run.as_object_mut().unwrap().remove("sourceIndex");
            }
        }
        let runs = paragraphs[0]["runs"].as_array_mut().unwrap();
        runs.remove(0);
        paragraphs[0]["text"] = json!(runs[0]["text"].as_str().unwrap());
        let edited = apply_shape_edit(RICH_TEXT_ANCHOR, &json!({"paragraphs":paragraphs})).unwrap();
        assert_eq!(text_fragment_by_keep(&edited, "r", "run-two"), surviving);
        assert!(!edited.contains("u:keep=\"run-one\""));
    }

    #[test]
    fn parsed_text_structure_is_a_byte_exact_noop() {
        let model = parse_shape_model(RICH_TEXT_ANCHOR);
        let edited = apply_shape_edit(
            RICH_TEXT_ANCHOR,
            &json!({"paragraphs":model["paragraphs"].clone()}),
        )
        .unwrap();
        assert_eq!(edited, RICH_TEXT_ANCHOR);
    }

    #[test]
    fn self_closing_run_properties_expand_for_font_and_colour_children() {
        let model = parse_shape_model(ANCHOR);
        let mut paragraphs = model["paragraphs"].clone();
        let run = &mut paragraphs[0]["runs"][0];
        run["font"] = json!("Arial");
        run["color"] = json!("#FF0000");
        run["alpha"] = json!(0.8);

        let edited = apply_shape_edit(ANCHOR, &json!({"paragraphs":paragraphs})).unwrap();
        Document::parse(&edited).unwrap();
        assert!(edited.contains("<a:rPr b=\"1\">"));
        assert!(edited.contains("<a:srgbClr val=\"FF0000\"><a:alpha val=\"80000\"/>"));
        assert!(edited.contains("<a:latin typeface=\"Arial\"/>"));
        let reparsed = parse_shape_model(&edited);
        let run = &reparsed["paragraphs"][0]["runs"][0];
        assert_eq!(run["font"], "Arial");
        assert_eq!(run["color"], "#FF0000");
        assert_eq!(run["alpha"], 0.8);
    }

    #[test]
    fn edits_one_run_without_touching_other_runs_or_unknown_markup() {
        let model = parse_shape_model(RICH_TEXT_ANCHOR);
        let mut paragraphs = model["paragraphs"].clone();
        let run = &mut paragraphs[0]["runs"][0];
        run["text"] = json!("Edited first");
        run["font"] = json!("Calibri");
        run["size"] = json!(14.5);
        run["bold"] = json!(false);
        run["italic"] = json!(true);
        run["underline"] = json!("dbl");
        run["color"] = json!("#123456");
        run["alpha"] = json!(0.4);

        let original_document = Document::parse(RICH_TEXT_ANCHOR).unwrap();
        let original_second_run = original_document
            .descendants()
            .find(|node| {
                node.is_element()
                    && node.tag_name().name() == "r"
                    && child(*node, "t").and_then(|text| text.text()) == Some("第二")
            })
            .map(|node| RICH_TEXT_ANCHOR[node.range()].to_string())
            .unwrap();
        let original_second_paragraph = original_document
            .descendants()
            .find(|node| {
                node.is_element()
                    && node.tag_name().name() == "p"
                    && node.attribute(("urn:unicell:text-test", "keep")) == Some("paragraph-two")
            })
            .map(|node| RICH_TEXT_ANCHOR[node.range()].to_string())
            .unwrap();

        let edited = apply_shape_edit(
            RICH_TEXT_ANCHOR,
            &json!({"text":"ignored because structured text wins", "paragraphs":paragraphs}),
        )
        .unwrap();
        let edited_document = Document::parse(&edited).unwrap();
        let edited_second_run = edited_document
            .descendants()
            .find(|node| {
                node.is_element()
                    && node.tag_name().name() == "r"
                    && child(*node, "t").and_then(|text| text.text()) == Some("第二")
            })
            .map(|node| edited[node.range()].to_string())
            .unwrap();
        let edited_second_paragraph = edited_document
            .descendants()
            .find(|node| {
                node.is_element()
                    && node.tag_name().name() == "p"
                    && node.attribute(("urn:unicell:text-test", "keep")) == Some("paragraph-two")
            })
            .map(|node| edited[node.range()].to_string())
            .unwrap();
        assert_eq!(edited_second_run, original_second_run);
        assert_eq!(edited_second_paragraph, original_second_paragraph);
        assert!(edited.contains("u:keep=\"body\""));
        assert!(edited.contains("<u:paragraphOpaque marker=\"keep\"/>"));
        assert!(edited.contains("u:keep=\"properties\""));
        assert!(!edited.contains("<a:tint val=\"23000\"/>"));
        assert!(edited.contains("<u:colorTransform marker=\"keep\"/>"));
        assert!(edited.contains("<u:runPropertiesOpaque marker=\"keep\"/>"));
        assert!(edited.contains("<u:runOpaque marker=\"keep\"/>"));
        assert!(edited.contains("pitchFamily=\"34\""));
        let reparsed = parse_shape_model(&edited);
        let run = &reparsed["paragraphs"][0]["runs"][0];
        assert_eq!(reparsed["text"], "Edited first第二\nTail");
        assert_eq!(run["text"], "Edited first");
        assert_eq!(run["font"], "Calibri");
        assert_eq!(run["size"], 14.5);
        assert_eq!(run["bold"], false);
        assert_eq!(run["italic"], true);
        assert_eq!(run["underline"], "dbl");
        assert_eq!(run["color"], "#123456");
        assert_eq!(run["alpha"], 0.4);
    }

    #[test]
    fn partial_edit_preserves_gradient_extensions_theme_transforms_and_rich_runs() {
        let original_gradient = Document::parse(ANCHOR)
            .unwrap()
            .descendants()
            .find(|n| n.is_element() && n.tag_name().name() == "gradFill")
            .map(|n| ANCHOR[n.range()].to_string())
            .unwrap();
        let edited = apply_shape_edit(
            ANCHOR,
            &json!({
                "text":"Strongly styled\n第二段",
                "rotation":37.5,
                "flipH":true,
                "line":{"width":3.25,"dash":"dashDot"},
                "effects":{"shadow":{"distance":7.0},"softEdge":2.5}
            }),
        )
        .unwrap();
        let doc = Document::parse(&edited).unwrap();
        let gradient = doc
            .descendants()
            .find(|n| n.is_element() && n.tag_name().name() == "gradFill")
            .unwrap();
        assert_eq!(&edited[gradient.range()], original_gradient);
        assert!(edited.contains("<a:rPr b=\"1\"/>"));
        assert!(edited.contains("<a:rPr i=\"1\"/>"));
        assert!(edited.contains("<a:rPr lang=\"zh-CN\"/>"));
        assert!(edited.contains("<u:sentinel foo=\"bar\"/>"));
        assert!(edited.contains("<a:headEnd type=\"triangle\"/>"));
        assert!(edited.contains("<a:glow rad=\"999\">"));
        let model = parse_shape_model(&edited);
        assert_eq!(model["text"], "Strongly styled\n第二段");
        assert_eq!(model["rotation"], 37.5);
        assert_eq!(model["flipH"], true);
        assert_eq!(model["line"]["width"], 3.25);
        assert_eq!(model["line"]["dash"], "dashDot");
        assert_eq!(model["effects"]["shadow"]["distance"], 7.0);
        assert_eq!(model["effects"]["softEdge"], 2.5);
    }

    #[test]
    fn shadow_colour_and_different_length_numeric_attributes_remain_well_formed() {
        let edited = apply_shape_edit(
            ANCHOR,
            &json!({"effects":{"shadow":{
                "color":"#404040",
                "alpha":0.45,
                "blur":0.1,
                "distance":123.456,
                "angle":3.0
            }}}),
        )
        .unwrap();
        Document::parse(&edited).unwrap();
        let model = parse_shape_model(&edited);
        assert_eq!(model["effects"]["shadow"]["color"], "#404040");
        assert_eq!(model["effects"]["shadow"]["alpha"], 0.45);
        assert_eq!(model["effects"]["shadow"]["blur"], 0.1);
        assert!((model["effects"]["shadow"]["distance"].as_f64().unwrap() - 123.456).abs() < 0.001);
        assert_eq!(model["effects"]["shadow"]["angle"], 3.0);
        assert!(!edited.contains("<a:tint val=\"10000\"/>"));
    }

    #[test]
    fn changing_scrgb_shadow_to_srgb_removes_incompatible_channel_attributes() {
        let source = format!(
            r#"<xdr:absoluteAnchor xmlns:xdr="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing" xmlns:a="{DRAWING_NS}"><xdr:pos x="0" y="0"/><xdr:ext cx="1" cy="1"/><xdr:sp><xdr:nvSpPr><xdr:cNvPr id="1" name="Shadow"/><xdr:cNvSpPr/></xdr:nvSpPr><xdr:spPr><a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:effectLst><a:outerShdw blurRad="1" dist="0" dir="0"><a:scrgbClr r="0" g="0" b="0"><a:alpha val="50000"/><a:tint val="10000"/></a:scrgbClr></a:outerShdw></a:effectLst></xdr:spPr></xdr:sp><xdr:clientData/></xdr:absoluteAnchor>"#
        );
        let edited = apply_shape_edit(
            &source,
            &json!({"effects":{"shadow":{
                "color":"#404040", "alpha":0.45, "blur":8, "distance":5, "angle":35
            }}}),
        )
        .unwrap();
        Document::parse(&edited).unwrap();
        assert!(edited.contains("<a:srgbClr val=\"404040\">"));
        assert!(!edited.contains("<a:srgbClr r="));
        assert!(!edited.contains(" g=\"0\""));
        assert!(!edited.contains(" b=\"0\""));
        assert!(!edited.contains("<a:tint val=\"10000\"/>"));
    }

    #[test]
    fn changing_scrgb_text_to_srgb_removes_incompatible_channel_attributes() {
        let source = RICH_TEXT_ANCHOR.replace(
            r#"<a:schemeClr val="accent2"><a:tint val="23000"/><a:alpha val="65000"/><u:colorTransform marker="keep"/></a:schemeClr>"#,
            r#"<a:scrgbClr r="10000" g="20000" b="30000"><a:tint val="23000"/><a:alpha val="65000"/><u:colorTransform marker="keep"/></a:scrgbClr>"#,
        );
        assert_ne!(source, RICH_TEXT_ANCHOR);
        let mut paragraphs = parse_shape_model(&source)["paragraphs"].clone();
        paragraphs[0]["runs"][0]["color"] = json!("#123456");
        let edited = apply_shape_edit(&source, &json!({"paragraphs":paragraphs})).unwrap();
        let document = Document::parse(&edited).unwrap();
        let color = document
            .descendants()
            .find(|node| node.is_element() && node.tag_name().name() == "srgbClr")
            .unwrap();
        assert_eq!(color.attribute("val"), Some("123456"));
        for incompatible in ["r", "g", "b", "hue", "sat", "lum", "lastClr"] {
            assert_eq!(color.attribute(incompatible), None);
        }
        assert!(!edited.contains("<a:tint val=\"23000\"/>"));
        assert!(edited.contains("<u:colorTransform marker=\"keep\"/>"));
    }

    #[test]
    fn editing_gradient_direction_or_stops_only_replaces_target_children() {
        let edited = apply_shape_edit(
            ANCHOR,
            &json!({"fill":{
                "kind":"gradient",
                "angle":30,
                "stops":[
                    {"position":0,"color":"accent3","alpha":0.75},
                    {"position":1,"color":"#123456","alpha":1}
                ]
            }}),
        )
        .unwrap();
        assert!(edited.contains("<a:lin ang=\"1800000\" scaled=\"1\"/>"));
        assert!(!edited.contains("<a:path path=\"circle\">"));
        assert!(edited.contains("<a:tileRect l=\"1\" t=\"2\" r=\"3\" b=\"4\"/>"));
        assert!(edited.contains("<a:ext uri=\"{keep-me}\">"));
        assert!(
            edited.contains("<a:schemeClr val=\"accent3\"><a:alpha val=\"75000\"/></a:schemeClr>")
        );
        assert!(edited.contains("<a:srgbClr val=\"123456\"><a:alpha val=\"100000\"/></a:srgbClr>"));
        assert_eq!(
            parse_shape_model(&edited)["fill"]["stops"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn gradient_stop_identity_moves_opaque_theme_payload_on_reorder_and_delete() {
        let original_fragments = gradient_stop_fragments(ANCHOR);
        let model = parse_shape_model(ANCHOR);
        assert_eq!(model["fill"]["stops"][0]["sourceIndex"], 0);
        assert_eq!(model["fill"]["stops"][1]["sourceIndex"], 1);
        assert_eq!(model["fill"]["stops"][2]["sourceIndex"], 2);
        let requested = json!([
            model["fill"]["stops"][2].clone(),
            model["fill"]["stops"][0].clone()
        ]);
        let edited = apply_shape_edit(ANCHOR, &json!({"fill":{"stops":requested}})).unwrap();
        Document::parse(&edited).unwrap();
        let edited_fragments = gradient_stop_fragments(&edited);
        assert_eq!(edited_fragments.len(), 2);
        assert_eq!(edited_fragments[0], original_fragments[2]);
        assert_eq!(edited_fragments[1], original_fragments[0]);
        assert!(!edited.contains(&original_fragments[1]));
        assert!(edited_fragments[0].contains("<a:shade val=\"30000\"/>"));
        assert!(edited_fragments[0].contains("<a:satMod val=\"125000\"/>"));
        assert!(edited_fragments[1].contains("<a:tint val=\"35000\"/>"));
    }

    #[test]
    fn legacy_gradient_stop_payload_uses_semantics_for_lossless_reorder() {
        let original_fragments = gradient_stop_fragments(ANCHOR);
        let model = parse_shape_model(ANCHOR);
        let mut requested = model["fill"]["stops"].as_array().unwrap().clone();
        requested.reverse();
        for stop in &mut requested {
            stop.as_object_mut().unwrap().remove("sourceIndex");
        }
        let edited = apply_shape_edit(ANCHOR, &json!({"fill":{"stops":requested}})).unwrap();
        let edited_fragments = gradient_stop_fragments(&edited);
        assert_eq!(
            edited_fragments,
            original_fragments.into_iter().rev().collect::<Vec<_>>()
        );
    }

    #[test]
    fn linear_gradient_metadata_is_parsed_and_edited_differentially() {
        let model = parse_shape_model(LINEAR_ANCHOR);
        assert_eq!(model["fill"]["directionType"], "linear");
        assert_eq!(model["fill"]["angle"], 45.0);
        assert_eq!(model["fill"]["scaled"], false);
        assert_eq!(model["fill"]["flip"], "xy");
        assert_eq!(model["fill"]["rotWithShape"], false);
        assert_eq!(model["fill"]["tileRect"]["l"], -0.1);
        assert_eq!(model["fill"]["tileRect"]["b"], 0.4);

        let edited = apply_shape_edit(
            LINEAR_ANCHOR,
            &json!({"fill":{
                "angle":60,
                "scaled":true,
                "tileRect":{"l":0.15,"r":null},
                "flip":"y",
                "rotWithShape":true
            }}),
        )
        .unwrap();
        assert!(edited.contains("<a:lin ang=\"3600000\" scaled=\"1\" u:keep=\"linear\"/>"));
        assert!(
            edited.contains("<a:tileRect l=\"15000\" t=\"20000\" b=\"40000\" u:keep=\"tile\"/>")
        );
        assert!(!edited.contains(" r=\"30000\""));
        assert!(edited.contains("flip=\"y\""));
        assert!(edited.contains("rotWithShape=\"1\""));
        assert!(edited.contains("<a:tint val=\"23000\"/>"));
        assert!(edited.contains("<u:colorExtension marker=\"keep\"/>"));
        assert!(edited.contains("<u:listExtension marker=\"keep\"/>"));
        assert!(edited.contains("<u:gradientExtension marker=\"keep\"/>"));
        let model = parse_shape_model(&edited);
        assert_eq!(model["fill"]["angle"], 60.0);
        assert_eq!(model["fill"]["scaled"], true);
        assert_eq!(model["fill"]["tileRect"]["l"], 0.15);
        assert!(model["fill"]["tileRect"]["r"].is_null());
    }

    #[test]
    fn path_gradient_metadata_is_parsed_and_edited_differentially() {
        let edited = apply_shape_edit(
            ANCHOR,
            &json!({"fill":{
                "directionType":"path",
                "path":"rect",
                "fillToRect":{"t":0.25,"r":null},
                "tileRect":{"b":0.5},
                "flip":"x",
                "rotWithShape":false
            }}),
        )
        .unwrap();
        assert!(edited.contains(
            "<a:path path=\"rect\"><a:fillToRect l=\"20000\" t=\"25000\" b=\"40000\"/></a:path>"
        ));
        assert!(edited.contains("<a:tileRect l=\"1\" t=\"2\" r=\"3\" b=\"50000\"/>"));
        assert!(edited.contains("flip=\"x\""));
        assert!(edited.contains("rotWithShape=\"0\""));
        assert!(edited.contains("<a:tint val=\"35000\"/>"));
        assert!(edited.contains("<a:shade val=\"30000\"/><a:satMod val=\"125000\"/>"));
        assert!(edited.contains("<a:ext uri=\"{keep-me}\">"));
        let model = parse_shape_model(&edited);
        assert_eq!(model["fill"]["directionType"], "path");
        assert_eq!(model["fill"]["path"], "rect");
        assert_eq!(model["fill"]["fillToRect"]["t"], 0.25);
        assert!(model["fill"]["fillToRect"]["r"].is_null());
        assert_eq!(model["fill"]["tileRect"]["b"], 0.5);
    }

    #[test]
    fn empty_gradient_edits_are_byte_exact_noops() {
        assert_eq!(
            apply_shape_edit(ANCHOR, &json!({"fill":{}})).unwrap(),
            ANCHOR
        );
        assert_eq!(
            apply_shape_edit(LINEAR_ANCHOR, &json!({})).unwrap(),
            LINEAR_ANCHOR
        );
    }

    #[test]
    fn nested_group_targets_child_and_can_remove_fill_and_shadow() {
        let grouped = format!(
            "<xdr:oneCellAnchor xmlns:xdr=\"http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing\" xmlns:a=\"{DRAWING_NS}\"><xdr:from><xdr:col>0</xdr:col><xdr:row>0</xdr:row></xdr:from><xdr:ext cx=\"1\" cy=\"1\"/><xdr:grpSp><xdr:nvGrpSpPr><xdr:cNvPr id=\"1\" name=\"Group\"/><xdr:cNvGrpSpPr/></xdr:nvGrpSpPr><xdr:grpSpPr><a:xfrm><a:off x=\"0\" y=\"0\"/></a:xfrm><a:extLst><a:ext uri=\"group-keep\"/></a:extLst></xdr:grpSpPr><xdr:cxnSp><xdr:nvCxnSpPr><xdr:cNvPr id=\"2\" name=\"Connector\"/><xdr:cNvCxnSpPr/></xdr:nvCxnSpPr><xdr:spPr><a:prstGeom prst=\"line\"><a:avLst/></a:prstGeom><a:solidFill><a:schemeClr val=\"accent5\"/></a:solidFill><a:ln><a:solidFill><a:srgbClr val=\"000000\"/></a:solidFill></a:ln><a:effectLst><a:outerShdw dist=\"12700\"><a:srgbClr val=\"000000\"/></a:outerShdw></a:effectLst><a:extLst><a:ext uri=\"child-keep\"/></a:extLst></xdr:spPr></xdr:cxnSp></xdr:grpSp><xdr:clientData/></xdr:oneCellAnchor>"
        );
        let edited = apply_shape_edit(
            &grouped,
            &json!({
                "fill":{"kind":"none"},
                "effects":{"shadow":{"enabled":false}}
            }),
        )
        .unwrap();
        assert!(edited.contains("<a:ext uri=\"group-keep\"/>"));
        assert!(edited.contains("<a:ext uri=\"child-keep\"/>"));
        assert!(edited.contains("<a:noFill/>"));
        assert!(!edited.contains("outerShdw"));
        assert_eq!(parse_shape_model(&edited)["geometry"], "line");
    }

    #[test]
    fn explicit_color_edit_drops_old_color_transforms_but_keeps_alpha() {
        let edited =
            apply_shape_edit(ANCHOR, &json!({"line":{"color":"#FF0000","alpha":0.4}})).unwrap();
        assert!(edited.contains("<a:srgbClr val=\"FF0000\"><a:alpha val=\"40000\"/></a:srgbClr>"));
        assert!(!edited.contains("<a:lumMod val=\"70000\"/>"));
        assert!(edited.contains("<a:headEnd type=\"triangle\"/>"));
        assert_eq!(parse_shape_model(&edited)["line"]["color"], "#FF0000");
        assert_eq!(parse_shape_model(&edited)["line"]["alpha"], 0.4);
    }
}
