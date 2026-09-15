//! Differential OOXML chart reader/editor.
//!
//! The implementation deliberately works on XML fragments instead of serialising a new
//! `c:chartSpace`.  Excel chart parts contain a large number of vendor extensions and style
//! details which a small chart model cannot represent.  Keeping the original fragments and
//! replacing only the requested children makes edits lossless for everything outside the
//! public model below.

use roxmltree::{Document, Node};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::ops::Range;

const CHART_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/chart";
const DRAWING_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";

const CHART_KINDS: &[&str] = &[
    "barChart",
    "lineChart",
    "pieChart",
    "pie3DChart",
    "doughnutChart",
    "areaChart",
    "scatterChart",
    "bubbleChart",
    "radarChart",
    "stockChart",
];

fn local_name<'a, 'input>(node: Node<'a, 'input>) -> &'a str {
    node.tag_name().name()
}

fn direct_child<'a, 'input>(node: Node<'a, 'input>, name: &str) -> Option<Node<'a, 'input>> {
    node.children()
        .find(|child| child.is_element() && local_name(*child) == name)
}

fn direct_children<'a, 'input>(
    node: Node<'a, 'input>,
    name: &'a str,
) -> impl Iterator<Item = Node<'a, 'input>> + 'a {
    node.children()
        .filter(move |child| child.is_element() && local_name(*child) == name)
}

fn attr_truthy(node: Node<'_, '_>, name: &str, default: bool) -> bool {
    match node.attribute(name) {
        Some("0") | Some("false") | Some("off") => false,
        Some("1") | Some("true") | Some("on") => true,
        Some(_) => default,
        None => default,
    }
}

fn chart_kind(local: &str) -> Option<&'static str> {
    match local {
        "barChart" => Some("bar"),
        "lineChart" => Some("line"),
        "pieChart" | "pie3DChart" => Some("pie"),
        "doughnutChart" => Some("doughnut"),
        "areaChart" => Some("area"),
        "scatterChart" => Some("scatter"),
        "bubbleChart" => Some("bubble"),
        "radarChart" => Some("radar"),
        "stockChart" => Some("stock"),
        _ => None,
    }
}

fn chart_local(kind: &str) -> Option<&'static str> {
    match normalize_chart_kind(kind).as_str() {
        "bar" => Some("barChart"),
        "line" => Some("lineChart"),
        "pie" => Some("pieChart"),
        "doughnut" => Some("doughnutChart"),
        "area" => Some("areaChart"),
        "scatter" => Some("scatterChart"),
        "bubble" => Some("bubbleChart"),
        "radar" => Some("radarChart"),
        "stock" => Some("stockChart"),
        _ => None,
    }
}

fn normalize_chart_kind(value: &str) -> String {
    let value = value.trim().to_ascii_lowercase();
    match value.as_str() {
        "barchart" | "column" | "columnchart" => "bar".into(),
        "linechart" => "line".into(),
        "piechart" | "pie3dchart" => "pie".into(),
        "doughnutchart" | "donut" => "doughnut".into(),
        "areachart" => "area".into(),
        "scatterchart" | "xy" => "scatter".into(),
        "bubblechart" => "bubble".into(),
        "radarchart" => "radar".into(),
        "stockchart" => "stock".into(),
        "combochart" | "combination" => "combo".into(),
        _ => value,
    }
}

fn text_content(node: Node<'_, '_>) -> String {
    let rich = node
        .descendants()
        .filter(|child| child.is_element() && local_name(*child) == "t")
        .filter_map(|child| child.text())
        .collect::<Vec<_>>();
    if !rich.is_empty() {
        return rich.join("");
    }
    node.descendants()
        .filter(|child| child.is_element() && local_name(*child) == "v")
        .filter_map(|child| child.text())
        .next()
        .unwrap_or("")
        .to_string()
}

fn formula_text(node: Option<Node<'_, '_>>) -> String {
    node.and_then(|container| {
        container
            .descendants()
            .find(|child| child.is_element() && local_name(*child) == "f")
            .and_then(|child| child.text())
    })
    .unwrap_or("")
    .to_string()
}

fn cache_node<'a, 'input>(node: Node<'a, 'input>) -> Option<Node<'a, 'input>> {
    node.descendants().find(|child| {
        child.is_element()
            && matches!(
                local_name(*child),
                "strCache" | "numCache" | "strLit" | "numLit" | "multiLvlStrCache"
            )
    })
}

fn cache_point_count(cache: Node<'_, '_>) -> usize {
    direct_child(cache, "ptCount")
        .and_then(|node| node.attribute("val"))
        .and_then(|value| value.parse::<usize>().ok())
        .or_else(|| {
            cache
                .descendants()
                .filter(|node| node.is_element() && local_name(*node) == "pt")
                .filter_map(|node| node.attribute("idx")?.parse::<usize>().ok())
                .max()
                .map(|idx| idx + 1)
        })
        .unwrap_or(0)
}

fn cache_strings(container: Option<Node<'_, '_>>) -> Vec<String> {
    let Some(container) = container else {
        return Vec::new();
    };
    let Some(cache) = cache_node(container) else {
        return Vec::new();
    };
    let cache = if local_name(cache) == "multiLvlStrCache" {
        direct_child(cache, "lvl").unwrap_or(cache)
    } else {
        cache
    };
    let mut points: Vec<(usize, String)> = cache
        .children()
        .filter(|node| node.is_element() && local_name(*node) == "pt")
        .filter_map(|point| {
            let index = point.attribute("idx")?.parse::<usize>().ok()?;
            let value = direct_child(point, "v")
                .and_then(|node| node.text())
                .unwrap_or("")
                .to_string();
            Some((index, value))
        })
        .collect();
    points.sort_by_key(|point| point.0);
    let count = cache_point_count(cache).max(points.last().map(|point| point.0 + 1).unwrap_or(0));
    let mut result = vec![String::new(); count];
    for (index, value) in points {
        if let Some(slot) = result.get_mut(index) {
            *slot = value;
        }
    }
    result
}

fn cache_numbers(container: Option<Node<'_, '_>>) -> Vec<Value> {
    let Some(container) = container else {
        return Vec::new();
    };
    let Some(cache) = cache_node(container) else {
        return Vec::new();
    };
    let mut points: Vec<(usize, Value)> = cache
        .children()
        .filter(|node| node.is_element() && local_name(*node) == "pt")
        .filter_map(|point| {
            let index = point.attribute("idx")?.parse::<usize>().ok()?;
            let raw = direct_child(point, "v").and_then(|node| node.text())?;
            Some((
                index,
                raw.parse::<f64>()
                    .ok()
                    .map(Value::from)
                    .unwrap_or(Value::Null),
            ))
        })
        .collect();
    points.sort_by_key(|point| point.0);
    let count = cache_point_count(cache).max(points.last().map(|point| point.0 + 1).unwrap_or(0));
    let mut result = vec![Value::Null; count];
    for (index, value) in points {
        if let Some(slot) = result.get_mut(index) {
            *slot = value;
        }
    }
    result
}

fn series_sources<'a, 'input>(
    series: Node<'a, 'input>,
    kind: &str,
) -> (Option<Node<'a, 'input>>, Option<Node<'a, 'input>>) {
    if matches!(kind, "scatter" | "bubble") {
        (direct_child(series, "xVal"), direct_child(series, "yVal"))
    } else {
        (direct_child(series, "cat"), direct_child(series, "val"))
    }
}

fn color_value(node: Node<'_, '_>) -> Option<String> {
    match local_name(node) {
        "srgbClr" => node
            .attribute("val")
            .filter(|value| value.len() == 6)
            .map(|value| format!("#{}", value.to_ascii_uppercase())),
        "schemeClr" => node.attribute("val").map(|value| format!("scheme:{value}")),
        "sysClr" => node
            .attribute("lastClr")
            .filter(|value| value.len() == 6)
            .map(|value| format!("#{}", value.to_ascii_uppercase()))
            .or_else(|| node.attribute("val").map(|value| format!("system:{value}"))),
        "prstClr" => node.attribute("val").map(|value| format!("preset:{value}")),
        "scrgbClr" => Some(format!(
            "scrgb({},{},{})",
            node.attribute("r").unwrap_or("0"),
            node.attribute("g").unwrap_or("0"),
            node.attribute("b").unwrap_or("0")
        )),
        "hslClr" => Some(format!(
            "hsl({},{},{})",
            node.attribute("hue").unwrap_or("0"),
            node.attribute("sat").unwrap_or("0"),
            node.attribute("lum").unwrap_or("0")
        )),
        _ => node.attribute("val").map(str::to_string),
    }
}

fn shape_color_node<'a, 'input>(
    shape: Node<'a, 'input>,
    line_like: bool,
) -> Option<Node<'a, 'input>> {
    let target = if line_like {
        direct_child(shape, "ln").unwrap_or(shape)
    } else {
        shape
    };
    for fill in target
        .children()
        .filter(|node| node.is_element() && local_name(*node) == "solidFill")
    {
        if let Some(color) = fill.children().find(|node| {
            node.is_element()
                && matches!(
                    local_name(*node),
                    "srgbClr" | "schemeClr" | "sysClr" | "prstClr" | "scrgbClr" | "hslClr"
                )
        }) {
            return Some(color);
        }
    }
    None
}

fn shape_color(shape: Node<'_, '_>, line_like: bool) -> String {
    shape_color_node(shape, line_like)
        .and_then(color_value)
        .unwrap_or_default()
}

fn shape_color_spec(shape: Node<'_, '_>, line_like: bool) -> Value {
    shape_color_node(shape, line_like)
        .map(crate::native_shape_edit::drawing_color_spec)
        .unwrap_or(Value::Null)
}

fn series_color(series: Node<'_, '_>, kind: &str) -> String {
    direct_child(series, "spPr")
        .map(|shape| {
            shape_color(
                shape,
                matches!(kind, "line" | "scatter" | "radar" | "stock"),
            )
        })
        .unwrap_or_default()
}

fn series_color_spec(series: Node<'_, '_>, kind: &str) -> Value {
    direct_child(series, "spPr")
        .map(|shape| {
            shape_color_spec(
                shape,
                matches!(kind, "line" | "scatter" | "radar" | "stock"),
            )
        })
        .unwrap_or(Value::Null)
}

fn source_binding_mode(container: Option<Node<'_, '_>>) -> &'static str {
    let Some(source) = container.and_then(|container| {
        container.children().find(|node| {
            node.is_element()
                && matches!(
                    local_name(*node),
                    "strRef" | "numRef" | "multiLvlStrRef" | "strLit" | "numLit"
                )
        })
    }) else {
        return "unknown";
    };
    match local_name(source) {
        "strRef" | "numRef" | "multiLvlStrRef" => "reference",
        "strLit" | "numLit" => "embedded",
        _ => "unknown",
    }
}

fn parse_point_override(point: Node<'_, '_>) -> Option<Value> {
    let index = direct_child(point, "idx")?
        .attribute("val")?
        .parse::<u64>()
        .ok()?;
    let explosion = direct_child(point, "explosion")
        .and_then(|node| node.attribute("val"))
        .and_then(|value| value.parse::<u64>().ok())
        .map(Value::from)
        .unwrap_or(Value::Null);
    let marker = direct_child(point, "marker");
    let marker_symbol = marker
        .and_then(|node| direct_child(node, "symbol"))
        .and_then(|node| node.attribute("val"))
        .unwrap_or("");
    let marker_size = marker
        .and_then(|node| direct_child(node, "size"))
        .and_then(|node| node.attribute("val"))
        .and_then(|value| value.parse::<u64>().ok())
        .map(Value::from)
        .unwrap_or(Value::Null);
    let color = direct_child(point, "spPr")
        .map(|shape| shape_color(shape, false))
        .unwrap_or_default();
    let color_spec = direct_child(point, "spPr")
        .map(|shape| shape_color_spec(shape, false))
        .unwrap_or(Value::Null);
    Some(json!({
        "index": index,
        "color": color,
        "colorSpec": color_spec,
        "explosion": explosion,
        "markerSymbol": marker_symbol,
        "markerSize": marker_size,
    }))
}

fn parse_point_overrides(series: Node<'_, '_>) -> Vec<Value> {
    direct_children(series, "dPt")
        .filter_map(parse_point_override)
        .collect()
}

fn child_val(node: Node<'_, '_>, name: &str) -> Option<String> {
    direct_child(node, name)
        .and_then(|child| child.attribute("val"))
        .map(str::to_string)
}

fn child_number(node: Node<'_, '_>, name: &str) -> Value {
    child_val(node, name)
        .and_then(|value| value.parse::<f64>().ok())
        .map(Value::from)
        .unwrap_or(Value::Null)
}

fn child_bool(node: Node<'_, '_>, name: &str) -> Value {
    direct_child(node, name)
        .map(|child| Value::Bool(attr_truthy(child, "val", true)))
        .unwrap_or(Value::Null)
}

fn data_label_position(value: &str) -> &str {
    match value {
        "ctr" => "center",
        "inBase" => "insideBase",
        "inEnd" => "insideEnd",
        "outEnd" => "outsideEnd",
        "bestFit" => "bestFit",
        "t" => "top",
        "b" => "bottom",
        "l" => "left",
        "r" => "right",
        _ => value,
    }
}

fn data_label_position_code(value: &str) -> Option<&str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "center" | "centre" | "ctr" => Some("ctr"),
        "insidebase" | "inside-base" | "inbase" => Some("inBase"),
        "insideend" | "inside-end" | "inend" => Some("inEnd"),
        "outsideend" | "outside-end" | "outend" => Some("outEnd"),
        "bestfit" | "best-fit" => Some("bestFit"),
        "top" | "t" => Some("t"),
        "bottom" | "b" => Some("b"),
        "left" | "l" => Some("l"),
        "right" | "r" => Some("r"),
        _ => None,
    }
}

fn parse_data_label_entry(label: Node<'_, '_>) -> Option<Value> {
    let index = direct_child(label, "idx")?
        .attribute("val")?
        .parse::<u64>()
        .ok()?;
    Some(json!({
        "index": index,
        "delete": child_bool(label, "delete"),
        "text": direct_child(label, "tx").map(text_content).unwrap_or_default(),
        "numberFormat": direct_child(label, "numFmt").map(|node| json!({
            "code": node.attribute("formatCode").unwrap_or(""),
            "sourceLinked": node.attribute("sourceLinked").map(|_| attr_truthy(node, "sourceLinked", true)).unwrap_or(true),
        })).unwrap_or(Value::Null),
        "separator": direct_child(label, "separator").and_then(|node| node.text()).unwrap_or(""),
        "position": child_val(label, "dLblPos").map(|value| data_label_position(&value).to_string()).unwrap_or_default(),
        "showLegendKey": child_bool(label, "showLegendKey"),
        "showValue": child_bool(label, "showVal"),
        "showCategoryName": child_bool(label, "showCatName"),
        "showSeriesName": child_bool(label, "showSerName"),
        "showPercent": child_bool(label, "showPercent"),
        "showBubbleSize": child_bool(label, "showBubbleSize"),
        "showLeaderLines": child_bool(label, "showLeaderLines"),
    }))
}

fn parse_data_labels(labels: Node<'_, '_>) -> Value {
    let number_format = direct_child(labels, "numFmt")
        .map(|node| json!({
            "code": node.attribute("formatCode").unwrap_or(""),
            "sourceLinked": node.attribute("sourceLinked").map(|_| attr_truthy(node, "sourceLinked", true)).unwrap_or(true),
        }))
        .unwrap_or(Value::Null);
    let entries: Vec<Value> = direct_children(labels, "dLbl")
        .filter_map(parse_data_label_entry)
        .collect();
    json!({
        "delete": child_bool(labels, "delete"),
        "position": child_val(labels, "dLblPos").map(|value| data_label_position(&value).to_string()).unwrap_or_default(),
        "numberFormat": number_format,
        "separator": direct_child(labels, "separator").and_then(|node| node.text()).unwrap_or(""),
        "showLegendKey": child_bool(labels, "showLegendKey"),
        "showValue": child_bool(labels, "showVal"),
        "showCategoryName": child_bool(labels, "showCatName"),
        "showSeriesName": child_bool(labels, "showSerName"),
        "showPercent": child_bool(labels, "showPercent"),
        "showBubbleSize": child_bool(labels, "showBubbleSize"),
        "showLeaderLines": child_bool(labels, "showLeaderLines"),
        "showDataLabelsRange": child_bool(labels, "showDataLabelsRange"),
        "labels": entries,
    })
}

fn parse_trendline(trendline: Node<'_, '_>, index: usize) -> Value {
    json!({
        "index": index,
        "name": direct_child(trendline, "name").and_then(|node| node.text()).unwrap_or(""),
        "type": child_val(trendline, "trendlineType").unwrap_or_else(|| "linear".to_string()),
        "order": child_number(trendline, "order"),
        "period": child_number(trendline, "period"),
        "forward": child_number(trendline, "forward"),
        "backward": child_number(trendline, "backward"),
        "intercept": child_number(trendline, "intercept"),
        "displayRSquared": child_bool(trendline, "dispRSqr"),
        "displayEquation": child_bool(trendline, "dispEq"),
        "label": direct_child(trendline, "trendlineLbl").map(text_content).unwrap_or_default(),
    })
}

fn parse_error_bar(error: Node<'_, '_>, index: usize) -> Value {
    let parse_amount = |name: &str| {
        direct_child(error, name)
            .map(|container| {
                json!({
                    "formula": formula_text(Some(container)),
                    "values": cache_numbers(Some(container)),
                    "bindingMode": source_binding_mode(Some(container)),
                })
            })
            .unwrap_or(Value::Null)
    };
    json!({
        "index": index,
        "direction": child_val(error, "errDir").unwrap_or_else(|| "y".to_string()),
        "barType": child_val(error, "errBarType").unwrap_or_else(|| "both".to_string()),
        "valueType": child_val(error, "errValType").unwrap_or_else(|| "fixedVal".to_string()),
        "noEndCap": child_bool(error, "noEndCap"),
        "value": child_number(error, "val"),
        "plus": parse_amount("plus"),
        "minus": parse_amount("minus"),
    })
}

fn parse_series(series: Node<'_, '_>, kind: &str) -> Value {
    let (category, value) = series_sources(series, kind);
    let category_binding = source_binding_mode(category);
    let value_binding = source_binding_mode(value);
    let binding_mode = if category_binding == value_binding {
        category_binding
    } else {
        "mixed"
    };
    let name = direct_child(series, "tx")
        .map(text_content)
        .unwrap_or_default();
    let data_labels = direct_child(series, "dLbls")
        .map(parse_data_labels)
        .unwrap_or(Value::Null);
    let trendlines: Vec<Value> = direct_children(series, "trendline")
        .enumerate()
        .map(|(index, trendline)| parse_trendline(trendline, index))
        .collect();
    let error_bars: Vec<Value> = direct_children(series, "errBars")
        .enumerate()
        .map(|(index, error)| parse_error_bar(error, index))
        .collect();
    json!({
        "name": name,
        "categoryFormula": formula_text(category),
        "valueFormula": formula_text(value),
        "categories": cache_strings(category),
        "values": cache_numbers(value),
        "bindingMode": binding_mode,
        "color": series_color(series, kind),
        "colorSpec": series_color_spec(series, kind),
        "pointOverrides": parse_point_overrides(series),
        "dataLabels": data_labels,
        "trendlines": trendlines,
        "errorBars": error_bars,
    })
}

fn legend_position(value: &str) -> &str {
    match value {
        "l" => "left",
        "t" => "top",
        "b" => "bottom",
        "tr" => "topRight",
        _ => "right",
    }
}

fn legend_position_code(value: &str) -> &str {
    match value.trim().to_ascii_lowercase().as_str() {
        "l" | "left" => "l",
        "t" | "top" => "t",
        "b" | "bottom" => "b",
        "tr" | "topright" | "top-right" => "tr",
        _ => "r",
    }
}

fn axis_type(local: &str) -> Option<&'static str> {
    match local {
        "catAx" => Some("category"),
        "valAx" => Some("value"),
        "dateAx" => Some("date"),
        "serAx" => Some("series"),
        _ => None,
    }
}

fn axis_local(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "category" | "cat" | "catax" => Some("catAx"),
        "value" | "val" | "valax" => Some("valAx"),
        "date" | "dateax" => Some("dateAx"),
        "series" | "ser" | "serax" => Some("serAx"),
        _ => None,
    }
}

fn axis_position(value: &str) -> &str {
    match value {
        "b" => "bottom",
        "t" => "top",
        "l" => "left",
        "r" => "right",
        _ => value,
    }
}

fn axis_position_code(value: &str) -> Option<&str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "bottom" | "b" => Some("b"),
        "top" | "t" => Some("t"),
        "left" | "l" => Some("l"),
        "right" | "r" => Some("r"),
        _ => None,
    }
}

fn parse_axis(axis: Node<'_, '_>) -> Value {
    let scaling = direct_child(axis, "scaling");
    let number_format = direct_child(axis, "numFmt")
        .map(|node| json!({
            "code": node.attribute("formatCode").unwrap_or(""),
            "sourceLinked": node.attribute("sourceLinked").map(|_| attr_truthy(node, "sourceLinked", true)).unwrap_or(true),
        }))
        .unwrap_or(Value::Null);
    let display_units = direct_child(axis, "dispUnits")
        .map(|node| {
            json!({
                "builtIn": child_val(node, "builtInUnit").unwrap_or_default(),
                "custom": child_number(node, "custUnit"),
                "showLabel": direct_child(node, "dispUnitsLbl").is_some(),
                "label": direct_child(node, "dispUnitsLbl").map(text_content).unwrap_or_default(),
            })
        })
        .unwrap_or(Value::Null);
    json!({
        "id": child_val(axis, "axId").and_then(|value| value.parse::<u64>().ok()).map(Value::from).unwrap_or(Value::Null),
        "axisType": axis_type(local_name(axis)).unwrap_or("unknown"),
        "position": child_val(axis, "axPos").map(|value| axis_position(&value).to_string()).unwrap_or_default(),
        "delete": child_bool(axis, "delete"),
        "title": direct_child(axis, "title").map(text_content).unwrap_or_default(),
        "scaling": {
            "logBase": scaling.map(|node| child_number(node, "logBase")).unwrap_or(Value::Null),
            "orientation": scaling.and_then(|node| child_val(node, "orientation")).unwrap_or_default(),
            "min": scaling.map(|node| child_number(node, "min")).unwrap_or(Value::Null),
            "max": scaling.map(|node| child_number(node, "max")).unwrap_or(Value::Null),
        },
        "numberFormat": number_format,
        "majorGridlines": direct_child(axis, "majorGridlines").is_some(),
        "minorGridlines": direct_child(axis, "minorGridlines").is_some(),
        "majorTickMark": child_val(axis, "majorTickMark").unwrap_or_default(),
        "minorTickMark": child_val(axis, "minorTickMark").unwrap_or_default(),
        "tickLabelPosition": child_val(axis, "tickLblPos").unwrap_or_default(),
        "crossAxisId": child_val(axis, "crossAx").and_then(|value| value.parse::<u64>().ok()).map(Value::from).unwrap_or(Value::Null),
        "crosses": child_val(axis, "crosses").unwrap_or_default(),
        "crossesAt": child_number(axis, "crossesAt"),
        "crossBetween": child_val(axis, "crossBetween").unwrap_or_default(),
        "auto": child_bool(axis, "auto"),
        "labelAlignment": child_val(axis, "lblAlgn").unwrap_or_default(),
        "labelOffset": child_number(axis, "lblOffset"),
        "tickLabelSkip": child_number(axis, "tickLblSkip"),
        "tickMarkSkip": child_number(axis, "tickMarkSkip"),
        "noMultiLevelLabels": child_bool(axis, "noMultiLvlLbl"),
        "majorUnit": child_number(axis, "majorUnit"),
        "minorUnit": child_number(axis, "minorUnit"),
        "baseTimeUnit": child_val(axis, "baseTimeUnit").unwrap_or_default(),
        "majorTimeUnit": child_val(axis, "majorTimeUnit").unwrap_or_default(),
        "minorTimeUnit": child_val(axis, "minorTimeUnit").unwrap_or_default(),
        "displayUnits": display_units,
    })
}

fn plot_axis_ids(plot: Node<'_, '_>) -> Vec<Value> {
    direct_children(plot, "axId")
        .filter_map(|node| node.attribute("val")?.parse::<u64>().ok())
        .map(Value::from)
        .collect()
}

fn inferred_axis_group(axis_ids: &[Value], axes: &[Value]) -> &'static str {
    for id in axis_ids.iter().filter_map(Value::as_u64) {
        let position = axes
            .iter()
            .find(|axis| axis.get("id").and_then(Value::as_u64) == Some(id))
            .and_then(|axis| axis.get("position"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if matches!(position, "top" | "right") {
            return "secondary";
        }
    }
    "primary"
}

/// Parse the editable subset of an OOXML chart part.
///
/// Invalid or unsupported XML is represented by a stable empty model with an `error` field;
/// callers can still keep and round-trip the original chart part.
pub fn parse_chart_model(chart_xml: &str) -> Value {
    let Ok(document) = Document::parse(chart_xml) else {
        return json!({
            "chartType":"unknown", "title":"",
            "legend":{"show":false,"position":"right"}, "series":[],
            "error":"invalid chart XML"
        });
    };
    let Some(chart) = document
        .descendants()
        .find(|node| node.is_element() && local_name(*node) == "chart")
    else {
        return json!({
            "chartType":"unknown", "title":"",
            "legend":{"show":false,"position":"right"}, "series":[],
            "error":"missing chart element"
        });
    };
    let Some(plot_area) = direct_child(chart, "plotArea") else {
        return json!({
            "chartType":"unknown", "title":"",
            "legend":{"show":false,"position":"right"}, "series":[],
            "error":"missing plotArea"
        });
    };
    let chart_nodes: Vec<Node<'_, '_>> = plot_area
        .children()
        .filter(|node| node.is_element() && CHART_KINDS.contains(&local_name(*node)))
        .collect();
    let kind = if chart_nodes.len() > 1 {
        "combo".to_string()
    } else {
        chart_nodes
            .first()
            .and_then(|node| chart_kind(local_name(*node)))
            .unwrap_or("unknown")
            .to_string()
    };
    let title = direct_child(chart, "title")
        .map(text_content)
        .unwrap_or_default();
    let legend = direct_child(chart, "legend");
    let show_legend = legend
        .map(|legend| {
            !direct_child(legend, "delete")
                .map(|node| attr_truthy(node, "val", true))
                .unwrap_or(false)
        })
        .unwrap_or(false);
    let position = legend
        .and_then(|legend| direct_child(legend, "legendPos"))
        .and_then(|node| node.attribute("val"))
        .map(legend_position)
        .unwrap_or("right");
    let axes: Vec<Value> = plot_area
        .children()
        .filter(|node| node.is_element() && axis_type(local_name(*node)).is_some())
        .map(parse_axis)
        .collect();
    let mut plots = Vec::new();
    let mut series = Vec::new();
    for (plot_index, chart_node) in chart_nodes.into_iter().enumerate() {
        let node_kind = chart_kind(local_name(chart_node)).unwrap_or("unknown");
        let axis_ids = plot_axis_ids(chart_node);
        let axis_group = inferred_axis_group(&axis_ids, &axes);
        let labels = direct_child(chart_node, "dLbls")
            .map(parse_data_labels)
            .unwrap_or(Value::Null);
        plots.push(json!({
            "index": plot_index,
            "chartType": node_kind,
            "axisIds": axis_ids.clone(),
            "axisGroup": axis_group,
            "dataLabels": labels,
            "grouping": child_val(chart_node, "grouping").unwrap_or_default(),
            "barDirection": child_val(chart_node, "barDir").unwrap_or_default(),
            "gapWidth": child_number(chart_node, "gapWidth"),
            "overlap": child_number(chart_node, "overlap"),
            "smooth": child_bool(chart_node, "smooth"),
            "varyColors": child_bool(chart_node, "varyColors"),
        }));
        for item in direct_children(chart_node, "ser") {
            let mut parsed = parse_series(item, node_kind);
            if let Some(object) = parsed.as_object_mut() {
                object.insert("plotIndex".into(), Value::from(plot_index));
                object.insert("plotType".into(), Value::String(node_kind.to_string()));
                object.insert("axisIds".into(), Value::Array(axis_ids.clone()));
                object.insert("axisGroup".into(), Value::String(axis_group.to_string()));
            }
            series.push(parsed);
        }
    }
    json!({
        "chartType": kind,
        "title": title,
        "legend": {"show":show_legend,"position":position},
        "plots": plots,
        "axes": axes,
        "series": series,
    })
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn qname_prefix(xml: &str, node: Node<'_, '_>) -> String {
    let start = node.range().start;
    let raw = &xml[start + 1..];
    let end = raw
        .find(|ch: char| ch.is_ascii_whitespace() || ch == '>' || ch == '/')
        .unwrap_or(raw.len());
    let name = &raw[..end];
    name.rsplit_once(':')
        .map(|(prefix, _)| format!("{prefix}:"))
        .unwrap_or_default()
}

fn root_namespace_context(xml: &str) -> String {
    let Ok(document) = Document::parse(xml) else {
        return format!("xmlns:c=\"{CHART_NS}\" xmlns:a=\"{DRAWING_NS}\"");
    };
    let root = document.root_element();
    let start = root.range().start;
    let Some(end) = xml[start..].find('>').map(|offset| start + offset) else {
        return format!("xmlns:c=\"{CHART_NS}\" xmlns:a=\"{DRAWING_NS}\"");
    };
    let tag = &xml[start..=end];
    let mut declarations = Vec::new();
    let mut offset = 0usize;
    while let Some(relative) = tag[offset..].find(" xmlns") {
        let begin = offset + relative + 1;
        let Some(eq_relative) = tag[begin..].find('=') else {
            break;
        };
        let eq = begin + eq_relative;
        let name = tag[begin..eq].trim();
        if !(name == "xmlns" || name.starts_with("xmlns:")) {
            offset = eq + 1;
            continue;
        }
        let rest = &tag[eq + 1..];
        let Some(quote_relative) = rest.find(['"', '\'']) else {
            break;
        };
        let quote_at = eq + 1 + quote_relative;
        let quote = tag.as_bytes()[quote_at] as char;
        let Some(close_relative) = tag[quote_at + 1..].find(quote) else {
            break;
        };
        let close = quote_at + 1 + close_relative + 1;
        declarations.push(tag[begin..close].to_string());
        offset = close;
    }
    if !declarations.iter().any(|decl| decl.starts_with("xmlns:c="))
        && !declarations
            .iter()
            .any(|decl| decl == &format!("xmlns=\"{CHART_NS}\""))
    {
        declarations.push(format!("xmlns:c=\"{CHART_NS}\""));
    }
    if !declarations.iter().any(|decl| decl.starts_with("xmlns:a=")) {
        declarations.push(format!("xmlns:a=\"{DRAWING_NS}\""));
    }
    declarations.join(" ")
}

fn set_start_tag_attribute(fragment: &str, name: &str, value: &str) -> String {
    let Some(tag_end) = fragment.find('>') else {
        return fragment.to_string();
    };
    let mut output = fragment.to_string();
    for quote in ['"', '\''] {
        let needle = format!(" {name}={quote}");
        if let Some(start) = output[..tag_end].find(&needle) {
            let value_start = start + needle.len();
            if let Some(relative_end) = output[value_start..tag_end].find(quote) {
                output.replace_range(value_start..value_start + relative_end, &xml_escape(value));
                return output;
            }
        }
    }
    let insert = if tag_end > 0 && output.as_bytes()[tag_end - 1] == b'/' {
        tag_end - 1
    } else {
        tag_end
    };
    output.insert_str(insert, &format!(" {name}=\"{}\"", xml_escape(value)));
    output
}

fn wrapped_document(fragment: &str, namespaces: &str) -> Result<(String, usize), String> {
    let open = format!("<unicellRoot {namespaces}>");
    let xml = format!("{open}{fragment}</unicellRoot>");
    Document::parse(&xml).map_err(|error| format!("chart fragment XML: {error}"))?;
    Ok((xml, open.len()))
}

fn fragment_root<'a, 'input>(document: &'a Document<'input>) -> Option<Node<'a, 'input>> {
    document
        .root_element()
        .children()
        .find(|node| node.is_element())
}

fn root_child_ranges(
    fragment: &str,
    namespaces: &str,
) -> Result<Vec<(String, Range<usize>)>, String> {
    let (wrapped, offset) = wrapped_document(fragment, namespaces)?;
    let document = Document::parse(&wrapped).map_err(|error| error.to_string())?;
    let root = fragment_root(&document).ok_or("missing fragment root")?;
    Ok(root
        .children()
        .filter(|node| node.is_element())
        .map(|node| {
            let range = node.range();
            (
                local_name(node).to_string(),
                range.start - offset..range.end - offset,
            )
        })
        .collect())
}

fn replace_root_child(
    fragment: &str,
    namespaces: &str,
    name: &str,
    replacement: Option<&str>,
    order: &[&str],
) -> Result<String, String> {
    let children = root_child_ranges(fragment, namespaces)?;
    if let Some((_, range)) = children.iter().find(|(local, _)| local == name) {
        let mut output = fragment.to_string();
        output.replace_range(range.clone(), replacement.unwrap_or(""));
        return Ok(output);
    }
    let Some(replacement) = replacement else {
        return Ok(fragment.to_string());
    };
    let desired_order = order
        .iter()
        .position(|candidate| *candidate == name)
        .unwrap_or(usize::MAX - 1);
    let insert = children
        .iter()
        .find(|(local, _)| {
            order
                .iter()
                .position(|candidate| *candidate == local)
                .unwrap_or(usize::MAX)
                > desired_order
        })
        .map(|(_, range)| range.start)
        .unwrap_or_else(|| fragment.rfind("</").unwrap_or(fragment.len()));
    let mut output = fragment.to_string();
    output.insert_str(insert, replacement);
    Ok(output)
}

fn rename_root_element(fragment: &str, new_local: &str) -> String {
    let Some(open_end) =
        fragment.find(|ch: char| ch.is_ascii_whitespace() || ch == '>' || ch == '/')
    else {
        return fragment.to_string();
    };
    if !fragment.starts_with('<') {
        return fragment.to_string();
    }
    let old_qname = &fragment[1..open_end];
    let prefix = old_qname
        .rsplit_once(':')
        .map(|(prefix, _)| format!("{prefix}:"))
        .unwrap_or_default();
    let new_qname = format!("{prefix}{new_local}");
    let mut output = fragment.to_string();
    output.replace_range(1..open_end, &new_qname);
    let old_close = format!("</{old_qname}>");
    if let Some(close) = output.rfind(&old_close) {
        output.replace_range(close + 2..close + 2 + old_qname.len(), &new_qname);
    }
    output
}

fn rename_root_child(
    fragment: &str,
    namespaces: &str,
    old: &str,
    new: &str,
) -> Result<String, String> {
    let children = root_child_ranges(fragment, namespaces)?;
    let Some((_, range)) = children.iter().find(|(name, _)| name == old) else {
        return Ok(fragment.to_string());
    };
    let replacement = rename_root_element(&fragment[range.clone()], new);
    let mut output = fragment.to_string();
    output.replace_range(range.clone(), &replacement);
    Ok(output)
}

fn replace_xml_range(xml: &mut String, range: Range<usize>, replacement: &str) {
    xml.replace_range(range, replacement);
}

fn apply_title(chart_xml: &mut String, title_edit: Option<&Value>) -> Result<(), String> {
    let Some(title) = title_edit.and_then(Value::as_str) else {
        return Ok(());
    };
    let document = Document::parse(chart_xml).map_err(|error| format!("chart XML: {error}"))?;
    let chart = document
        .descendants()
        .find(|node| node.is_element() && local_name(*node) == "chart")
        .ok_or("missing chart element")?;
    let prefix = qname_prefix(chart_xml, chart);
    let existing = direct_child(chart, "title");
    if existing.map(text_content).unwrap_or_default() == title {
        return Ok(());
    }
    if title.is_empty() {
        if let Some(existing) = existing {
            replace_xml_range(chart_xml, existing.range(), "");
        }
        return Ok(());
    }
    let tx = format!(
        "<{p}tx><{p}rich><a:bodyPr xmlns:a=\"{a}\"/><a:lstStyle xmlns:a=\"{a}\"/><a:p xmlns:a=\"{a}\"><a:r><a:t>{title}</a:t></a:r></a:p></{p}rich></{p}tx>",
        p = prefix,
        a = DRAWING_NS,
        title = xml_escape(title)
    );
    if let Some(existing) = existing {
        if let Some(existing_tx) = direct_child(existing, "tx") {
            replace_xml_range(chart_xml, existing_tx.range(), &tx);
        } else {
            let range = existing.range();
            let raw = &chart_xml[range.clone()];
            let insert = raw.find('>').ok_or("malformed chart title")? + 1;
            let mut replacement = raw.to_string();
            replacement.insert_str(insert, &tx);
            replace_xml_range(chart_xml, range, &replacement);
        }
    } else {
        let title_xml = format!("<{p}title>{tx}</{p}title>", p = prefix);
        let insert = direct_child(chart, "autoTitleDeleted")
            .or_else(|| direct_child(chart, "pivotFmts"))
            .or_else(|| direct_child(chart, "view3D"))
            .or_else(|| direct_child(chart, "floor"))
            .or_else(|| direct_child(chart, "sideWall"))
            .or_else(|| direct_child(chart, "backWall"))
            .or_else(|| direct_child(chart, "plotArea"))
            .map(|node| node.range().start)
            .unwrap_or_else(|| {
                chart_xml[chart.range()]
                    .rfind("</")
                    .unwrap_or(chart.range().end)
            });
        chart_xml.insert_str(insert, &title_xml);
    }
    Ok(())
}

fn apply_legend(chart_xml: &mut String, legend_edit: Option<&Value>) -> Result<(), String> {
    let Some(edit) = legend_edit.and_then(Value::as_object) else {
        return Ok(());
    };
    if !edit.contains_key("show") && !edit.contains_key("position") {
        return Ok(());
    }
    let document = Document::parse(chart_xml).map_err(|error| format!("chart XML: {error}"))?;
    let chart = document
        .descendants()
        .find(|node| node.is_element() && local_name(*node) == "chart")
        .ok_or("missing chart element")?;
    let existing = direct_child(chart, "legend");
    let show_edit = edit.get("show").and_then(Value::as_bool);
    if show_edit == Some(false) {
        if let Some(existing) = existing {
            replace_xml_range(chart_xml, existing.range(), "");
        }
        return Ok(());
    }
    let requested_position = edit
        .get("position")
        .and_then(Value::as_str)
        .map(legend_position_code);
    let prefix = qname_prefix(chart_xml, chart);
    if let Some(existing) = existing {
        let range = existing.range();
        let raw = &chart_xml[range.clone()];
        let namespaces = root_namespace_context(chart_xml);
        let order = [
            "legendPos",
            "legendEntry",
            "layout",
            "overlay",
            "spPr",
            "txPr",
            "extLst",
        ];
        let mut replacement = raw.to_string();
        if let Some(position) = requested_position {
            let position_xml = format!("<{p}legendPos val=\"{position}\"/>", p = prefix);
            replacement = replace_root_child(
                &replacement,
                &namespaces,
                "legendPos",
                Some(&position_xml),
                &order,
            )?;
        }
        if show_edit == Some(true) {
            replacement = replace_root_child(&replacement, &namespaces, "delete", None, &order)?;
        }
        replace_xml_range(chart_xml, range, &replacement);
    } else {
        let position = requested_position.unwrap_or("r");
        let legend = format!(
            "<{p}legend><{p}legendPos val=\"{position}\"/><{p}layout/></{p}legend>",
            p = prefix
        );
        let insert = direct_child(chart, "plotVisOnly")
            .or_else(|| direct_child(chart, "dispBlanksAs"))
            .or_else(|| direct_child(chart, "showDLblsOverMax"))
            .or_else(|| direct_child(chart, "extLst"))
            .map(|node| node.range().start)
            .unwrap_or_else(|| {
                let range = chart.range();
                chart_xml[range.clone()]
                    .rfind("</")
                    .map(|offset| range.start + offset)
                    .unwrap_or(range.end)
            });
        chart_xml.insert_str(insert, &legend);
    }
    Ok(())
}

fn series_order(name: &str) -> usize {
    const ORDER: &[&str] = &[
        "idx",
        "order",
        "tx",
        "spPr",
        "invertIfNegative",
        "marker",
        "dPt",
        "dLbls",
        "trendline",
        "errBars",
        "cat",
        "xVal",
        "val",
        "yVal",
        "bubbleSize",
        "shape",
        "smooth",
        "extLst",
    ];
    ORDER
        .iter()
        .position(|candidate| *candidate == name)
        .unwrap_or(usize::MAX - 1)
}

fn series_order_names() -> &'static [&'static str] {
    &[
        "idx",
        "order",
        "tx",
        "spPr",
        "invertIfNegative",
        "marker",
        "dPt",
        "dLbls",
        "trendline",
        "errBars",
        "cat",
        "xVal",
        "val",
        "yVal",
        "bubbleSize",
        "shape",
        "smooth",
        "extLst",
    ]
}

fn set_series_index(
    fragment: &str,
    namespaces: &str,
    prefix: &str,
    name: &str,
    index: usize,
) -> Result<String, String> {
    let children = root_child_ranges(fragment, namespaces)?;
    if let Some((_, range)) = children.iter().find(|(local, _)| local == name) {
        let replacement =
            set_start_tag_attribute(&fragment[range.clone()], "val", &index.to_string());
        let mut output = fragment.to_string();
        output.replace_range(range.clone(), &replacement);
        return Ok(output);
    }
    let child = format!("<{prefix}{name} val=\"{index}\"/>");
    replace_root_child(
        fragment,
        namespaces,
        name,
        Some(&child),
        series_order_names(),
    )
}

fn cache_points_xml(prefix: &str, points: &[Value], numeric: bool) -> String {
    let mut output = format!("<{prefix}ptCount val=\"{}\"/>", points.len());
    for (index, value) in points.iter().enumerate() {
        let serialized = if numeric {
            value.as_f64().map(|number| number.to_string())
        } else {
            value.as_str().map(xml_escape)
        };
        if let Some(serialized) = serialized {
            output.push_str(&format!(
                "<{prefix}pt idx=\"{index}\"><{prefix}v>{serialized}</{prefix}v></{prefix}pt>"
            ));
        }
    }
    output
}

fn update_cache_fragment(
    cache: &str,
    namespaces: &str,
    prefix: &str,
    points: &[Value],
    numeric: bool,
) -> Result<String, String> {
    let mut output = cache.to_string();
    let mut ranges: Vec<Range<usize>> = root_child_ranges(cache, namespaces)?
        .into_iter()
        .filter(|(name, _)| name == "ptCount" || name == "pt")
        .map(|(_, range)| range)
        .collect();
    ranges.sort_by(|left, right| right.start.cmp(&left.start));
    for range in ranges {
        output.replace_range(range, "");
    }
    let children = root_child_ranges(&output, namespaces)?;
    let insert = children
        .iter()
        .find(|(name, _)| name == "extLst")
        .map(|(_, range)| range.start)
        .unwrap_or_else(|| output.rfind("</").unwrap_or(output.len()));
    output.insert_str(insert, &cache_points_xml(prefix, points, numeric));
    Ok(output)
}

fn source_kind_for(numeric: bool, formula: &str) -> &'static str {
    match (numeric, formula.is_empty()) {
        (true, false) => "numRef",
        (true, true) => "numLit",
        (false, false) => "strRef",
        (false, true) => "strLit",
    }
}

fn cache_kind_for(numeric: bool) -> &'static str {
    if numeric { "numCache" } else { "strCache" }
}

fn build_source_xml(prefix: &str, formula: &str, points: &[Value], numeric: bool) -> String {
    let source_kind = source_kind_for(numeric, formula);
    if formula.is_empty() {
        return format!(
            "<{prefix}{source_kind}>{}</{prefix}{source_kind}>",
            cache_points_xml(prefix, points, numeric)
        );
    }
    let cache_kind = cache_kind_for(numeric);
    format!(
        "<{prefix}{source_kind}><{prefix}f>{}</{prefix}f><{prefix}{cache_kind}>{}</{prefix}{cache_kind}></{prefix}{source_kind}>",
        xml_escape(formula),
        cache_points_xml(prefix, points, numeric)
    )
}

fn update_source_fragment(
    source: &str,
    namespaces: &str,
    prefix: &str,
    formula: &str,
    points: &[Value],
    numeric: bool,
) -> Result<String, String> {
    let (wrapped, offset) = wrapped_document(source, namespaces)?;
    let document = Document::parse(&wrapped).map_err(|error| error.to_string())?;
    let root = fragment_root(&document).ok_or("missing data source root")?;
    let current_kind = local_name(root);
    let wanted_kind = source_kind_for(numeric, formula);
    if current_kind != wanted_kind {
        return Ok(build_source_xml(prefix, formula, points, numeric));
    }
    let mut output = source.to_string();
    if !formula.is_empty() {
        let formula_node = direct_child(root, "f");
        if let Some(formula_node) = formula_node {
            let range = formula_node.range();
            let local_range = range.start - offset..range.end - offset;
            output.replace_range(
                local_range,
                &format!("<{prefix}f>{}</{prefix}f>", xml_escape(formula)),
            );
        } else {
            let insert = output.find('>').ok_or("malformed data source")? + 1;
            output.insert_str(
                insert,
                &format!("<{prefix}f>{}</{prefix}f>", xml_escape(formula)),
            );
        }
        let (wrapped, offset) = wrapped_document(&output, namespaces)?;
        let document = Document::parse(&wrapped).map_err(|error| error.to_string())?;
        let root = fragment_root(&document).ok_or("missing data source root")?;
        let cache = root
            .children()
            .find(|node| node.is_element() && local_name(*node) == cache_kind_for(numeric));
        if let Some(cache) = cache {
            let range = cache.range();
            let local_range = range.start - offset..range.end - offset;
            let replacement = update_cache_fragment(
                &output[local_range.clone()],
                namespaces,
                prefix,
                points,
                numeric,
            )?;
            output.replace_range(local_range, &replacement);
        } else {
            let insert = output.rfind("</").unwrap_or(output.len());
            output.insert_str(
                insert,
                &format!(
                    "<{prefix}{cache}>{}</{prefix}{cache}>",
                    cache_points_xml(prefix, points, numeric),
                    cache = cache_kind_for(numeric)
                ),
            );
        }
    } else {
        output = update_cache_fragment(&output, namespaces, prefix, points, numeric)?;
    }
    Ok(output)
}

fn update_data_container(
    series: &str,
    namespaces: &str,
    prefix: &str,
    container_name: &str,
    formula: &str,
    points: &[Value],
    numeric: bool,
) -> Result<String, String> {
    let children = root_child_ranges(series, namespaces)?;
    let container = children
        .iter()
        .find(|(name, _)| name == container_name)
        .map(|(_, range)| range.clone());
    let source_names = ["strRef", "numRef", "strLit", "numLit", "multiLvlStrRef"];
    let container_xml = if let Some(range) = &container {
        let raw = &series[range.clone()];
        let source = root_child_ranges(raw, namespaces)?
            .into_iter()
            .find(|(name, _)| source_names.contains(&name.as_str()));
        if let Some((_, source_range)) = source {
            let replacement = update_source_fragment(
                &raw[source_range.clone()],
                namespaces,
                prefix,
                formula,
                points,
                numeric,
            )?;
            let mut output = raw.to_string();
            output.replace_range(source_range, &replacement);
            output
        } else {
            let mut output = raw.to_string();
            let insert = output.find('>').ok_or("malformed data container")? + 1;
            output.insert_str(insert, &build_source_xml(prefix, formula, points, numeric));
            output
        }
    } else {
        format!(
            "<{prefix}{container_name}>{}</{prefix}{container_name}>",
            build_source_xml(prefix, formula, points, numeric)
        )
    };
    replace_root_child(
        series,
        namespaces,
        container_name,
        Some(&container_xml),
        series_order_names(),
    )
}

fn json_string_points(value: Option<&Value>, fallback: &[String]) -> Vec<Value> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    Value::String(
                        item.as_str()
                            .map(str::to_string)
                            .or_else(|| item.as_f64().map(|number| number.to_string()))
                            .unwrap_or_default(),
                    )
                })
                .collect()
        })
        .unwrap_or_else(|| fallback.iter().cloned().map(Value::String).collect())
}

fn json_number_points(value: Option<&Value>, fallback: &[Value]) -> Vec<Value> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| item.as_f64().map(Value::from).unwrap_or(Value::Null))
                .collect()
        })
        .unwrap_or_else(|| fallback.to_vec())
}

fn numeric_category_points(points: Vec<Value>) -> Vec<Value> {
    points
        .into_iter()
        .map(|point| {
            point
                .as_f64()
                .or_else(|| point.as_str().and_then(|value| value.parse::<f64>().ok()))
                .map(Value::from)
                .unwrap_or(Value::Null)
        })
        .collect()
}

fn data_container_is_numeric(
    series: &str,
    namespaces: &str,
    container_name: &str,
) -> Result<bool, String> {
    let (wrapped, _) = wrapped_document(series, namespaces)?;
    let document = Document::parse(&wrapped).map_err(|error| error.to_string())?;
    let root = fragment_root(&document).ok_or("missing series root")?;
    Ok(direct_child(root, container_name)
        .and_then(|container| {
            container
                .descendants()
                .find(|node| node.is_element() && matches!(local_name(*node), "numRef" | "numLit"))
        })
        .is_some())
}

fn normalize_series_sources(
    mut series: String,
    namespaces: &str,
    target_kind: &str,
) -> Result<String, String> {
    if matches!(target_kind, "scatter" | "bubble") {
        series = rename_root_child(&series, namespaces, "cat", "xVal")?;
        series = rename_root_child(&series, namespaces, "val", "yVal")?;
    } else {
        series = rename_root_child(&series, namespaces, "xVal", "cat")?;
        series = rename_root_child(&series, namespaces, "yVal", "val")?;
    }
    if target_kind != "bubble" {
        series = replace_root_child(
            &series,
            namespaces,
            "bubbleSize",
            None,
            series_order_names(),
        )?;
    }
    Ok(series)
}

fn replace_series_name(
    series: &str,
    namespaces: &str,
    prefix: &str,
    name: &str,
) -> Result<String, String> {
    let tx = format!(
        "<{prefix}tx><{prefix}v>{}</{prefix}v></{prefix}tx>",
        xml_escape(name)
    );
    replace_root_child(series, namespaces, "tx", Some(&tx), series_order_names())
}

fn desired_color_node(color: &str) -> Option<(&'static str, String)> {
    let value = color.trim();
    if let Some(value) = value.strip_prefix('#') {
        if value.len() == 6 && value.chars().all(|ch| ch.is_ascii_hexdigit()) {
            return Some(("srgbClr", value.to_ascii_uppercase()));
        }
    }
    if value.len() == 6 && value.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Some(("srgbClr", value.to_ascii_uppercase()));
    }
    value
        .strip_prefix("scheme:")
        .map(|value| ("schemeClr", value.to_string()))
        .or_else(|| {
            value
                .strip_prefix("preset:")
                .map(|value| ("prstClr", value.to_string()))
        })
}

fn apply_series_color(
    series: &str,
    namespaces: &str,
    chart_prefix: &str,
    color: &str,
    line_like: bool,
) -> Result<String, String> {
    let Some((wanted_kind, wanted_value)) = desired_color_node(color) else {
        return Ok(series.to_string());
    };
    let (wrapped, offset) = wrapped_document(series, namespaces)?;
    let document = Document::parse(&wrapped).map_err(|error| error.to_string())?;
    let root = fragment_root(&document).ok_or("missing series root")?;
    let shape = direct_child(root, "spPr");
    let color_container = shape.and_then(|shape| {
        if line_like {
            direct_child(shape, "ln")
        } else {
            Some(shape)
        }
    });
    if let Some(color_node) = color_container.and_then(|container| {
        direct_child(container, "solidFill").and_then(|fill| {
            fill.children().find(|node| {
                node.is_element()
                    && matches!(
                        local_name(*node),
                        "srgbClr" | "schemeClr" | "sysClr" | "prstClr" | "scrgbClr" | "hslClr"
                    )
            })
        })
    }) {
        let range = color_node.range();
        let local_range = range.start - offset..range.end - offset;
        // A newly selected series colour represents the exact requested base colour.  Old
        // tint/shade/luminance transforms belong to the previous theme colour and must not
        // leak into the replacement; untouched colours never reach this function.
        let replacement = format!(
            "<a:{wanted_kind} xmlns:a=\"{DRAWING_NS}\" val=\"{}\"/>",
            xml_escape(&wanted_value),
        );
        let mut output = series.to_string();
        output.replace_range(local_range, &replacement);
        return Ok(output);
    }
    let a_fill = format!(
        "<a:solidFill xmlns:a=\"{DRAWING_NS}\"><a:{wanted_kind} val=\"{}\"/></a:solidFill>",
        xml_escape(&wanted_value)
    );
    if let Some(container) = color_container {
        let range = container.range();
        let local_range = range.start - offset..range.end - offset;
        let mut raw = series[local_range.clone()].to_string();
        let insert = raw.find('>').ok_or("malformed series colour container")? + 1;
        raw.insert_str(insert, &a_fill);
        let mut output = series.to_string();
        output.replace_range(local_range, &raw);
        return Ok(output);
    }
    if let Some(shape) = shape {
        let range = shape.range();
        let local_range = range.start - offset..range.end - offset;
        let mut raw = series[local_range.clone()].to_string();
        if line_like {
            let insert = raw.rfind("</").unwrap_or(raw.len());
            raw.insert_str(
                insert,
                &format!("<a:ln xmlns:a=\"{DRAWING_NS}\">{a_fill}</a:ln>"),
            );
        } else {
            let insert = raw.find('>').ok_or("malformed spPr")? + 1;
            raw.insert_str(insert, &a_fill);
        }
        let mut output = series.to_string();
        output.replace_range(local_range, &raw);
        Ok(output)
    } else {
        let content = if line_like {
            format!("<a:ln xmlns:a=\"{DRAWING_NS}\">{a_fill}</a:ln>")
        } else {
            a_fill
        };
        let shape = format!("<{chart_prefix}spPr>{content}</{chart_prefix}spPr>");
        replace_root_child(
            series,
            namespaces,
            "spPr",
            Some(&shape),
            series_order_names(),
        )
    }
}

fn point_order_names() -> &'static [&'static str] {
    &[
        "idx",
        "invertIfNegative",
        "marker",
        "bubble3D",
        "explosion",
        "spPr",
        "pictureOptions",
        "extLst",
    ]
}

fn marker_order_names() -> &'static [&'static str] {
    &["symbol", "size", "spPr", "extLst"]
}

fn set_value_child(
    fragment: &str,
    namespaces: &str,
    prefix: &str,
    name: &str,
    value: Option<&str>,
    order: &[&str],
) -> Result<String, String> {
    let children = root_child_ranges(fragment, namespaces)?;
    if let Some((_, range)) = children.iter().find(|(local, _)| local == name) {
        if let Some(value) = value {
            let replacement = set_start_tag_attribute(&fragment[range.clone()], "val", value);
            let mut output = fragment.to_string();
            output.replace_range(range.clone(), &replacement);
            return Ok(output);
        }
        return replace_root_child(fragment, namespaces, name, None, order);
    }
    let replacement = value.map(|value| format!("<{prefix}{name} val=\"{}\"/>", xml_escape(value)));
    replace_root_child(fragment, namespaces, name, replacement.as_deref(), order)
}

fn set_text_child(
    fragment: &str,
    namespaces: &str,
    prefix: &str,
    name: &str,
    value: Option<&str>,
    order: &[&str],
) -> Result<String, String> {
    let replacement =
        value.map(|value| format!("<{prefix}{name}>{}</{prefix}{name}>", xml_escape(value)));
    replace_root_child(fragment, namespaces, name, replacement.as_deref(), order)
}

fn bool_value(value: &Value, field: &str) -> Result<Option<&'static str>, String> {
    match value {
        Value::Null => Ok(None),
        Value::Bool(true) => Ok(Some("1")),
        Value::Bool(false) => Ok(Some("0")),
        _ => Err(format!("{field} must be a boolean or null")),
    }
}

fn number_value(value: &Value, field: &str) -> Result<Option<String>, String> {
    match value {
        Value::Null => Ok(None),
        Value::Number(number) => number
            .as_f64()
            .filter(|number| number.is_finite())
            .map(|number| Some(number.to_string()))
            .ok_or_else(|| format!("{field} must be a finite number or null")),
        _ => Err(format!("{field} must be a finite number or null")),
    }
}

fn data_labels_order() -> &'static [&'static str] {
    &[
        "dLbl",
        "delete",
        "numFmt",
        "spPr",
        "txPr",
        "dLblPos",
        "showLegendKey",
        "showVal",
        "showCatName",
        "showSerName",
        "showPercent",
        "showBubbleSize",
        "showLeaderLines",
        "leaderLines",
        "showDataLabelsRange",
        "separator",
        "extLst",
    ]
}

fn data_label_entry_order() -> &'static [&'static str] {
    &[
        "idx",
        "delete",
        "layout",
        "tx",
        "numFmt",
        "spPr",
        "txPr",
        "dLblPos",
        "showLegendKey",
        "showVal",
        "showCatName",
        "showSerName",
        "showPercent",
        "showBubbleSize",
        "showLeaderLines",
        "leaderLines",
        "separator",
        "extLst",
    ]
}

fn edit_number_format(
    fragment: &str,
    namespaces: &str,
    prefix: &str,
    edit: &Value,
    order: &[&str],
) -> Result<String, String> {
    if edit.is_null() {
        return replace_root_child(fragment, namespaces, "numFmt", None, order);
    }
    let edit = edit
        .as_object()
        .ok_or("numberFormat must be an object or null")?;
    let children = root_child_ranges(fragment, namespaces)?;
    let existing = children
        .iter()
        .find(|(name, _)| name == "numFmt")
        .map(|(_, range)| fragment[range.clone()].to_string());
    let mut num_fmt = existing.unwrap_or_else(|| format!("<{prefix}numFmt/>"));
    if let Some(code) = edit.get("code") {
        let code = code.as_str().ok_or("numberFormat.code must be a string")?;
        num_fmt = set_start_tag_attribute(&num_fmt, "formatCode", code);
    }
    if let Some(source_linked) = edit.get("sourceLinked") {
        let value = bool_value(source_linked, "numberFormat.sourceLinked")?
            .ok_or("numberFormat.sourceLinked cannot be null")?;
        num_fmt = set_start_tag_attribute(&num_fmt, "sourceLinked", value);
    }
    replace_root_child(fragment, namespaces, "numFmt", Some(&num_fmt), order)
}

fn edit_data_label_entry(
    template: Option<&str>,
    namespaces: &str,
    prefix: &str,
    edit: &Value,
) -> Result<String, String> {
    let index = edit
        .get("index")
        .and_then(Value::as_u64)
        .ok_or("data label requires a non-negative integer index")?;
    let mut label = template
        .map(str::to_string)
        .unwrap_or_else(|| format!("<{prefix}dLbl><{prefix}idx val=\"{index}\"/></{prefix}dLbl>"));
    label = set_value_child(
        &label,
        namespaces,
        prefix,
        "idx",
        Some(&index.to_string()),
        data_label_entry_order(),
    )?;
    for (json_name, xml_name) in [
        ("delete", "delete"),
        ("showLegendKey", "showLegendKey"),
        ("showValue", "showVal"),
        ("showCategoryName", "showCatName"),
        ("showSeriesName", "showSerName"),
        ("showPercent", "showPercent"),
        ("showBubbleSize", "showBubbleSize"),
        ("showLeaderLines", "showLeaderLines"),
    ] {
        if let Some(value) = edit.get(json_name) {
            label = set_value_child(
                &label,
                namespaces,
                prefix,
                xml_name,
                bool_value(value, json_name)?,
                data_label_entry_order(),
            )?;
        }
    }
    if let Some(value) = edit.get("position") {
        let position = match value {
            Value::Null => None,
            Value::String(value) => Some(
                data_label_position_code(value)
                    .ok_or_else(|| format!("unsupported data-label position {value}"))?,
            ),
            _ => return Err("data-label position must be a string or null".into()),
        };
        label = set_value_child(
            &label,
            namespaces,
            prefix,
            "dLblPos",
            position,
            data_label_entry_order(),
        )?;
    }
    if let Some(value) = edit.get("text") {
        let value = match value {
            Value::Null => None,
            Value::String(value) if value.is_empty() => None,
            Value::String(value) => Some(value.as_str()),
            _ => return Err("data-label text must be a string or null".into()),
        };
        label = if let Some(value) = value {
            edit_text_owner(&label, namespaces, prefix, value, data_label_entry_order())?
        } else {
            replace_root_child(&label, namespaces, "tx", None, data_label_entry_order())?
        };
    }
    if let Some(value) = edit.get("numberFormat") {
        label = edit_number_format(&label, namespaces, prefix, value, data_label_entry_order())?;
    }
    if let Some(value) = edit.get("separator") {
        let value = match value {
            Value::Null => None,
            Value::String(value) => Some(value.as_str()),
            _ => return Err("data-label separator must be a string or null".into()),
        };
        label = set_text_child(
            &label,
            namespaces,
            prefix,
            "separator",
            value,
            data_label_entry_order(),
        )?;
    }
    Ok(label)
}

fn edit_data_label_entries(
    labels: &str,
    namespaces: &str,
    prefix: &str,
    edits: &Value,
) -> Result<String, String> {
    let edits = edits
        .as_array()
        .ok_or("dataLabels.labels must be an array")?;
    let ranges: Vec<Range<usize>> = root_child_ranges(labels, namespaces)?
        .into_iter()
        .filter(|(name, _)| name == "dLbl")
        .map(|(_, range)| range)
        .collect();
    let mut existing = Vec::new();
    for range in &ranges {
        let raw = labels[range.clone()].to_string();
        let (wrapped, _) = wrapped_document(&raw, namespaces)?;
        let document = Document::parse(&wrapped).map_err(|error| error.to_string())?;
        let index = fragment_root(&document)
            .and_then(|root| direct_child(root, "idx"))
            .and_then(|node| node.attribute("val"))
            .and_then(|value| value.parse::<u64>().ok());
        existing.push((index, raw));
    }
    let mut seen = HashSet::new();
    let mut replacements = Vec::new();
    for edit in edits {
        let index = edit
            .get("index")
            .and_then(Value::as_u64)
            .ok_or("data label requires a non-negative integer index")?;
        if !seen.insert(index) {
            return Err(format!("duplicate data-label index {index}"));
        }
        let template = existing
            .iter()
            .find(|(candidate, _)| *candidate == Some(index))
            .map(|(_, raw)| raw.as_str());
        replacements.push(edit_data_label_entry(template, namespaces, prefix, edit)?);
    }
    replacements.extend(
        existing
            .iter()
            .filter(|(index, _)| index.is_none())
            .map(|(_, raw)| raw.clone()),
    );
    let mut output = labels.to_string();
    for range in ranges.into_iter().rev() {
        output.replace_range(range, "");
    }
    if replacements.is_empty() {
        return Ok(output);
    }
    replace_root_child(
        &output,
        namespaces,
        "dLbl",
        Some(&replacements.concat()),
        data_labels_order(),
    )
}

fn edit_data_labels_fragment(
    template: Option<&str>,
    namespaces: &str,
    prefix: &str,
    edit: &Value,
) -> Result<Option<String>, String> {
    if edit.is_null() {
        return Ok(None);
    }
    let edit = edit
        .as_object()
        .ok_or("dataLabels must be an object or null")?;
    let mut labels = template
        .map(str::to_string)
        .unwrap_or_else(|| format!("<{prefix}dLbls></{prefix}dLbls>"));
    for (json_name, xml_name) in [
        ("delete", "delete"),
        ("showLegendKey", "showLegendKey"),
        ("showValue", "showVal"),
        ("showCategoryName", "showCatName"),
        ("showSeriesName", "showSerName"),
        ("showPercent", "showPercent"),
        ("showBubbleSize", "showBubbleSize"),
        ("showLeaderLines", "showLeaderLines"),
        ("showDataLabelsRange", "showDataLabelsRange"),
    ] {
        if let Some(value) = edit.get(json_name) {
            labels = set_value_child(
                &labels,
                namespaces,
                prefix,
                xml_name,
                bool_value(value, json_name)?,
                data_labels_order(),
            )?;
        }
    }
    if let Some(value) = edit.get("position") {
        let position = match value {
            Value::Null => None,
            Value::String(value) => Some(
                data_label_position_code(value)
                    .ok_or_else(|| format!("unsupported data-label position {value}"))?,
            ),
            _ => return Err("data-label position must be a string or null".into()),
        };
        labels = set_value_child(
            &labels,
            namespaces,
            prefix,
            "dLblPos",
            position,
            data_labels_order(),
        )?;
    }
    if let Some(value) = edit.get("separator") {
        let separator = match value {
            Value::Null => None,
            Value::String(value) => Some(value.as_str()),
            _ => return Err("dataLabels.separator must be a string or null".into()),
        };
        labels = set_text_child(
            &labels,
            namespaces,
            prefix,
            "separator",
            separator,
            data_labels_order(),
        )?;
    }
    if let Some(value) = edit.get("numberFormat") {
        labels = edit_number_format(&labels, namespaces, prefix, value, data_labels_order())?;
    }
    if let Some(value) = edit.get("labels") {
        labels = edit_data_label_entries(&labels, namespaces, prefix, value)?;
    }
    Ok(Some(labels))
}

fn trendline_order() -> &'static [&'static str] {
    &[
        "name",
        "spPr",
        "trendlineType",
        "order",
        "period",
        "forward",
        "backward",
        "intercept",
        "dispRSqr",
        "dispEq",
        "trendlineLbl",
        "extLst",
    ]
}

fn edit_trendline_fragment(
    template: Option<&str>,
    namespaces: &str,
    prefix: &str,
    edit: &Value,
) -> Result<String, String> {
    let mut trendline = template
        .map(str::to_string)
        .unwrap_or_else(|| format!("<{prefix}trendline></{prefix}trendline>"));
    if let Some(value) = edit.get("name") {
        let value = match value {
            Value::Null => None,
            Value::String(value) => Some(value.as_str()),
            _ => return Err("trendline.name must be a string or null".into()),
        };
        trendline = set_text_child(
            &trendline,
            namespaces,
            prefix,
            "name",
            value,
            trendline_order(),
        )?;
    }
    if let Some(value) = edit.get("type") {
        let value = value.as_str().ok_or("trendline.type must be a string")?;
        if !matches!(
            value,
            "exp" | "linear" | "log" | "movingAvg" | "poly" | "power"
        ) {
            return Err(format!("unsupported trendline type {value}"));
        }
        trendline = set_value_child(
            &trendline,
            namespaces,
            prefix,
            "trendlineType",
            Some(value),
            trendline_order(),
        )?;
    } else if template.is_none() {
        trendline = set_value_child(
            &trendline,
            namespaces,
            prefix,
            "trendlineType",
            Some("linear"),
            trendline_order(),
        )?;
    }
    for (json_name, xml_name) in [
        ("order", "order"),
        ("period", "period"),
        ("forward", "forward"),
        ("backward", "backward"),
        ("intercept", "intercept"),
    ] {
        if let Some(value) = edit.get(json_name) {
            let serialized = number_value(value, json_name)?;
            if matches!(json_name, "order" | "period") {
                if let Some(number) = serialized
                    .as_deref()
                    .and_then(|value| value.parse::<f64>().ok())
                {
                    if number.fract() != 0.0 || number < 2.0 || number > 255.0 {
                        return Err(format!(
                            "trendline.{json_name} must be an integer from 2 to 255"
                        ));
                    }
                }
            }
            trendline = set_value_child(
                &trendline,
                namespaces,
                prefix,
                xml_name,
                serialized.as_deref(),
                trendline_order(),
            )?;
        }
    }
    for (json_name, xml_name) in [
        ("displayRSquared", "dispRSqr"),
        ("displayEquation", "dispEq"),
    ] {
        if let Some(value) = edit.get(json_name) {
            trendline = set_value_child(
                &trendline,
                namespaces,
                prefix,
                xml_name,
                bool_value(value, json_name)?,
                trendline_order(),
            )?;
        }
    }
    if let Some(value) = edit.get("label") {
        let value = match value {
            Value::Null => None,
            Value::String(value) if value.is_empty() => None,
            Value::String(value) => Some(value.as_str()),
            _ => return Err("trendline.label must be a string or null".into()),
        };
        if let Some(value) = value {
            let existing = root_child_ranges(&trendline, namespaces)?
                .into_iter()
                .find(|(name, _)| name == "trendlineLbl")
                .map(|(_, range)| trendline[range].to_string());
            let label = if let Some(existing) = existing {
                edit_text_owner(
                    &existing,
                    namespaces,
                    prefix,
                    value,
                    &["layout", "tx", "numFmt", "spPr", "txPr", "extLst"],
                )?
            } else {
                let tx = rich_text_xml(prefix, value);
                format!("<{prefix}trendlineLbl>{tx}</{prefix}trendlineLbl>")
            };
            trendline = replace_root_child(
                &trendline,
                namespaces,
                "trendlineLbl",
                Some(&label),
                trendline_order(),
            )?;
        } else {
            trendline = replace_root_child(
                &trendline,
                namespaces,
                "trendlineLbl",
                None,
                trendline_order(),
            )?;
        }
    }
    Ok(trendline)
}

fn apply_trendlines(
    series: &str,
    namespaces: &str,
    prefix: &str,
    edits: &Value,
) -> Result<String, String> {
    let edits = edits.as_array().ok_or("trendlines must be an array")?;
    let ranges: Vec<Range<usize>> = root_child_ranges(series, namespaces)?
        .into_iter()
        .filter(|(name, _)| name == "trendline")
        .map(|(_, range)| range)
        .collect();
    let templates: Vec<String> = ranges
        .iter()
        .map(|range| series[range.clone()].to_string())
        .collect();
    let mut replacements = Vec::new();
    for (position, edit) in edits.iter().enumerate() {
        if edit.get("$delete").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let requested_index = edit
            .get("index")
            .and_then(Value::as_u64)
            .map(|index| index as usize)
            .unwrap_or(position);
        replacements.push(edit_trendline_fragment(
            templates.get(requested_index).map(String::as_str),
            namespaces,
            prefix,
            edit,
        )?);
    }
    let mut output = series.to_string();
    for range in ranges.into_iter().rev() {
        output.replace_range(range, "");
    }
    if replacements.is_empty() {
        return Ok(output);
    }
    replace_root_child(
        &output,
        namespaces,
        "trendline",
        Some(&replacements.concat()),
        series_order_names(),
    )
}

fn error_bars_order() -> &'static [&'static str] {
    &[
        "errDir",
        "errBarType",
        "errValType",
        "noEndCap",
        "plus",
        "minus",
        "val",
        "spPr",
        "extLst",
    ]
}

fn edit_error_amount(
    error: &str,
    namespaces: &str,
    prefix: &str,
    name: &str,
    edit: &Value,
) -> Result<String, String> {
    if edit.is_null() {
        return replace_root_child(error, namespaces, name, None, error_bars_order());
    }
    let edit = edit
        .as_object()
        .ok_or_else(|| format!("errorBars.{name} must be an object or null"))?;
    let children = root_child_ranges(error, namespaces)?;
    let existing = children
        .iter()
        .find(|(local, _)| local == name)
        .map(|(_, range)| error[range.clone()].to_string());
    let (baseline_formula, baseline_values, baseline_binding) = if let Some(raw) = &existing {
        let (wrapped, _) = wrapped_document(raw, namespaces)?;
        let document = Document::parse(&wrapped).map_err(|error| error.to_string())?;
        let root = fragment_root(&document).ok_or("missing error-bar amount")?;
        (
            formula_text(Some(root)),
            cache_numbers(Some(root)),
            source_binding_mode(Some(root)).to_string(),
        )
    } else {
        (String::new(), Vec::new(), "embedded".to_string())
    };
    let mut formula = edit
        .get("formula")
        .and_then(Value::as_str)
        .unwrap_or(&baseline_formula)
        .trim()
        .to_string();
    let values = edit
        .get("values")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or(baseline_values);
    if values
        .iter()
        .any(|value| !value.is_null() && value.as_f64().is_none())
    {
        return Err(format!(
            "errorBars.{name}.values must contain numbers or null"
        ));
    }
    let binding = edit
        .get("bindingMode")
        .and_then(Value::as_str)
        .unwrap_or(&baseline_binding);
    match binding {
        "embedded" => formula.clear(),
        "reference" if formula.is_empty() => {
            return Err(format!(
                "errorBars.{name} reference binding requires formula"
            ));
        }
        "reference" | "unknown" => {}
        _ => {
            return Err(format!(
                "unsupported errorBars.{name}.bindingMode {binding}"
            ));
        }
    }
    let source = build_source_xml(prefix, &formula, &values, true);
    let container = if let Some(raw) = existing {
        let source_names = ["numRef", "numLit"];
        let source_range = root_child_ranges(&raw, namespaces)?
            .into_iter()
            .find(|(local, _)| source_names.contains(&local.as_str()))
            .map(|(_, range)| range);
        let mut output = raw;
        if let Some(range) = source_range {
            output.replace_range(range, &source);
        } else {
            let insert = output.find('>').ok_or("malformed error-bar amount")? + 1;
            output.insert_str(insert, &source);
        }
        output
    } else {
        format!("<{prefix}{name}>{source}</{prefix}{name}>")
    };
    replace_root_child(
        error,
        namespaces,
        name,
        Some(&container),
        error_bars_order(),
    )
}

fn edit_error_bar_fragment(
    template: Option<&str>,
    namespaces: &str,
    prefix: &str,
    edit: &Value,
) -> Result<String, String> {
    let mut error = template
        .map(str::to_string)
        .unwrap_or_else(|| format!("<{prefix}errBars></{prefix}errBars>"));
    for (json_name, xml_name, allowed) in [
        ("direction", "errDir", &["x", "y"][..]),
        ("barType", "errBarType", &["both", "minus", "plus"][..]),
        (
            "valueType",
            "errValType",
            &["cust", "fixedVal", "percentage", "stdDev", "stdErr"][..],
        ),
    ] {
        if let Some(value) = edit.get(json_name) {
            let value = value
                .as_str()
                .ok_or_else(|| format!("errorBars.{json_name} must be a string"))?;
            if !allowed.contains(&value) {
                return Err(format!("unsupported errorBars.{json_name} {value}"));
            }
            error = set_value_child(
                &error,
                namespaces,
                prefix,
                xml_name,
                Some(value),
                error_bars_order(),
            )?;
        }
    }
    if template.is_none() {
        for (name, value) in [
            ("errDir", "y"),
            ("errBarType", "both"),
            ("errValType", "fixedVal"),
        ] {
            if !root_child_ranges(&error, namespaces)?
                .iter()
                .any(|(local, _)| local == name)
            {
                error = set_value_child(
                    &error,
                    namespaces,
                    prefix,
                    name,
                    Some(value),
                    error_bars_order(),
                )?;
            }
        }
    }
    if let Some(value) = edit.get("noEndCap") {
        error = set_value_child(
            &error,
            namespaces,
            prefix,
            "noEndCap",
            bool_value(value, "errorBars.noEndCap")?,
            error_bars_order(),
        )?;
    }
    if let Some(value) = edit.get("value") {
        let serialized = number_value(value, "errorBars.value")?;
        error = set_value_child(
            &error,
            namespaces,
            prefix,
            "val",
            serialized.as_deref(),
            error_bars_order(),
        )?;
    }
    for name in ["plus", "minus"] {
        if let Some(value) = edit.get(name) {
            error = edit_error_amount(&error, namespaces, prefix, name, value)?;
        }
    }
    Ok(error)
}

fn apply_error_bars(
    series: &str,
    namespaces: &str,
    prefix: &str,
    edits: &Value,
) -> Result<String, String> {
    let edits = edits.as_array().ok_or("errorBars must be an array")?;
    let ranges: Vec<Range<usize>> = root_child_ranges(series, namespaces)?
        .into_iter()
        .filter(|(name, _)| name == "errBars")
        .map(|(_, range)| range)
        .collect();
    let templates: Vec<String> = ranges
        .iter()
        .map(|range| series[range.clone()].to_string())
        .collect();
    let mut used = HashSet::new();
    let mut replacements = Vec::new();
    for (position, edit) in edits.iter().enumerate() {
        if edit.get("$delete").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let direction = edit.get("direction").and_then(Value::as_str);
        let matched = direction
            .and_then(|direction| {
                templates.iter().enumerate().find_map(|(index, raw)| {
                    if used.contains(&index) {
                        return None;
                    }
                    let (wrapped, _) = wrapped_document(raw, namespaces).ok()?;
                    let document = Document::parse(&wrapped).ok()?;
                    let root = fragment_root(&document)?;
                    (child_val(root, "errDir").as_deref() == Some(direction)).then_some(index)
                })
            })
            .or_else(|| {
                (!used.contains(&position) && position < templates.len()).then_some(position)
            });
        if let Some(index) = matched {
            used.insert(index);
        }
        replacements.push(edit_error_bar_fragment(
            matched.and_then(|index| templates.get(index).map(String::as_str)),
            namespaces,
            prefix,
            edit,
        )?);
    }
    let mut output = series.to_string();
    for range in ranges.into_iter().rev() {
        output.replace_range(range, "");
    }
    if replacements.is_empty() {
        return Ok(output);
    }
    replace_root_child(
        &output,
        namespaces,
        "errBars",
        Some(&replacements.concat()),
        series_order_names(),
    )
}

fn remove_point_color(point: &str, namespaces: &str) -> Result<String, String> {
    let (wrapped, offset) = wrapped_document(point, namespaces)?;
    let document = Document::parse(&wrapped).map_err(|error| error.to_string())?;
    let root = fragment_root(&document).ok_or("missing data point root")?;
    let Some(fill) = direct_child(root, "spPr").and_then(|shape| direct_child(shape, "solidFill"))
    else {
        return Ok(point.to_string());
    };
    let range = fill.range();
    let mut output = point.to_string();
    output.replace_range(range.start - offset..range.end - offset, "");
    Ok(output)
}

fn apply_point_color(
    point: &str,
    namespaces: &str,
    prefix: &str,
    color: &str,
) -> Result<String, String> {
    if color.trim().is_empty() {
        return remove_point_color(point, namespaces);
    }
    if desired_color_node(color).is_none() {
        return Err(format!("unsupported chart data-point colour {color}"));
    }
    let (wrapped, _) = wrapped_document(point, namespaces)?;
    let document = Document::parse(&wrapped).map_err(|error| error.to_string())?;
    let root = fragment_root(&document).ok_or("missing data point root")?;
    if direct_child(root, "spPr").is_some() {
        return apply_series_color(point, namespaces, prefix, color, false);
    }
    let (kind, value) = desired_color_node(color).expect("validated point colour");
    let shape = format!(
        "<{prefix}spPr><a:solidFill xmlns:a=\"{DRAWING_NS}\"><a:{kind} val=\"{}\"/></a:solidFill></{prefix}spPr>",
        xml_escape(&value)
    );
    replace_root_child(point, namespaces, "spPr", Some(&shape), point_order_names())
}

fn edit_point_marker(
    point: &str,
    namespaces: &str,
    prefix: &str,
    symbol: Option<&Value>,
    size: Option<&Value>,
) -> Result<String, String> {
    let children = root_child_ranges(point, namespaces)?;
    let marker_range = children
        .iter()
        .find(|(name, _)| name == "marker")
        .map(|(_, range)| range.clone());
    if matches!(symbol, Some(Value::Null))
        || symbol
            .and_then(Value::as_str)
            .is_some_and(|value| value.trim().is_empty())
    {
        return replace_root_child(point, namespaces, "marker", None, point_order_names());
    }
    if marker_range.is_none() && symbol.is_none() && size.is_some() {
        return Err("markerSize requires markerSymbol".to_string());
    }
    if marker_range.is_none() && symbol.is_none() && size.is_none() {
        return Ok(point.to_string());
    }
    let mut marker = marker_range
        .as_ref()
        .map(|range| point[range.clone()].to_string())
        .unwrap_or_else(|| format!("<{prefix}marker></{prefix}marker>"));
    if let Some(symbol) = symbol {
        let symbol = symbol
            .as_str()
            .ok_or("markerSymbol must be a string or null")?
            .trim();
        marker = set_value_child(
            &marker,
            namespaces,
            prefix,
            "symbol",
            Some(symbol),
            marker_order_names(),
        )?;
    }
    if let Some(size) = size {
        let size = match size {
            Value::Null => None,
            value => {
                let size = value
                    .as_u64()
                    .ok_or("markerSize must be an integer or null")?;
                if !(2..=72).contains(&size) {
                    return Err("markerSize must be between 2 and 72".to_string());
                }
                Some(size.to_string())
            }
        };
        marker = set_value_child(
            &marker,
            namespaces,
            prefix,
            "size",
            size.as_deref(),
            marker_order_names(),
        )?;
    }
    replace_root_child(
        point,
        namespaces,
        "marker",
        Some(&marker),
        point_order_names(),
    )
}

fn edit_point_fragment(
    template: Option<&str>,
    namespaces: &str,
    prefix: &str,
    edit: &Value,
) -> Result<String, String> {
    let index = edit
        .get("index")
        .and_then(Value::as_u64)
        .ok_or("chart data-point override requires a non-negative integer index")?;
    let mut point = template
        .map(str::to_string)
        .unwrap_or_else(|| format!("<{prefix}dPt><{prefix}idx val=\"{index}\"/></{prefix}dPt>"));
    let baseline = if template.is_some() {
        let (wrapped, _) = wrapped_document(&point, namespaces)?;
        let document = Document::parse(&wrapped).map_err(|error| error.to_string())?;
        fragment_root(&document)
            .and_then(parse_point_override)
            .unwrap_or_else(|| json!({}))
    } else {
        json!({})
    };
    point = set_value_child(
        &point,
        namespaces,
        prefix,
        "idx",
        Some(&index.to_string()),
        point_order_names(),
    )?;
    if let Some(color) = edit.get("color") {
        if baseline.get("color") != Some(color) {
            let color = color
                .as_str()
                .ok_or("chart data-point color must be a string")?;
            point = apply_point_color(&point, namespaces, prefix, color)?;
        }
    }
    if let Some(explosion) = edit.get("explosion") {
        if baseline.get("explosion") != Some(explosion) {
            let value = match explosion {
                Value::Null => None,
                value => {
                    let value = value
                        .as_u64()
                        .ok_or("chart data-point explosion must be an integer or null")?;
                    if value > 400 {
                        return Err("chart data-point explosion must be between 0 and 400".into());
                    }
                    Some(value.to_string())
                }
            };
            point = set_value_child(
                &point,
                namespaces,
                prefix,
                "explosion",
                value.as_deref(),
                point_order_names(),
            )?;
        }
    }
    let symbol = edit.get("markerSymbol");
    let size = edit.get("markerSize");
    if symbol.is_some_and(|value| baseline.get("markerSymbol") != Some(value))
        || size.is_some_and(|value| baseline.get("markerSize") != Some(value))
    {
        point = edit_point_marker(&point, namespaces, prefix, symbol, size)?;
    }
    Ok(point)
}

fn apply_point_overrides(
    series: &str,
    namespaces: &str,
    prefix: &str,
    edits: &Value,
) -> Result<String, String> {
    let edits = edits.as_array().ok_or("pointOverrides must be an array")?;
    let children = root_child_ranges(series, namespaces)?;
    let point_ranges: Vec<Range<usize>> = children
        .iter()
        .filter(|(name, _)| name == "dPt")
        .map(|(_, range)| range.clone())
        .collect();
    let mut existing = Vec::<(Option<u64>, String)>::new();
    for range in &point_ranges {
        let raw = series[range.clone()].to_string();
        let (wrapped, _) = wrapped_document(&raw, namespaces)?;
        let document = Document::parse(&wrapped).map_err(|error| error.to_string())?;
        let index = fragment_root(&document)
            .and_then(parse_point_override)
            .and_then(|model| model.get("index").and_then(Value::as_u64));
        existing.push((index, raw));
    }
    let mut seen = HashSet::new();
    let mut replacements = Vec::new();
    for edit in edits {
        let index = edit
            .get("index")
            .and_then(Value::as_u64)
            .ok_or("chart data-point override requires a non-negative integer index")?;
        if !seen.insert(index) {
            return Err(format!("duplicate chart data-point override index {index}"));
        }
        let template = existing
            .iter()
            .find(|(candidate, _)| *candidate == Some(index))
            .map(|(_, raw)| raw.as_str());
        replacements.push(edit_point_fragment(template, namespaces, prefix, edit)?);
    }
    // Malformed/extension-only dPt nodes without an index cannot be represented by the public
    // model, so retain them byte-for-byte even when the visible override list is edited.
    replacements.extend(
        existing
            .iter()
            .filter(|(index, _)| index.is_none())
            .map(|(_, raw)| raw.clone()),
    );
    let mut output = series.to_string();
    for range in point_ranges.into_iter().rev() {
        output.replace_range(range, "");
    }
    let replacement = replacements.concat();
    if replacement.is_empty() {
        return Ok(output);
    }
    replace_root_child(
        &output,
        namespaces,
        "dPt",
        Some(&replacement),
        series_order_names(),
    )
}

fn edit_series_fragment(
    template: &str,
    namespaces: &str,
    prefix: &str,
    target_kind: &str,
    edit: &Value,
    index: usize,
) -> Result<String, String> {
    let mut series = normalize_series_sources(template.to_string(), namespaces, target_kind)?;
    let baseline = {
        let (wrapped, _) = wrapped_document(&series, namespaces)?;
        let document = Document::parse(&wrapped).map_err(|error| error.to_string())?;
        let node = fragment_root(&document).ok_or("missing series root")?;
        parse_series(node, target_kind)
    };
    series = set_series_index(&series, namespaces, prefix, "idx", index)?;
    series = set_series_index(&series, namespaces, prefix, "order", index)?;

    if let Some(name) = edit.get("name").and_then(Value::as_str) {
        if baseline.get("name").and_then(Value::as_str) != Some(name) {
            series = replace_series_name(&series, namespaces, prefix, name)?;
        }
    }
    let baseline_categories: Vec<String> = baseline
        .get("categories")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| item.as_str().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default();
    let baseline_values = baseline
        .get("values")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let baseline_category_formula = baseline
        .get("categoryFormula")
        .and_then(Value::as_str)
        .unwrap_or("");
    let baseline_value_formula = baseline
        .get("valueFormula")
        .and_then(Value::as_str)
        .unwrap_or("");
    let baseline_binding_mode = baseline
        .get("bindingMode")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let binding_mode_explicit = edit.get("bindingMode").is_some();
    let requested_binding_mode = edit
        .get("bindingMode")
        .and_then(Value::as_str)
        .unwrap_or(baseline_binding_mode);
    if !matches!(
        requested_binding_mode,
        "reference" | "embedded" | "mixed" | "unknown"
    ) {
        return Err(format!(
            "unsupported chart series bindingMode {requested_binding_mode}"
        ));
    }
    let mut category_formula = edit
        .get("categoryFormula")
        .and_then(Value::as_str)
        .unwrap_or(baseline_category_formula)
        .trim()
        .to_string();
    let mut value_formula = edit
        .get("valueFormula")
        .and_then(Value::as_str)
        .unwrap_or(baseline_value_formula)
        .trim()
        .to_string();
    let mut categories = json_string_points(edit.get("categories"), &baseline_categories);
    let values = json_number_points(edit.get("values"), &baseline_values);
    let category_container = if matches!(target_kind, "scatter" | "bubble") {
        "xVal"
    } else {
        "cat"
    };
    let value_container = if matches!(target_kind, "scatter" | "bubble") {
        "yVal"
    } else {
        "val"
    };
    let category_was_numeric = data_container_is_numeric(&series, namespaces, category_container)?;
    let category_numeric = matches!(target_kind, "scatter" | "bubble") || category_was_numeric;
    let category_source_kind_changed =
        matches!(target_kind, "scatter" | "bubble") && !category_was_numeric;
    let mut baseline_category_points = json_string_points(None, &baseline_categories);
    if category_numeric {
        categories = numeric_category_points(categories);
        baseline_category_points = numeric_category_points(baseline_category_points);
    }
    let category_formula_changed = category_formula != baseline_category_formula;
    let value_formula_changed = value_formula != baseline_value_formula;
    let category_points_changed =
        edit.get("categories").is_some() && categories != baseline_category_points;
    let value_points_changed = edit.get("values").is_some() && values != baseline_values;
    let binding_changed = binding_mode_explicit && requested_binding_mode != baseline_binding_mode;

    match (binding_mode_explicit, requested_binding_mode) {
        (true, "embedded") => {
            // Literal sources must not retain an empty/old c:f child.  Rebuilding the source as
            // strLit/numLit also prevents Excel from replacing the edited cache on recalculation.
            category_formula.clear();
            value_formula.clear();
        }
        (true, "reference") => {
            let binding_touched = binding_changed
                || category_formula_changed
                || value_formula_changed
                || category_points_changed
                || value_points_changed;
            if binding_touched && (category_formula.is_empty() || value_formula.is_empty()) {
                return Err(
                    "reference-bound chart series require both categoryFormula and valueFormula; choose bindingMode=embedded for constant data"
                        .to_string(),
                );
            }
            if category_points_changed && !category_formula_changed {
                return Err(
                    "category data belongs to the referenced cells; change categoryFormula/source cells or choose bindingMode=embedded"
                        .to_string(),
                );
            }
            if value_points_changed && !value_formula_changed {
                return Err(
                    "series values belong to the referenced cells; change valueFormula/source cells or choose bindingMode=embedded"
                        .to_string(),
                );
            }
        }
        // Mixed/legacy payloads retain independent source semantics.  Editing only a cache that
        // still points at the same range is interpreted as an intentional conversion of that
        // source to a literal, never as a stale cache underneath an unchanged reference.
        _ => {
            if category_points_changed && !category_formula_changed && !category_formula.is_empty()
            {
                category_formula.clear();
            }
            if value_points_changed && !value_formula_changed && !value_formula.is_empty() {
                value_formula.clear();
            }
        }
    }
    let category_changed = binding_changed
        || category_source_kind_changed
        || category_formula != baseline_category_formula
        || category_points_changed;
    let value_changed =
        binding_changed || value_formula != baseline_value_formula || value_points_changed;
    if category_changed {
        series = update_data_container(
            &series,
            namespaces,
            prefix,
            category_container,
            &category_formula,
            &categories,
            category_numeric,
        )?;
    }
    if value_changed {
        series = update_data_container(
            &series,
            namespaces,
            prefix,
            value_container,
            &value_formula,
            &values,
            true,
        )?;
    }
    if target_kind == "bubble"
        && !root_child_ranges(&series, namespaces)?
            .iter()
            .any(|(name, _)| name == "bubbleSize")
    {
        let sizes = vec![Value::from(1.0); values.len().max(categories.len())];
        series =
            update_data_container(&series, namespaces, prefix, "bubbleSize", "", &sizes, true)?;
    }
    if let Some(color) = edit.get("color").and_then(Value::as_str) {
        if baseline.get("color").and_then(Value::as_str) != Some(color) {
            series = apply_series_color(
                &series,
                namespaces,
                prefix,
                color,
                matches!(target_kind, "line" | "scatter" | "radar" | "stock"),
            )?;
        }
    }
    if let Some(point_overrides) = edit.get("pointOverrides") {
        if baseline.get("pointOverrides") != Some(point_overrides) {
            series = apply_point_overrides(&series, namespaces, prefix, point_overrides)?;
        }
    }
    if let Some(data_labels) = edit.get("dataLabels") {
        if baseline.get("dataLabels") != Some(data_labels) {
            let existing = root_child_ranges(&series, namespaces)?
                .into_iter()
                .find(|(name, _)| name == "dLbls")
                .map(|(_, range)| series[range].to_string());
            let replacement =
                edit_data_labels_fragment(existing.as_deref(), namespaces, prefix, data_labels)?;
            series = replace_root_child(
                &series,
                namespaces,
                "dLbls",
                replacement.as_deref(),
                series_order_names(),
            )?;
        }
    }
    if let Some(trendlines) = edit.get("trendlines") {
        if baseline.get("trendlines") != Some(trendlines) {
            series = apply_trendlines(&series, namespaces, prefix, trendlines)?;
        }
    }
    if let Some(error_bars) = edit.get("errorBars") {
        if baseline.get("errorBars") != Some(error_bars) {
            series = apply_error_bars(&series, namespaces, prefix, error_bars)?;
        }
    }
    Ok(series)
}

fn compatible_chart_conversion(old: &str, new: &str) -> bool {
    if old == new {
        return true;
    }
    let family = |kind: &str| match kind {
        "bar" | "line" | "area" | "radar" => 1,
        "pie" | "doughnut" => 2,
        "scatter" | "bubble" => 3,
        "stock" => 4,
        _ => 0,
    };
    family(old) != 0 && family(old) == family(new) && family(old) != 4
}

fn axis_order_names() -> &'static [&'static str] {
    &[
        "axId",
        "scaling",
        "delete",
        "axPos",
        "majorGridlines",
        "minorGridlines",
        "title",
        "numFmt",
        "majorTickMark",
        "minorTickMark",
        "tickLblPos",
        "spPr",
        "txPr",
        "crossAx",
        "crossBetween",
        "crosses",
        "crossesAt",
        "auto",
        "lblAlgn",
        "lblOffset",
        "tickLblSkip",
        "tickMarkSkip",
        "noMultiLvlLbl",
        "majorUnit",
        "minorUnit",
        "baseTimeUnit",
        "majorTimeUnit",
        "minorTimeUnit",
        "dispUnits",
        "extLst",
    ]
}

fn scaling_order_names() -> &'static [&'static str] {
    &["logBase", "orientation", "max", "min", "extLst"]
}

fn rich_text_xml(prefix: &str, value: &str) -> String {
    format!(
        "<{prefix}tx><{prefix}rich><a:bodyPr xmlns:a=\"{DRAWING_NS}\"/><a:lstStyle xmlns:a=\"{DRAWING_NS}\"/><a:p xmlns:a=\"{DRAWING_NS}\"><a:r><a:t>{}</a:t></a:r></a:p></{prefix}rich></{prefix}tx>",
        xml_escape(value)
    )
}

fn edit_text_owner(
    owner: &str,
    namespaces: &str,
    prefix: &str,
    value: &str,
    order: &[&str],
) -> Result<String, String> {
    let (wrapped, offset) = wrapped_document(owner, namespaces)?;
    let document = Document::parse(&wrapped).map_err(|error| error.to_string())?;
    let root = fragment_root(&document).ok_or("missing chart text owner")?;
    let scope = direct_child(root, "tx").unwrap_or(root);
    if let Some(text) = scope.descendants().find(|node| {
        node.is_element() && matches!(local_name(*node), "t" | "v") && node.text().is_some()
    }) {
        let range = text.range();
        let local = range.start - offset..range.end - offset;
        let raw = &owner[local.clone()];
        let open = raw.find('>').ok_or("malformed chart text node")? + 1;
        let close = raw.rfind("</").ok_or("malformed chart text node")?;
        let mut replacement = raw.to_string();
        replacement.replace_range(open..close, &xml_escape(value));
        let mut output = owner.to_string();
        output.replace_range(local, &replacement);
        return Ok(output);
    }
    replace_root_child(
        owner,
        namespaces,
        "tx",
        Some(&rich_text_xml(prefix, value)),
        order,
    )
}

fn edit_fragment_title(
    fragment: &str,
    namespaces: &str,
    prefix: &str,
    title: &Value,
    order: &[&str],
) -> Result<String, String> {
    let title = match title {
        Value::Null => None,
        Value::String(value) if value.is_empty() => None,
        Value::String(value) => Some(value.as_str()),
        _ => return Err("axis title must be a string or null".into()),
    };
    let existing = root_child_ranges(fragment, namespaces)?
        .into_iter()
        .find(|(name, _)| name == "title")
        .map(|(_, range)| fragment[range.clone()].to_string());
    let Some(title) = title else {
        return replace_root_child(fragment, namespaces, "title", None, order);
    };
    let replacement = if let Some(existing) = existing {
        edit_text_owner(
            &existing,
            namespaces,
            prefix,
            title,
            &["tx", "layout", "overlay", "spPr", "txPr", "extLst"],
        )?
    } else {
        let tx = rich_text_xml(prefix, title);
        format!("<{prefix}title>{tx}</{prefix}title>")
    };
    replace_root_child(fragment, namespaces, "title", Some(&replacement), order)
}

fn edit_scaling(
    axis: &str,
    namespaces: &str,
    prefix: &str,
    edit: &Value,
) -> Result<String, String> {
    if edit.is_null() {
        return replace_root_child(axis, namespaces, "scaling", None, axis_order_names());
    }
    let edit = edit
        .as_object()
        .ok_or("axis.scaling must be an object or null")?;
    let existing = root_child_ranges(axis, namespaces)?
        .into_iter()
        .find(|(name, _)| name == "scaling")
        .map(|(_, range)| axis[range].to_string());
    let mut scaling = existing.unwrap_or_else(|| format!("<{prefix}scaling></{prefix}scaling>"));
    for (json_name, xml_name) in [("logBase", "logBase"), ("max", "max"), ("min", "min")] {
        if let Some(value) = edit.get(json_name) {
            let serialized = number_value(value, &format!("axis.scaling.{json_name}"))?;
            if json_name == "logBase" {
                if let Some(number) = serialized
                    .as_deref()
                    .and_then(|value| value.parse::<f64>().ok())
                {
                    if !(2.0..=1000.0).contains(&number) {
                        return Err("axis.scaling.logBase must be between 2 and 1000".into());
                    }
                }
            }
            scaling = set_value_child(
                &scaling,
                namespaces,
                prefix,
                xml_name,
                serialized.as_deref(),
                scaling_order_names(),
            )?;
        }
    }
    if let Some(value) = edit.get("orientation") {
        let value = match value {
            Value::Null => None,
            Value::String(value) if matches!(value.as_str(), "minMax" | "maxMin") => {
                Some(value.as_str())
            }
            Value::String(value) => return Err(format!("unsupported axis orientation {value}")),
            _ => return Err("axis.scaling.orientation must be a string or null".into()),
        };
        scaling = set_value_child(
            &scaling,
            namespaces,
            prefix,
            "orientation",
            value,
            scaling_order_names(),
        )?;
    }
    replace_root_child(
        axis,
        namespaces,
        "scaling",
        Some(&scaling),
        axis_order_names(),
    )
}

fn edit_display_units(
    axis: &str,
    namespaces: &str,
    prefix: &str,
    edit: &Value,
) -> Result<String, String> {
    if edit.is_null() {
        return replace_root_child(axis, namespaces, "dispUnits", None, axis_order_names());
    }
    let edit = edit
        .as_object()
        .ok_or("axis.displayUnits must be an object or null")?;
    let existing = root_child_ranges(axis, namespaces)?
        .into_iter()
        .find(|(name, _)| name == "dispUnits")
        .map(|(_, range)| axis[range].to_string());
    let mut units = existing.unwrap_or_else(|| format!("<{prefix}dispUnits></{prefix}dispUnits>"));
    let order = ["custUnit", "builtInUnit", "dispUnitsLbl", "extLst"];
    if let Some(value) = edit.get("builtIn") {
        let value = match value {
            Value::Null => None,
            Value::String(value) => Some(value.as_str()),
            _ => return Err("axis.displayUnits.builtIn must be a string or null".into()),
        };
        units = set_value_child(&units, namespaces, prefix, "builtInUnit", value, &order)?;
        if value.is_some() {
            units = replace_root_child(&units, namespaces, "custUnit", None, &order)?;
        }
    }
    if let Some(value) = edit.get("custom") {
        let serialized = number_value(value, "axis.displayUnits.custom")?;
        units = set_value_child(
            &units,
            namespaces,
            prefix,
            "custUnit",
            serialized.as_deref(),
            &order,
        )?;
        if serialized.is_some() {
            units = replace_root_child(&units, namespaces, "builtInUnit", None, &order)?;
        }
    }
    if let Some(value) = edit.get("showLabel") {
        let show = value
            .as_bool()
            .ok_or("axis.displayUnits.showLabel must be a boolean")?;
        let label = show.then(|| format!("<{prefix}dispUnitsLbl/>"));
        units = replace_root_child(&units, namespaces, "dispUnitsLbl", label.as_deref(), &order)?;
    }
    if let Some(value) = edit.get("label") {
        let value = match value {
            Value::Null => None,
            Value::String(value) if value.is_empty() => None,
            Value::String(value) => Some(value.as_str()),
            _ => return Err("axis.displayUnits.label must be a string or null".into()),
        };
        if let Some(value) = value {
            let existing_label = root_child_ranges(&units, namespaces)?
                .into_iter()
                .find(|(name, _)| name == "dispUnitsLbl")
                .map(|(_, range)| units[range].to_string());
            let label = if let Some(existing_label) = existing_label {
                edit_text_owner(
                    &existing_label,
                    namespaces,
                    prefix,
                    value,
                    &["layout", "tx", "spPr", "txPr", "extLst"],
                )?
            } else {
                let tx = rich_text_xml(prefix, value);
                format!("<{prefix}dispUnitsLbl>{tx}</{prefix}dispUnitsLbl>")
            };
            units = replace_root_child(&units, namespaces, "dispUnitsLbl", Some(&label), &order)?;
        } else {
            units = replace_root_child(&units, namespaces, "dispUnitsLbl", None, &order)?;
        }
    }
    replace_root_child(
        axis,
        namespaces,
        "dispUnits",
        Some(&units),
        axis_order_names(),
    )
}

fn edit_axis_fragment(
    template: Option<&str>,
    namespaces: &str,
    prefix: &str,
    edit: &Value,
) -> Result<String, String> {
    let id = edit
        .get("id")
        .and_then(Value::as_u64)
        .ok_or("axis edit requires a non-negative integer id")?;
    let requested_type = edit
        .get("axisType")
        .and_then(Value::as_str)
        .and_then(axis_local);
    let mut axis = if let Some(template) = template {
        let mut axis = template.to_string();
        if let Some(local) = requested_type {
            axis = rename_root_element(&axis, local);
        } else if edit.get("axisType").is_some() {
            return Err("unsupported axis type".into());
        }
        axis
    } else {
        let local = requested_type.ok_or("new axis requires axisType")?;
        let position = edit
            .get("position")
            .and_then(Value::as_str)
            .and_then(axis_position_code)
            .unwrap_or(if local == "valAx" { "l" } else { "b" });
        format!(
            "<{prefix}{local}><{prefix}axId val=\"{id}\"/><{prefix}scaling><{prefix}orientation val=\"minMax\"/></{prefix}scaling><{prefix}axPos val=\"{position}\"/></{prefix}{local}>"
        )
    };
    axis = set_value_child(
        &axis,
        namespaces,
        prefix,
        "axId",
        Some(&id.to_string()),
        axis_order_names(),
    )?;
    if let Some(value) = edit.get("scaling") {
        axis = edit_scaling(&axis, namespaces, prefix, value)?;
    }
    if let Some(value) = edit.get("delete") {
        axis = set_value_child(
            &axis,
            namespaces,
            prefix,
            "delete",
            bool_value(value, "axis.delete")?,
            axis_order_names(),
        )?;
    }
    if let Some(value) = edit.get("position") {
        let value = match value {
            Value::Null => None,
            Value::String(value) => Some(
                axis_position_code(value)
                    .ok_or_else(|| format!("unsupported axis position {value}"))?,
            ),
            _ => return Err("axis.position must be a string or null".into()),
        };
        axis = set_value_child(
            &axis,
            namespaces,
            prefix,
            "axPos",
            value,
            axis_order_names(),
        )?;
    }
    if let Some(value) = edit.get("title") {
        axis = edit_fragment_title(&axis, namespaces, prefix, value, axis_order_names())?;
    }
    if let Some(value) = edit.get("numberFormat") {
        axis = edit_number_format(&axis, namespaces, prefix, value, axis_order_names())?;
    }
    for (json_name, xml_name) in [
        ("majorGridlines", "majorGridlines"),
        ("minorGridlines", "minorGridlines"),
    ] {
        if let Some(value) = edit.get(json_name) {
            let show = value
                .as_bool()
                .ok_or_else(|| format!("axis.{json_name} must be a boolean"))?;
            let grid = show.then(|| format!("<{prefix}{xml_name}/>"));
            axis = replace_root_child(
                &axis,
                namespaces,
                xml_name,
                grid.as_deref(),
                axis_order_names(),
            )?;
        }
    }
    for (json_name, xml_name) in [
        ("majorTickMark", "majorTickMark"),
        ("minorTickMark", "minorTickMark"),
        ("tickLabelPosition", "tickLblPos"),
        ("crosses", "crosses"),
        ("crossBetween", "crossBetween"),
        ("labelAlignment", "lblAlgn"),
        ("baseTimeUnit", "baseTimeUnit"),
        ("majorTimeUnit", "majorTimeUnit"),
        ("minorTimeUnit", "minorTimeUnit"),
    ] {
        if let Some(value) = edit.get(json_name) {
            let value = match value {
                Value::Null => None,
                Value::String(value) => Some(value.as_str()),
                _ => return Err(format!("axis.{json_name} must be a string or null")),
            };
            axis = set_value_child(
                &axis,
                namespaces,
                prefix,
                xml_name,
                value,
                axis_order_names(),
            )?;
        }
    }
    if let Some(value) = edit.get("auto") {
        axis = set_value_child(
            &axis,
            namespaces,
            prefix,
            "auto",
            bool_value(value, "axis.auto")?,
            axis_order_names(),
        )?;
    }
    if let Some(value) = edit.get("noMultiLevelLabels") {
        axis = set_value_child(
            &axis,
            namespaces,
            prefix,
            "noMultiLvlLbl",
            bool_value(value, "axis.noMultiLevelLabels")?,
            axis_order_names(),
        )?;
    }
    if let Some(value) = edit.get("crossAxisId") {
        let serialized = match value {
            Value::Null => None,
            value => Some(
                value
                    .as_u64()
                    .ok_or("axis.crossAxisId must be a non-negative integer or null")?
                    .to_string(),
            ),
        };
        axis = set_value_child(
            &axis,
            namespaces,
            prefix,
            "crossAx",
            serialized.as_deref(),
            axis_order_names(),
        )?;
    }
    for (json_name, xml_name) in [
        ("crossesAt", "crossesAt"),
        ("labelOffset", "lblOffset"),
        ("tickLabelSkip", "tickLblSkip"),
        ("tickMarkSkip", "tickMarkSkip"),
        ("majorUnit", "majorUnit"),
        ("minorUnit", "minorUnit"),
    ] {
        if let Some(value) = edit.get(json_name) {
            let serialized = number_value(value, &format!("axis.{json_name}"))?;
            axis = set_value_child(
                &axis,
                namespaces,
                prefix,
                xml_name,
                serialized.as_deref(),
                axis_order_names(),
            )?;
            if json_name == "crossesAt" && serialized.is_some() {
                axis = replace_root_child(&axis, namespaces, "crosses", None, axis_order_names())?;
            }
        }
    }
    if edit.get("crosses").is_some() && edit.get("crosses").is_some_and(|value| !value.is_null()) {
        axis = replace_root_child(&axis, namespaces, "crossesAt", None, axis_order_names())?;
    }
    if let Some(value) = edit.get("displayUnits") {
        axis = edit_display_units(&axis, namespaces, prefix, value)?;
    }
    Ok(axis)
}

fn apply_axes(chart_xml: &mut String, axes_edit: Option<&Value>) -> Result<(), String> {
    let Some(edits) = axes_edit else {
        return Ok(());
    };
    let edits = edits.as_array().ok_or("axes must be an array")?;
    let namespaces = root_namespace_context(chart_xml);
    let mut seen = HashSet::new();
    for edit in edits {
        let id = edit
            .get("id")
            .and_then(Value::as_u64)
            .ok_or("axis edit requires a non-negative integer id")?;
        if !seen.insert(id) {
            return Err(format!("duplicate axis edit id {id}"));
        }
        let document = Document::parse(chart_xml).map_err(|error| format!("chart XML: {error}"))?;
        let plot_area = document
            .descendants()
            .find(|node| node.is_element() && local_name(*node) == "plotArea")
            .ok_or("missing plotArea")?;
        let existing = plot_area.children().find(|node| {
            node.is_element()
                && axis_type(local_name(*node)).is_some()
                && child_val(*node, "axId").and_then(|value| value.parse::<u64>().ok()) == Some(id)
        });
        if edit.get("$delete").and_then(Value::as_bool) == Some(true) {
            if let Some(existing) = existing {
                chart_xml.replace_range(existing.range(), "");
            }
            continue;
        }
        let prefix = existing
            .map(|node| qname_prefix(chart_xml, node))
            .unwrap_or_else(|| qname_prefix(chart_xml, plot_area));
        let replacement = edit_axis_fragment(
            existing.map(|node| &chart_xml[node.range()]),
            &namespaces,
            &prefix,
            edit,
        )?;
        if let Some(existing) = existing {
            chart_xml.replace_range(existing.range(), &replacement);
        } else {
            let insert = direct_child(plot_area, "extLst")
                .map(|node| node.range().start)
                .unwrap_or_else(|| {
                    let range = plot_area.range();
                    chart_xml[range.clone()]
                        .rfind("</")
                        .map(|offset| range.start + offset)
                        .unwrap_or(range.end)
                });
            chart_xml.insert_str(insert, &replacement);
        }
    }
    Ok(())
}

fn chart_block_order() -> &'static [&'static str] {
    &[
        "barDir",
        "grouping",
        "radarStyle",
        "scatterStyle",
        "varyColors",
        "ser",
        "dLbls",
        "dropLines",
        "hiLowLines",
        "upDownBars",
        "gapWidth",
        "overlap",
        "serLines",
        "firstSliceAng",
        "holeSize",
        "bubble3D",
        "bubbleScale",
        "showNegBubbles",
        "sizeRepresents",
        "axId",
        "extLst",
    ]
}

fn convert_chart_block(
    block: &str,
    namespaces: &str,
    prefix: &str,
    old: &str,
    new: &str,
) -> Result<String, String> {
    if old == new {
        return Ok(block.to_string());
    }
    let has_series = root_child_ranges(block, namespaces)?
        .iter()
        .any(|(name, _)| name == "ser");
    if !compatible_chart_conversion(old, new) && has_series {
        return Err(format!("unsafe chart type conversion: {old} -> {new}"));
    }
    let local = chart_local(new).ok_or_else(|| format!("unsupported chart type: {new}"))?;
    let mut output = rename_root_element(block, local);
    let remove = [
        "barDir",
        "grouping",
        "radarStyle",
        "scatterStyle",
        "dropLines",
        "hiLowLines",
        "upDownBars",
        "gapWidth",
        "overlap",
        "serLines",
        "firstSliceAng",
        "holeSize",
        "bubble3D",
        "bubbleScale",
        "showNegBubbles",
        "sizeRepresents",
    ];
    for child in remove {
        output = replace_root_child(&output, namespaces, child, None, chart_block_order())?;
    }
    let controls: &[(&str, &str)] = match new {
        "bar" => &[("barDir", "col"), ("grouping", "clustered")],
        "line" | "area" => &[("grouping", "standard")],
        "radar" => &[("radarStyle", "standard")],
        "scatter" => &[("scatterStyle", "lineMarker")],
        _ => &[],
    };
    for (name, value) in controls {
        let child = format!("<{prefix}{name} val=\"{value}\"/>");
        output = replace_root_child(&output, namespaces, name, Some(&child), chart_block_order())?;
    }
    if new == "doughnut" {
        let child = format!("<{prefix}holeSize val=\"50\"/>");
        output = replace_root_child(
            &output,
            namespaces,
            "holeSize",
            Some(&child),
            chart_block_order(),
        )?;
    } else {
        output = replace_root_child(&output, namespaces, "holeSize", None, chart_block_order())?;
    }
    let series_ranges: Vec<Range<usize>> = root_child_ranges(&output, namespaces)?
        .into_iter()
        .filter(|(name, _)| name == "ser")
        .map(|(_, range)| range)
        .collect();
    for range in series_ranges.into_iter().rev() {
        let mut replacement =
            normalize_series_sources(output[range.clone()].to_string(), namespaces, new)?;
        if new == "bubble"
            && !root_child_ranges(&replacement, namespaces)?
                .iter()
                .any(|(name, _)| name == "bubbleSize")
        {
            let (wrapped, _) = wrapped_document(&replacement, namespaces)?;
            let document = Document::parse(&wrapped).map_err(|error| error.to_string())?;
            let series = fragment_root(&document).ok_or("missing converted bubble series")?;
            let model = parse_series(series, "bubble");
            let count = model
                .get("values")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0)
                .max(
                    model
                        .get("categories")
                        .and_then(Value::as_array)
                        .map(Vec::len)
                        .unwrap_or(0),
                );
            let sizes = vec![Value::from(1.0); count];
            replacement = update_data_container(
                &replacement,
                namespaces,
                prefix,
                "bubbleSize",
                "",
                &sizes,
                true,
            )?;
        }
        output.replace_range(range, &replacement);
    }
    Ok(output)
}

fn replace_plot_axis_ids(
    block: &str,
    namespaces: &str,
    prefix: &str,
    axis_ids: &Value,
) -> Result<String, String> {
    let axis_ids = axis_ids.as_array().ok_or("plot.axisIds must be an array")?;
    let mut seen = HashSet::new();
    let mut serialized = String::new();
    for id in axis_ids {
        let id = id
            .as_u64()
            .ok_or("plot.axisIds must contain non-negative integers")?;
        if !seen.insert(id) {
            return Err(format!("duplicate plot axis id {id}"));
        }
        serialized.push_str(&format!("<{prefix}axId val=\"{id}\"/>"));
    }
    let ranges: Vec<Range<usize>> = root_child_ranges(block, namespaces)?
        .into_iter()
        .filter(|(name, _)| name == "axId")
        .map(|(_, range)| range)
        .collect();
    let mut output = block.to_string();
    for range in ranges.into_iter().rev() {
        output.replace_range(range, "");
    }
    if serialized.is_empty() {
        return Ok(output);
    }
    replace_root_child(
        &output,
        namespaces,
        "axId",
        Some(&serialized),
        chart_block_order(),
    )
}

fn edit_plot_block(
    block: &str,
    namespaces: &str,
    prefix: &str,
    current_kind: &str,
    edit: &Value,
) -> Result<String, String> {
    let target_kind = edit
        .get("chartType")
        .and_then(Value::as_str)
        .map(normalize_chart_kind)
        .unwrap_or_else(|| current_kind.to_string());
    let mut output = if target_kind != current_kind {
        convert_chart_block(block, namespaces, prefix, current_kind, &target_kind)?
    } else {
        block.to_string()
    };
    if let Some(axis_ids) = edit.get("axisIds") {
        output = replace_plot_axis_ids(&output, namespaces, prefix, axis_ids)?;
    }
    if let Some(data_labels) = edit.get("dataLabels") {
        let existing = root_child_ranges(&output, namespaces)?
            .into_iter()
            .find(|(name, _)| name == "dLbls")
            .map(|(_, range)| output[range].to_string());
        let replacement =
            edit_data_labels_fragment(existing.as_deref(), namespaces, prefix, data_labels)?;
        output = replace_root_child(
            &output,
            namespaces,
            "dLbls",
            replacement.as_deref(),
            chart_block_order(),
        )?;
    }
    for (json_name, xml_name) in [
        ("grouping", "grouping"),
        ("barDirection", "barDir"),
        ("overlap", "overlap"),
        ("gapWidth", "gapWidth"),
        ("smooth", "smooth"),
        ("varyColors", "varyColors"),
    ] {
        if let Some(value) = edit.get(json_name) {
            let serialized = match value {
                Value::Null => None,
                Value::String(value) => Some(value.to_string()),
                Value::Bool(value) => Some(if *value { "1" } else { "0" }.to_string()),
                Value::Number(value) => Some(value.to_string()),
                _ => return Err(format!("plot.{json_name} has an invalid value")),
            };
            output = set_value_child(
                &output,
                namespaces,
                prefix,
                xml_name,
                serialized.as_deref(),
                chart_block_order(),
            )?;
        }
    }
    Ok(output)
}

fn apply_plots(chart_xml: &mut String, plots_edit: Option<&Value>) -> Result<(), String> {
    let Some(edits) = plots_edit else {
        return Ok(());
    };
    let edits = edits.as_array().ok_or("plots must be an array")?;
    let namespaces = root_namespace_context(chart_xml);
    let mut seen = HashSet::new();
    for edit in edits {
        let index = edit
            .get("index")
            .and_then(Value::as_u64)
            .ok_or("plot edit requires a non-negative integer index")? as usize;
        if !seen.insert(index) {
            return Err(format!("duplicate plot edit index {index}"));
        }
        let document = Document::parse(chart_xml).map_err(|error| format!("chart XML: {error}"))?;
        let plot_area = document
            .descendants()
            .find(|node| node.is_element() && local_name(*node) == "plotArea")
            .ok_or("missing plotArea")?;
        let nodes: Vec<Node<'_, '_>> = plot_area
            .children()
            .filter(|node| node.is_element() && CHART_KINDS.contains(&local_name(*node)))
            .collect();
        if edit.get("$delete").and_then(Value::as_bool) == Some(true) {
            if nodes.len() <= 1 {
                return Err("a chart must retain at least one plot".into());
            }
            let node = nodes.get(index).ok_or("plot index is out of range")?;
            chart_xml.replace_range(node.range(), "");
            continue;
        }
        if let Some(node) = nodes.get(index) {
            let prefix = qname_prefix(chart_xml, *node);
            let current_kind = chart_kind(local_name(*node)).unwrap_or("unknown");
            let replacement = edit_plot_block(
                &chart_xml[node.range()],
                &namespaces,
                &prefix,
                current_kind,
                edit,
            )?;
            chart_xml.replace_range(node.range(), &replacement);
            continue;
        }
        if index != nodes.len() {
            return Err("new plots can only be appended at the next plot index".into());
        }
        let source_index = edit.get("cloneFrom").and_then(Value::as_u64).unwrap_or(0) as usize;
        let source = nodes
            .get(source_index)
            .ok_or("plot.cloneFrom is out of range")?;
        let insert = nodes
            .last()
            .map(|node| node.range().end)
            .ok_or("chart contains no source plot")?;
        let prefix = qname_prefix(chart_xml, *source);
        let current_kind = chart_kind(local_name(*source)).unwrap_or("unknown");
        let mut block = chart_xml[source.range()].to_string();
        let series_ranges: Vec<Range<usize>> = root_child_ranges(&block, &namespaces)?
            .into_iter()
            .filter(|(name, _)| name == "ser")
            .map(|(_, range)| range)
            .collect();
        for range in series_ranges.into_iter().rev() {
            block.replace_range(range, "");
        }
        block = edit_plot_block(&block, &namespaces, &prefix, current_kind, edit)?;
        chart_xml.insert_str(insert, &block);
    }
    Ok(())
}

#[derive(Clone)]
struct SeriesTemplate {
    block: usize,
    raw: String,
    model: Value,
}

fn series_match_score(
    edit: &Value,
    template: &SeriesTemplate,
    position: usize,
    template_position: usize,
) -> i32 {
    let mut score = 0;
    let equal_nonempty = |field: &str| {
        let left = edit.get(field).and_then(Value::as_str).unwrap_or("");
        let right = template
            .model
            .get(field)
            .and_then(Value::as_str)
            .unwrap_or("");
        !left.is_empty() && left == right
    };
    if equal_nonempty("valueFormula") {
        score += 120;
    }
    if equal_nonempty("categoryFormula") {
        score += 70;
    }
    if equal_nonempty("name") {
        score += 30;
    }
    if position == template_position {
        score += 5;
    }
    score
}

fn generic_series(prefix: &str) -> String {
    format!("<{prefix}ser><{prefix}idx val=\"0\"/><{prefix}order val=\"0\"/></{prefix}ser>")
}

fn replace_block_series(
    block: &str,
    namespaces: &str,
    replacements: &[String],
) -> Result<String, String> {
    let children = root_child_ranges(block, namespaces)?;
    let series_ranges: Vec<Range<usize>> = children
        .iter()
        .filter(|(name, _)| name == "ser")
        .map(|(_, range)| range.clone())
        .collect();
    let insert = series_ranges
        .first()
        .map(|range| range.start)
        .or_else(|| {
            children
                .iter()
                .find(|(name, _)| series_order(name) > series_order("ser"))
                .map(|(_, range)| range.start)
        })
        .unwrap_or_else(|| block.rfind("</").unwrap_or(block.len()));
    let mut output = block.to_string();
    for range in series_ranges.into_iter().rev() {
        output.replace_range(range, "");
    }
    output.insert_str(insert, &replacements.join(""));
    Ok(output)
}

fn apply_series_and_type(chart_xml: &mut String, edit: &Value) -> Result<(), String> {
    let requested_series = edit.get("series").and_then(Value::as_array);
    let requested_kind = edit
        .get("chartType")
        .and_then(Value::as_str)
        .map(normalize_chart_kind);
    if requested_series.is_none() && requested_kind.is_none() {
        return Ok(());
    }
    let namespaces = root_namespace_context(chart_xml);
    let document = Document::parse(chart_xml).map_err(|error| format!("chart XML: {error}"))?;
    let chart = document
        .descendants()
        .find(|node| node.is_element() && local_name(*node) == "chart")
        .ok_or("missing chart element")?;
    let plot_area = direct_child(chart, "plotArea").ok_or("missing plotArea")?;
    let nodes: Vec<Node<'_, '_>> = plot_area
        .children()
        .filter(|node| node.is_element() && CHART_KINDS.contains(&local_name(*node)))
        .collect();
    if nodes.is_empty() {
        return Err("chart contains no supported plot".into());
    }
    let combo = nodes.len() > 1;
    let current_kind = if combo {
        "combo".to_string()
    } else {
        chart_kind(local_name(nodes[0]))
            .unwrap_or("unknown")
            .to_string()
    };
    let target_kind = requested_kind.unwrap_or_else(|| current_kind.clone());
    if combo && target_kind != "combo" {
        return Err(
            "converting a combo chart to a single chart requires an explicit plot assignment"
                .into(),
        );
    }
    if !combo && target_kind == "combo" {
        return Err("creating a combo chart requires a per-series plot type".into());
    }

    let chart_prefix = qname_prefix(chart_xml, nodes[0]);
    let mut blocks: Vec<String> = nodes
        .iter()
        .map(|node| chart_xml[node.range()].to_string())
        .collect();
    if !combo && target_kind != current_kind {
        blocks[0] = convert_chart_block(
            &blocks[0],
            &namespaces,
            &chart_prefix,
            &current_kind,
            &target_kind,
        )?;
    }
    let block_kinds: Vec<String> = if combo {
        nodes
            .iter()
            .map(|node| {
                chart_kind(local_name(*node))
                    .unwrap_or("unknown")
                    .to_string()
            })
            .collect()
    } else {
        vec![target_kind.clone()]
    };
    let axes: Vec<Value> = plot_area
        .children()
        .filter(|node| node.is_element() && axis_type(local_name(*node)).is_some())
        .map(parse_axis)
        .collect();
    let block_axis_ids: Vec<Vec<Value>> = nodes.iter().map(|node| plot_axis_ids(*node)).collect();
    let block_axis_groups: Vec<&str> = block_axis_ids
        .iter()
        .map(|ids| inferred_axis_group(ids, &axes))
        .collect();

    if let Some(edits) = requested_series {
        let mut templates = Vec::new();
        for (block_index, block) in blocks.iter().enumerate() {
            let (wrapped, _) = wrapped_document(block, &namespaces)?;
            let document = Document::parse(&wrapped).map_err(|error| error.to_string())?;
            let root = fragment_root(&document).ok_or("missing chart block")?;
            for series in direct_children(root, "ser") {
                let raw = wrapped[series.range()].to_string();
                templates.push(SeriesTemplate {
                    block: block_index,
                    model: parse_series(series, &block_kinds[block_index]),
                    raw,
                });
            }
        }
        let mut used = HashSet::new();
        let mut assignments: Vec<(usize, usize, String)> = Vec::new();
        for (position, series_edit) in edits.iter().enumerate() {
            let requested_plot_index = series_edit
                .get("plotIndex")
                .and_then(Value::as_u64)
                .map(|index| index as usize);
            let requested_plot_type = series_edit
                .get("plotType")
                .and_then(Value::as_str)
                .map(normalize_chart_kind);
            let requested_axis_ids = series_edit.get("axisIds").and_then(Value::as_array);
            let requested_axis_group = series_edit.get("axisGroup").and_then(Value::as_str);
            let explicit_block = if let Some(index) = requested_plot_index {
                if index >= blocks.len() {
                    return Err(format!("series plotIndex {index} is out of range"));
                }
                Some(index)
            } else if requested_plot_type.is_some()
                || requested_axis_ids.is_some()
                || requested_axis_group.is_some()
            {
                (0..blocks.len()).find(|index| {
                    requested_plot_type
                        .as_ref()
                        .is_none_or(|kind| &block_kinds[*index] == kind)
                        && requested_axis_ids.is_none_or(|ids| ids == &block_axis_ids[*index])
                        && requested_axis_group
                            .is_none_or(|group| group == block_axis_groups[*index])
                })
            } else {
                None
            };
            if (requested_plot_type.is_some()
                || requested_axis_ids.is_some()
                || requested_axis_group.is_some())
                && explicit_block.is_none()
            {
                return Err("no chart plot matches the requested series plot assignment".into());
            }
            if let Some(index) = explicit_block {
                if requested_plot_type
                    .as_ref()
                    .is_some_and(|kind| kind != &block_kinds[index])
                {
                    return Err("series plotType does not match plotIndex".into());
                }
            }
            let mut best: Option<(usize, i32)> = None;
            for (template_index, template) in templates.iter().enumerate() {
                if used.contains(&template_index) {
                    continue;
                }
                let score = series_match_score(series_edit, template, position, template_index);
                if best.map(|(_, current)| score > current).unwrap_or(true) {
                    best = Some((template_index, score));
                }
            }
            let template_index = best.map(|(index, _)| index);
            if let Some(index) = template_index {
                used.insert(index);
            }
            let block_index = explicit_block
                .or_else(|| template_index.map(|index| templates[index].block))
                .or_else(|| assignments.last().map(|(_, block, _)| *block))
                .unwrap_or(0);
            let template = template_index
                .map(|index| templates[index].raw.clone())
                .or_else(|| {
                    templates
                        .iter()
                        .find(|template| template.block == block_index)
                        .map(|template| template.raw.clone())
                })
                .unwrap_or_else(|| generic_series(&chart_prefix));
            let edited = edit_series_fragment(
                &template,
                &namespaces,
                &chart_prefix,
                &block_kinds[block_index],
                series_edit,
                position,
            )?;
            assignments.push((position, block_index, edited));
        }
        for (block_index, block) in blocks.iter_mut().enumerate() {
            let replacements: Vec<String> = assignments
                .iter()
                .filter(|(_, assigned_block, _)| *assigned_block == block_index)
                .map(|(_, _, raw)| raw.clone())
                .collect();
            *block = replace_block_series(block, &namespaces, &replacements)?;
        }
    }

    let mut replacements: Vec<(Range<usize>, String)> = nodes
        .iter()
        .zip(blocks)
        .map(|(node, block)| (node.range(), block))
        .collect();
    replacements.sort_by(|left, right| right.0.start.cmp(&left.0.start));
    for (range, replacement) in replacements {
        chart_xml.replace_range(range, &replacement);
    }
    Ok(())
}

fn validate_chart_integrity(chart_xml: &str) -> Result<(), String> {
    let document =
        Document::parse(chart_xml).map_err(|error| format!("edited chart XML: {error}"))?;
    let plot_area = document
        .descendants()
        .find(|node| node.is_element() && local_name(*node) == "plotArea")
        .ok_or("missing plotArea")?;
    let mut axis_ids = HashSet::new();
    for axis in plot_area
        .children()
        .filter(|node| node.is_element() && axis_type(local_name(*node)).is_some())
    {
        let id = child_val(axis, "axId")
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or("chart axis is missing a valid axId")?;
        if !axis_ids.insert(id) {
            return Err(format!("duplicate chart axis id {id}"));
        }
    }
    for plot in plot_area
        .children()
        .filter(|node| node.is_element() && CHART_KINDS.contains(&local_name(*node)))
    {
        for id in direct_children(plot, "axId").filter_map(|node| node.attribute("val")) {
            let id = id
                .parse::<u64>()
                .map_err(|_| "plot contains an invalid axis id")?;
            if !axis_ids.contains(&id) {
                return Err(format!("plot references missing chart axis {id}"));
            }
        }
    }
    for axis in plot_area
        .children()
        .filter(|node| node.is_element() && axis_type(local_name(*node)).is_some())
    {
        if let Some(cross) = child_val(axis, "crossAx") {
            let cross = cross
                .parse::<u64>()
                .map_err(|_| "axis contains an invalid crossAx")?;
            if !axis_ids.contains(&cross) {
                return Err(format!("axis references missing cross axis {cross}"));
            }
        }
    }
    Ok(())
}

/// Apply a differential edit to an OOXML chart part.
///
/// Top-level patches may edit `chartType`, `title`, `legend`, `plots`, `axes`, and `series`.
/// `plots` are addressed by `index` and expose chart type, axis IDs, plot data labels and common
/// grouping/direction controls; a plot at the next index may clone an existing compatible plot to
/// create an editable combination chart. `axes` are addressed by stable OOXML `axId` and expose
/// scaling, bounds, ticks, gridlines, number format, crossing, display units and titles. Use
/// `$delete=true` to remove a plot/axis, while `delete` edits Excel's visible-axis flag.
///
/// A series may choose `plotIndex`/`plotType`/`axisIds`/`axisGroup`,
/// `bindingMode=reference|embedded`, common `pointOverrides`, data labels, trendlines and error
/// bars. Missing fields are untouched. Existing fragments are matched and edited child-by-child,
/// retaining unknown attributes, `extLst`, DrawingML effects and vendor XML. The completed part is
/// parsed and cross-axis references are validated before it is returned, making a failed patch
/// transactional from the caller's perspective.
pub fn apply_chart_edit(chart_xml: &str, edit: &Value) -> Result<String, String> {
    Document::parse(chart_xml).map_err(|error| format!("chart XML: {error}"))?;
    let mut output = chart_xml.to_string();
    apply_title(&mut output, edit.get("title"))?;
    apply_legend(&mut output, edit.get("legend"))?;
    apply_plots(&mut output, edit.get("plots"))?;
    apply_axes(&mut output, edit.get("axes"))?;
    apply_series_and_type(&mut output, edit)?;
    validate_chart_integrity(&output)?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHART: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:x="urn:vendor">
  <c:chart>
    <c:title><c:tx><c:rich><a:bodyPr/><a:lstStyle/><a:p><a:r><a:t>Old title</a:t></a:r></a:p></c:rich></c:tx><c:overlay val="0"/><x:titleExt keep="yes"/></c:title>
    <c:plotArea><c:layout/><c:barChart>
      <c:barDir val="col"/><c:grouping val="clustered"/>
      <c:ser><c:idx val="4"/><c:order val="4"/><c:tx><c:v>North</c:v></c:tx><c:spPr><a:solidFill><a:schemeClr val="accent1"><a:lumMod val="65000"/></a:schemeClr></a:solidFill><x:effect keep="1"/></c:spPr><x:unknown keep="north"/><c:cat><c:strRef><c:f>Sheet1!$A$2:$A$3</c:f><c:strCache><c:ptCount val="2"/><c:pt idx="0"><c:v>A</c:v></c:pt><c:pt idx="1"><c:v>B</c:v></c:pt><x:cacheExt keep="yes"/></c:strCache></c:strRef></c:cat><c:val><c:numRef><c:f>Sheet1!$B$2:$B$3</c:f><c:numCache><c:formatCode>0.00</c:formatCode><c:ptCount val="2"/><c:pt idx="0"><c:v>1</c:v></c:pt><c:pt idx="1"><c:v>2</c:v></c:pt><x:cacheExt keep="yes"/></c:numCache></c:numRef></c:val></c:ser>
      <c:ser><c:idx val="9"/><c:order val="9"/><c:tx><c:v>South</c:v></c:tx><x:unknown keep="south"/><c:cat><c:strRef><c:f>Sheet1!$A$2:$A$3</c:f><c:strCache><c:ptCount val="2"/><c:pt idx="0"><c:v>A</c:v></c:pt><c:pt idx="1"><c:v>B</c:v></c:pt></c:strCache></c:strRef></c:cat><c:val><c:numRef><c:f>Sheet1!$C$2:$C$3</c:f><c:numCache><c:ptCount val="2"/><c:pt idx="0"><c:v>3</c:v></c:pt><c:pt idx="1"><c:v>4</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser>
      <c:axId val="10"/><c:axId val="20"/><x:blockExt keep="yes"/>
    </c:barChart><c:catAx><c:axId val="10"/></c:catAx><c:valAx><c:axId val="20"/></c:valAx></c:plotArea>
    <c:legend><c:legendPos val="r"/><c:layout/><x:legendExt keep="yes"/></c:legend><c:plotVisOnly val="1"/><x:chartExt keep="yes"/>
  </c:chart>
</c:chartSpace>"#;

    fn minimal_chart(local: &str, sources: &str) -> String {
        format!(
            r#"<c:chartSpace xmlns:c="{CHART_NS}"><c:chart><c:plotArea><c:{local}><c:ser><c:idx val="0"/><c:order val="0"/>{sources}</c:ser></c:{local}></c:plotArea></c:chart></c:chartSpace>"#
        )
    }

    #[test]
    fn parses_supported_model() {
        let model = parse_chart_model(CHART);
        assert_eq!(model["chartType"], "bar");
        assert_eq!(model["title"], "Old title");
        assert_eq!(model["legend"]["show"], true);
        assert_eq!(model["legend"]["position"], "right");
        assert_eq!(model["series"].as_array().unwrap().len(), 2);
        assert_eq!(model["series"][0]["name"], "North");
        assert_eq!(model["series"][0]["values"], json!([1.0, 2.0]));
        assert_eq!(model["series"][0]["color"], "scheme:accent1");
        assert_eq!(model["series"][0]["colorSpec"]["type"], "scheme");
        assert_eq!(model["series"][0]["colorSpec"]["value"], "accent1");
        assert_eq!(
            model["series"][0]["colorSpec"]["transforms"][0]["type"],
            "lumMod"
        );
        assert_eq!(
            model["series"][0]["colorSpec"]["transforms"][0]["value"],
            65_000.0
        );
    }

    #[test]
    fn recognizes_every_supported_plot_and_combo() {
        let standard = "<c:cat><c:strLit><c:ptCount val=\"0\"/></c:strLit></c:cat><c:val><c:numLit><c:ptCount val=\"0\"/></c:numLit></c:val>";
        let xy = "<c:xVal><c:numLit><c:ptCount val=\"0\"/></c:numLit></c:xVal><c:yVal><c:numLit><c:ptCount val=\"0\"/></c:numLit></c:yVal>";
        for (local, kind, sources) in [
            ("barChart", "bar", standard),
            ("lineChart", "line", standard),
            ("pieChart", "pie", standard),
            ("pie3DChart", "pie", standard),
            ("doughnutChart", "doughnut", standard),
            ("areaChart", "area", standard),
            ("scatterChart", "scatter", xy),
            ("bubbleChart", "bubble", xy),
            ("radarChart", "radar", standard),
            ("stockChart", "stock", standard),
        ] {
            assert_eq!(
                parse_chart_model(&minimal_chart(local, sources))["chartType"],
                kind
            );
        }
        let combo = format!(
            r#"<c:chartSpace xmlns:c="{CHART_NS}"><c:chart><c:plotArea><c:barChart/><c:lineChart/></c:plotArea></c:chart></c:chartSpace>"#
        );
        assert_eq!(parse_chart_model(&combo)["chartType"], "combo");
    }

    #[test]
    fn title_and_legend_are_differential() {
        let edited = apply_chart_edit(
            CHART,
            &json!({"title":"Revenue", "legend":{"show":true,"position":"bottom"}}),
        )
        .unwrap();
        assert!(edited.contains(">Revenue<"));
        assert!(edited.contains("legendPos val=\"b\""));
        assert!(edited.contains("<x:titleExt keep=\"yes\"/>"));
        assert!(edited.contains("<x:legendExt keep=\"yes\"/>"));
        assert!(edited.contains("<x:chartExt keep=\"yes\"/>"));
    }

    #[test]
    fn explicit_series_colour_drops_transforms_from_the_previous_theme_colour() {
        let edited = apply_chart_edit(
            CHART,
            &json!({"series":[{"name":"North","color":"#FF0000"}]}),
        )
        .unwrap();
        assert!(edited.contains("srgbClr"));
        assert!(edited.contains("val=\"FF0000\""));
        assert!(!edited.contains("lumMod val=\"65000\""));
        assert!(edited.contains("<x:effect keep=\"1\"/>"));
    }

    #[test]
    fn line_series_colour_targets_the_line_not_an_unrelated_shape_fill() {
        let source = format!(
            r#"<c:chartSpace xmlns:c="{CHART_NS}" xmlns:a="{DRAWING_NS}"><c:chart><c:plotArea><c:lineChart><c:ser><c:idx val="0"/><c:order val="0"/><c:spPr><a:solidFill><a:srgbClr val="ABCDEF"/></a:solidFill><a:ln><a:solidFill><a:schemeClr val="accent1"/></a:solidFill></a:ln></c:spPr><c:cat><c:strLit><c:ptCount val="0"/></c:strLit></c:cat><c:val><c:numLit><c:ptCount val="0"/></c:numLit></c:val></c:ser></c:lineChart></c:plotArea></c:chart></c:chartSpace>"#
        );
        assert_eq!(
            parse_chart_model(&source)["series"][0]["color"],
            "scheme:accent1"
        );
        let edited = apply_chart_edit(&source, &json!({"series":[{"color":"#123456"}]})).unwrap();
        assert!(edited.contains("val=\"ABCDEF\""));
        assert!(edited.contains("val=\"123456\""));
        assert!(!edited.contains("schemeClr val=\"accent1\""));
    }

    #[test]
    fn reorders_deletes_adds_and_synchronizes_caches() {
        let edited = apply_chart_edit(
            CHART,
            &json!({"series":[
                {"name":"South","categoryFormula":"Sheet1!$A$2:$A$4","valueFormula":"Sheet1!$C$2:$C$4","categories":["A","B","C"],"values":[30,null,50],"color":"#FF0000"},
                {"name":"New","categoryFormula":"Sheet1!$A$2:$A$4","valueFormula":"Sheet1!$D$2:$D$4","categories":["A","B","C"],"values":[7,8,9],"color":"scheme:accent2"}
            ]}),
        )
        .unwrap();
        let parsed = parse_chart_model(&edited);
        assert_eq!(parsed["series"].as_array().unwrap().len(), 2);
        assert_eq!(parsed["series"][0]["name"], "South");
        assert_eq!(parsed["series"][0]["values"], json!([30.0, null, 50.0]));
        assert_eq!(parsed["series"][1]["name"], "New");
        assert!(edited.contains("<c:idx val=\"0\"/>"));
        assert!(edited.contains("<c:order val=\"1\"/>"));
        assert!(edited.contains("<c:ptCount val=\"3\"/>"));
        assert!(edited.contains("<x:unknown keep=\"south\"/>"));
        assert!(edited.contains("<x:cacheExt keep=\"yes\"/>"));
        assert!(edited.contains("<x:blockExt keep=\"yes\"/>"));
    }

    #[test]
    fn compatible_type_conversion_keeps_unknown_nodes() {
        let edited = apply_chart_edit(CHART, &json!({"chartType":"line"})).unwrap();
        assert!(edited.contains("<c:lineChart>"));
        assert!(edited.contains("<c:grouping val=\"standard\"/>"));
        assert!(edited.contains("<x:blockExt keep=\"yes\"/>"));
        assert!(edited.contains("<c:cat>"));
        assert!(edited.contains("<c:val>"));
    }

    #[test]
    fn scatter_to_bubble_adds_required_size_cache() {
        let xy = "<c:xVal><c:numLit><c:ptCount val=\"2\"/><c:pt idx=\"0\"><c:v>1</c:v></c:pt><c:pt idx=\"1\"><c:v>2</c:v></c:pt></c:numLit></c:xVal><c:yVal><c:numLit><c:ptCount val=\"2\"/><c:pt idx=\"0\"><c:v>3</c:v></c:pt><c:pt idx=\"1\"><c:v>4</c:v></c:pt></c:numLit></c:yVal>";
        let source = minimal_chart("scatterChart", xy);
        let edited = apply_chart_edit(&source, &json!({"chartType":"bubble"})).unwrap();
        assert!(edited.contains("<c:bubbleChart>"));
        assert!(edited.contains("<c:bubbleSize><c:numLit><c:ptCount val=\"2\"/>"));
    }

    #[test]
    fn numeric_category_source_stays_numeric() {
        let sources = "<c:cat><c:numRef><c:f>Sheet1!$A$1:$A$2</c:f><c:numCache><c:ptCount val=\"2\"/><c:pt idx=\"0\"><c:v>45100</c:v></c:pt><c:pt idx=\"1\"><c:v>45101</c:v></c:pt></c:numCache></c:numRef></c:cat><c:val><c:numLit><c:ptCount val=\"2\"/><c:pt idx=\"0\"><c:v>1</c:v></c:pt><c:pt idx=\"1\"><c:v>2</c:v></c:pt></c:numLit></c:val>";
        let source = minimal_chart("lineChart", sources);
        let edited = apply_chart_edit(
            &source,
            &json!({"series":[{"categoryFormula":"Sheet1!$B$1:$B$2","categories":[45200,45201]}]}),
        )
        .unwrap();
        assert!(edited.contains("<c:numRef><c:f>Sheet1!$B$1:$B$2</c:f>"));
        assert!(edited.contains("<c:v>45200</c:v>"));
        assert!(!edited.contains("<c:strRef>"));
    }

    #[test]
    fn binding_mode_is_explicit_and_literal_switch_removes_references() {
        assert_eq!(
            parse_chart_model(CHART)["series"][0]["bindingMode"],
            "reference"
        );
        let edited = apply_chart_edit(
            CHART,
            &json!({"series":[{
                "name":"North",
                "bindingMode":"embedded",
                "categories":["Local A","Local B"],
                "values":[11,22]
            }]}),
        )
        .unwrap();
        let model = parse_chart_model(&edited);
        assert_eq!(model["series"][0]["bindingMode"], "embedded");
        assert_eq!(
            model["series"][0]["categories"],
            json!(["Local A", "Local B"])
        );
        assert_eq!(model["series"][0]["values"], json!([11.0, 22.0]));
        assert!(!edited.contains("<c:f>"));
        assert!(!edited.contains("<c:strRef>"));
        assert!(!edited.contains("<c:numRef>"));
        assert!(edited.contains("<c:strLit>"));
        assert!(edited.contains("<c:numLit>"));
    }

    #[test]
    fn reference_mode_rejects_cache_only_edits_and_empty_formulas() {
        let cache_error = apply_chart_edit(
            CHART,
            &json!({"series":[{
                "name":"North",
                "bindingMode":"reference",
                "categoryFormula":"Sheet1!$A$2:$A$3",
                "valueFormula":"Sheet1!$B$2:$B$3",
                "categories":["A","B"],
                "values":[99,100]
            }]}),
        )
        .unwrap_err();
        assert!(cache_error.contains("referenced cells"));

        let formula_error = apply_chart_edit(
            CHART,
            &json!({"series":[{
                "name":"North",
                "bindingMode":"reference",
                "categoryFormula":"",
                "valueFormula":"Sheet1!$B$2:$B$3"
            }]}),
        )
        .unwrap_err();
        assert!(formula_error.contains("require both"));

        // Legacy/differential clients that clear a formula without bindingMode still get a
        // legal literal source, never numRef/strRef containing an empty c:f.
        let legacy = apply_chart_edit(
            CHART,
            &json!({"series":[{"name":"North","valueFormula":"","values":[7,8]}]}),
        )
        .unwrap();
        assert!(legacy.contains("<c:val><c:numLit>"));
        assert!(!legacy.contains("<c:val><c:numRef><c:f></c:f>"));
        assert_eq!(
            parse_chart_model(&legacy)["series"][0]["bindingMode"],
            "mixed"
        );
    }

    #[test]
    fn literal_and_reference_binding_round_trip_for_xy_charts() {
        let xy = "<c:xVal><c:numLit><c:ptCount val=\"2\"/><c:pt idx=\"0\"><c:v>1</c:v></c:pt><c:pt idx=\"1\"><c:v>2</c:v></c:pt></c:numLit></c:xVal><c:yVal><c:numLit><c:ptCount val=\"2\"/><c:pt idx=\"0\"><c:v>3</c:v></c:pt><c:pt idx=\"1\"><c:v>4</c:v></c:pt></c:numLit></c:yVal>";
        let source = minimal_chart("scatterChart", xy);
        assert_eq!(
            parse_chart_model(&source)["series"][0]["bindingMode"],
            "embedded"
        );
        let referenced = apply_chart_edit(
            &source,
            &json!({"series":[{
                "bindingMode":"reference",
                "categoryFormula":"Sheet1!$A$1:$A$2",
                "valueFormula":"Sheet1!$B$1:$B$2"
            }]}),
        )
        .unwrap();
        assert!(referenced.contains("<c:xVal><c:numRef><c:f>Sheet1!$A$1:$A$2</c:f>"));
        assert!(referenced.contains("<c:yVal><c:numRef><c:f>Sheet1!$B$1:$B$2</c:f>"));
        assert_eq!(
            parse_chart_model(&referenced)["series"][0]["bindingMode"],
            "reference"
        );
    }

    #[test]
    fn edits_common_data_point_overrides_without_flattening_extensions() {
        let source = format!(
            r#"<c:chartSpace xmlns:c="{CHART_NS}" xmlns:a="{DRAWING_NS}" xmlns:x="urn:keep"><c:chart><c:plotArea><c:pieChart><c:ser><c:idx val="0"/><c:order val="0"/><c:dPt><c:idx val="1"/><c:marker><c:symbol val="square"/><c:size val="6"/><x:markerExt keep="yes"/></c:marker><c:explosion val="10"/><c:spPr><a:solidFill><a:schemeClr val="accent1"><a:tint val="20000"/></a:schemeClr></a:solidFill><x:pointExt keep="yes"/></c:spPr></c:dPt><c:cat><c:strLit><c:ptCount val="2"/><c:pt idx="0"><c:v>A</c:v></c:pt><c:pt idx="1"><c:v>B</c:v></c:pt></c:strLit></c:cat><c:val><c:numLit><c:ptCount val="2"/><c:pt idx="0"><c:v>1</c:v></c:pt><c:pt idx="1"><c:v>2</c:v></c:pt></c:numLit></c:val></c:ser></c:pieChart></c:plotArea></c:chart></c:chartSpace>"#
        );
        let parsed = parse_chart_model(&source);
        assert_eq!(parsed["series"][0]["pointOverrides"][0]["index"], 1);
        assert_eq!(
            parsed["series"][0]["pointOverrides"][0]["markerSymbol"],
            "square"
        );
        let edited = apply_chart_edit(
            &source,
            &json!({"series":[{"pointOverrides":[
                {"index":1,"color":"#FF0000","explosion":25,"markerSymbol":"circle","markerSize":8},
                {"index":0,"color":"scheme:accent2","explosion":null,"markerSymbol":"","markerSize":null}
            ]}]}),
        )
        .unwrap();
        let model = parse_chart_model(&edited);
        assert_eq!(
            model["series"][0]["pointOverrides"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(model["series"][0]["pointOverrides"][0]["color"], "#FF0000");
        assert_eq!(model["series"][0]["pointOverrides"][0]["explosion"], 25);
        assert_eq!(
            model["series"][0]["pointOverrides"][0]["markerSymbol"],
            "circle"
        );
        assert_eq!(model["series"][0]["pointOverrides"][0]["markerSize"], 8);
        assert!(edited.contains("<x:markerExt keep=\"yes\"/>"));
        assert!(edited.contains("<x:pointExt keep=\"yes\"/>"));
        assert!(!edited.contains("tint val=\"20000\""));
    }

    fn advanced_combo_chart() -> String {
        format!(
            r#"<c:chartSpace xmlns:c="{CHART_NS}" xmlns:a="{DRAWING_NS}" xmlns:x="urn:vendor"><c:chart><c:plotArea><c:layout/>
<c:barChart><c:barDir val="col"/><c:grouping val="clustered"/><c:ser><c:idx val="0"/><c:order val="0"/><c:tx><c:v>Bars</c:v></c:tx>
<c:dLbls><c:numFmt formatCode="0.0" sourceLinked="0"/><c:showVal val="1"/><c:separator>; </c:separator><x:labels keep="yes"/></c:dLbls>
<c:trendline><c:name>Forecast</c:name><c:trendlineType val="linear"/><c:forward val="1"/><c:dispEq val="1"/><x:trend keep="yes"/></c:trendline>
<c:errBars><c:errDir val="y"/><c:errBarType val="both"/><c:errValType val="fixedVal"/><c:noEndCap val="0"/><c:val val="2"/><x:error keep="yes"/></c:errBars>
<c:cat><c:strLit><c:ptCount val="2"/><c:pt idx="0"><c:v>A</c:v></c:pt><c:pt idx="1"><c:v>B</c:v></c:pt></c:strLit></c:cat><c:val><c:numLit><c:ptCount val="2"/><c:pt idx="0"><c:v>1</c:v></c:pt><c:pt idx="1"><c:v>2</c:v></c:pt></c:numLit></c:val><x:series keep="yes"/></c:ser><c:dLbls><c:showCatName val="1"/><x:plotLabels keep="yes"/></c:dLbls><c:gapWidth val="150"/><c:axId val="10"/><c:axId val="20"/><x:bar keep="yes"/></c:barChart>
<c:lineChart><c:grouping val="standard"/><c:ser><c:idx val="1"/><c:order val="1"/><c:tx><c:v>Line</c:v></c:tx><c:cat><c:strLit><c:ptCount val="2"/><c:pt idx="0"><c:v>A</c:v></c:pt><c:pt idx="1"><c:v>B</c:v></c:pt></c:strLit></c:cat><c:val><c:numLit><c:ptCount val="2"/><c:pt idx="0"><c:v>5</c:v></c:pt><c:pt idx="1"><c:v>6</c:v></c:pt></c:numLit></c:val></c:ser><c:axId val="30"/><c:axId val="40"/><x:line keep="yes"/></c:lineChart>
<c:catAx><c:axId val="10"/><c:scaling><c:orientation val="minMax"/></c:scaling><c:axPos val="b"/><c:crossAx val="20"/><c:crosses val="autoZero"/><x:axis keep="cat"/></c:catAx>
<c:valAx><c:axId val="20"/><c:scaling><c:min val="0"/><c:max val="10"/><x:scale keep="yes"/></c:scaling><c:axPos val="l"/><c:majorGridlines><x:grid keep="yes"/></c:majorGridlines><c:title><c:tx><c:rich><a:bodyPr/><a:lstStyle/><a:p><a:r><a:rPr b="1"/><a:t>Old Axis</a:t></a:r></a:p></c:rich><x:tx keep="yes"/></c:tx><x:title keep="yes"/></c:title><c:numFmt formatCode="0.00" sourceLinked="0"/><c:crossAx val="10"/><c:crosses val="autoZero"/><c:dispUnits><c:builtInUnit val="thousands"/><x:units keep="yes"/></c:dispUnits><x:axis keep="value"/></c:valAx>
<c:catAx><c:axId val="30"/><c:scaling/><c:axPos val="t"/><c:crossAx val="40"/><c:crosses val="max"/></c:catAx>
<c:valAx><c:axId val="40"/><c:scaling/><c:axPos val="r"/><c:crossAx val="30"/><c:crosses val="max"/></c:valAx>
<x:plotArea keep="yes"/></c:plotArea></c:chart></c:chartSpace>"#
        )
    }

    #[test]
    fn parses_axes_combo_assignment_labels_trendlines_and_error_bars() {
        let model = parse_chart_model(&advanced_combo_chart());
        assert_eq!(model["chartType"], "combo");
        assert_eq!(model["plots"].as_array().unwrap().len(), 2);
        assert_eq!(model["plots"][1]["axisGroup"], "secondary");
        assert_eq!(model["axes"].as_array().unwrap().len(), 4);
        assert_eq!(model["axes"][1]["scaling"]["max"], 10.0);
        assert_eq!(model["axes"][1]["numberFormat"]["code"], "0.00");
        assert_eq!(model["axes"][1]["displayUnits"]["builtIn"], "thousands");
        assert_eq!(model["series"][0]["plotIndex"], 0);
        assert_eq!(model["series"][1]["plotIndex"], 1);
        assert_eq!(model["series"][1]["axisGroup"], "secondary");
        assert_eq!(model["series"][0]["dataLabels"]["showValue"], true);
        assert_eq!(model["series"][0]["trendlines"][0]["type"], "linear");
        assert_eq!(model["series"][0]["errorBars"][0]["value"], 2.0);
    }

    #[test]
    fn deep_chart_patch_is_differential_and_preserves_vendor_xml() {
        let source = advanced_combo_chart();
        let edited = apply_chart_edit(
            &source,
            &json!({
                "plots":[{"index":0,"gapWidth":120,"dataLabels":{"showCategoryName":false,"showValue":true}}],
                "axes":[{"id":20,"title":"Revenue","scaling":{"min":-5,"max":25,"logBase":null},"majorGridlines":false,"majorUnit":5,"numberFormat":{"code":"$#,##0","sourceLinked":false},"displayUnits":{"builtIn":"millions","showLabel":true}}],
                "series":[
                    {"name":"Bars","plotIndex":0,"dataLabels":{"showValue":false,"position":"outsideEnd","separator":" | "},"trendlines":[{"index":0,"type":"poly","order":3,"forward":2,"displayRSquared":true}],"errorBars":[{"index":0,"direction":"y","barType":"plus","valueType":"percentage","value":10,"noEndCap":true}]},
                    {"name":"Line","plotIndex":1}
                ]
            }),
        )
        .unwrap();
        let model = parse_chart_model(&edited);
        assert_eq!(model["axes"][1]["title"], "Revenue");
        assert_eq!(model["axes"][1]["scaling"]["min"], -5.0);
        assert_eq!(model["axes"][1]["scaling"]["max"], 25.0);
        assert_eq!(model["axes"][1]["majorGridlines"], false);
        assert_eq!(model["axes"][1]["displayUnits"]["builtIn"], "millions");
        assert_eq!(model["series"][0]["dataLabels"]["position"], "outsideEnd");
        assert_eq!(model["series"][0]["trendlines"][0]["type"], "poly");
        assert_eq!(model["series"][0]["trendlines"][0]["order"], 3.0);
        assert_eq!(model["series"][0]["errorBars"][0]["barType"], "plus");
        for keep in [
            "<x:labels keep=\"yes\"/>",
            "<x:trend keep=\"yes\"/>",
            "<x:error keep=\"yes\"/>",
            "<x:plotLabels keep=\"yes\"/>",
            "<x:scale keep=\"yes\"/>",
            "<a:rPr b=\"1\"/>",
            "<x:tx keep=\"yes\"/>",
            "<x:title keep=\"yes\"/>",
            "<x:units keep=\"yes\"/>",
            "<x:axis keep=\"value\"/>",
            "<x:plotArea keep=\"yes\"/>",
        ] {
            assert!(edited.contains(keep), "lost vendor fragment {keep}");
        }
    }

    #[test]
    fn creates_secondary_axes_and_a_native_editable_combo_plot() {
        let edited = apply_chart_edit(
            CHART,
            &json!({
                "chartType":"combo",
                "plots":[{"index":1,"cloneFrom":0,"chartType":"line","axisIds":[30,40]}],
                "axes":[
                    {"id":30,"axisType":"category","position":"top","crossAxisId":40,"crosses":"max"},
                    {"id":40,"axisType":"value","position":"right","crossAxisId":30,"crosses":"max","scaling":{"min":0,"max":100}}
                ],
                "series":[
                    {"name":"North","plotIndex":0},
                    {"name":"South","plotIndex":1,"plotType":"line","axisGroup":"secondary"}
                ]
            }),
        )
        .unwrap();
        let model = parse_chart_model(&edited);
        assert_eq!(model["chartType"], "combo");
        assert_eq!(model["plots"].as_array().unwrap().len(), 2);
        assert_eq!(model["plots"][1]["chartType"], "line");
        assert_eq!(model["series"][1]["name"], "South");
        assert_eq!(model["series"][1]["plotIndex"], 1);
        assert_eq!(model["series"][1]["axisGroup"], "secondary");
        assert!(edited.contains("<x:unknown keep=\"south\"/>"));
        assert!(edited.contains("<x:blockExt keep=\"yes\"/>"));
    }

    #[test]
    fn rejects_dangling_axis_references_without_returning_partial_xml() {
        let source = advanced_combo_chart();
        let error = apply_chart_edit(
            &source,
            &json!({"axes":[{"id":20,"crossAxisId":999}],"title":"must not escape"}),
        )
        .unwrap_err();
        assert!(error.contains("missing cross axis 999"));
        assert!(!source.contains("must not escape"));
    }

    #[test]
    fn rejects_structurally_unsafe_conversion() {
        let error = apply_chart_edit(CHART, &json!({"chartType":"scatter"})).unwrap_err();
        assert!(error.contains("unsafe chart type conversion"));
    }

    #[test]
    fn invalid_xml_returns_empty_model_and_edit_error() {
        assert_eq!(parse_chart_model("<broken")["chartType"], "unknown");
        assert!(apply_chart_edit("<broken", &json!({"title":"x"})).is_err());
    }
}
