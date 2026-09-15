//! Excel page-layout interpretation used by the browser-native print renderer.
//!
//! This module deliberately has no dependency on `AppState`: it turns the lossless
//! page-review JSON into a compact, testable print contract.  The HTTP renderer can
//! therefore use the exact workbook settings without duplicating OOXML rules.

use serde_json::Value;

const XLSX_MAX_ROWS: i32 = 1_048_576;
const XLSX_MAX_COLUMNS: i32 = 16_384;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CellRange {
    pub r0: i32,
    pub c0: i32,
    pub r1: i32,
    pub c1: i32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RepeatTitles {
    pub rows: Option<(i32, i32)>,
    pub columns: Option<(i32, i32)>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PageOrder {
    /// Excel's default: finish the vertical strip before moving to the right.
    #[default]
    DownThenOver,
    /// Finish a horizontal strip before moving down.
    OverThenDown,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PrintedErrors {
    #[default]
    Displayed,
    Blank,
    Dash,
    NotAvailable,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PrintedComments {
    #[default]
    None,
    AsDisplayed,
    AtEnd,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PrintNote {
    pub reference: String,
    pub row: i32,
    pub column: i32,
    pub author: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PaperSpec {
    pub label: String,
    pub width_mm: f64,
    pub height_mm: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ExcelPrintSettings {
    pub paper: String,
    pub paper_code: u16,
    pub paper_width_mm: Option<f64>,
    pub paper_height_mm: Option<f64>,
    pub orientation: String,
    pub scale_percent: Option<f64>,
    pub fit_to_width: Option<u32>,
    pub fit_to_height: Option<u32>,
    pub margin_left_px: f64,
    pub margin_right_px: f64,
    pub margin_top_px: f64,
    pub margin_bottom_px: f64,
    pub header_px: f64,
    pub footer_px: f64,
    pub horizontal_centered: bool,
    pub vertical_centered: bool,
    pub print_grid_lines: bool,
    pub print_headings: bool,
    pub print_areas: Vec<CellRange>,
    pub repeat_titles: RepeatTitles,
    pub row_breaks: Vec<i32>,
    pub column_breaks: Vec<i32>,
    pub odd_header: String,
    pub odd_footer: String,
    pub even_header: String,
    pub even_footer: String,
    pub first_header: String,
    pub first_footer: String,
    pub different_first: bool,
    pub different_odd_even: bool,
    pub scale_header_footer: bool,
    pub align_header_footer_with_margins: bool,
    pub page_order: PageOrder,
    pub first_page_number: u32,
    pub use_first_page_number: bool,
    pub black_and_white: bool,
    pub draft: bool,
    pub printed_errors: PrintedErrors,
    pub printed_comments: PrintedComments,
    pub horizontal_dpi: Option<u32>,
    pub vertical_dpi: Option<u32>,
    pub copies: u32,
    pub notes: Vec<PrintNote>,
}

impl Default for ExcelPrintSettings {
    fn default() -> Self {
        // Excel's default margins are 0.7in left/right, 0.75in top/bottom and
        // 0.3in header/footer.  CSS print geometry is expressed at 96px/in.
        Self {
            paper: "A4".to_string(),
            paper_code: 9,
            paper_width_mm: Some(210.0),
            paper_height_mm: Some(297.0),
            orientation: "portrait".to_string(),
            scale_percent: None,
            fit_to_width: None,
            fit_to_height: None,
            margin_left_px: 0.7 * 96.0,
            margin_right_px: 0.7 * 96.0,
            margin_top_px: 0.75 * 96.0,
            margin_bottom_px: 0.75 * 96.0,
            header_px: 0.3 * 96.0,
            footer_px: 0.3 * 96.0,
            horizontal_centered: false,
            vertical_centered: false,
            print_grid_lines: false,
            print_headings: false,
            print_areas: Vec::new(),
            repeat_titles: RepeatTitles::default(),
            row_breaks: Vec::new(),
            column_breaks: Vec::new(),
            odd_header: String::new(),
            odd_footer: String::new(),
            even_header: String::new(),
            even_footer: String::new(),
            first_header: String::new(),
            first_footer: String::new(),
            different_first: false,
            different_odd_even: false,
            scale_header_footer: true,
            align_header_footer_with_margins: true,
            page_order: PageOrder::DownThenOver,
            first_page_number: 1,
            use_first_page_number: false,
            black_and_white: false,
            draft: false,
            printed_errors: PrintedErrors::Displayed,
            printed_comments: PrintedComments::None,
            horizontal_dpi: None,
            vertical_dpi: None,
            copies: 1,
            notes: Vec::new(),
        }
    }
}

fn attr<'a>(value: &'a Value, name: &str) -> Option<&'a str> {
    value.get(name).and_then(Value::as_str).or_else(|| {
        value
            .get("attributes")
            .and_then(|attrs| attrs.get(name))
            .and_then(Value::as_str)
    })
}

fn raw_attr<'a>(value: &'a Value, name: &str) -> Option<&'a Value> {
    value
        .get(name)
        .or_else(|| value.get("attributes").and_then(|attrs| attrs.get(name)))
}

fn bool_attr_opt(value: &Value, name: &str) -> Option<bool> {
    raw_attr(value, name).and_then(|raw| {
        raw.as_bool()
            .or_else(|| raw.as_i64().map(|number| number != 0))
            .or_else(|| {
                raw.as_str()
                    .and_then(|text| match text.to_ascii_lowercase().as_str() {
                        "1" | "true" => Some(true),
                        "0" | "false" => Some(false),
                        _ => None,
                    })
            })
    })
}

fn bool_attr(value: &Value, name: &str) -> bool {
    bool_attr_opt(value, name).unwrap_or(false)
}

fn number_attr(value: &Value, name: &str) -> Option<f64> {
    raw_attr(value, name).and_then(|raw| {
        raw.as_f64()
            .or_else(|| raw.as_str().and_then(|text| text.parse::<f64>().ok()))
    })
}

fn paper_from_code(code: u16) -> Option<PaperSpec> {
    // These are every paper type exposed by the page-layout editor.  Dimensions
    // come from the Windows/OOXML DMPAPER contract and are kept in portrait order;
    // `orientation` is applied later by the HTML renderer.
    let (label, width_mm, height_mm) = match code {
        1 => ("Letter", 215.9, 279.4),
        3 => ("Tabloid", 279.4, 431.8),
        4 => ("Ledger", 279.4, 431.8),
        5 => ("Legal", 215.9, 355.6),
        8 => ("A3", 297.0, 420.0),
        9 => ("A4", 210.0, 297.0),
        11 => ("A5", 148.0, 210.0),
        12 => ("B4 JIS", 257.0, 364.0),
        13 => ("B5 JIS", 182.0, 257.0),
        14 => ("Folio", 215.9, 330.2),
        _ => return None,
    };
    Some(PaperSpec {
        label: label.to_string(),
        width_mm,
        height_mm,
    })
}

fn paper_code(value: &str) -> Option<u16> {
    value
        .parse()
        .ok()
        .or_else(|| match value.trim().to_ascii_lowercase().as_str() {
            "letter" | "us-letter" => Some(1),
            "tabloid" => Some(3),
            "ledger" => Some(4),
            "legal" | "us-legal" => Some(5),
            "a3" => Some(8),
            "a4" => Some(9),
            "a5" => Some(11),
            "b4" | "b4-jis" => Some(12),
            "b5" | "b5-jis" => Some(13),
            "folio" => Some(14),
            _ => None,
        })
}

fn measurement_mm(value: &str) -> Option<f64> {
    let value = value.trim().to_ascii_lowercase();
    let split = value.find(|ch: char| !(ch.is_ascii_digit() || matches!(ch, '.' | '+' | '-')))?;
    let amount: f64 = value[..split].parse().ok()?;
    if !amount.is_finite() || amount <= 0.0 {
        return None;
    }
    let multiplier = match value[split..].trim() {
        "mm" => 1.0,
        "cm" => 10.0,
        "in" => 25.4,
        "pt" => 25.4 / 72.0,
        "pc" => 25.4 / 6.0,
        "px" => 25.4 / 96.0,
        _ => return None,
    };
    Some(amount * multiplier)
}

pub(crate) fn paper_spec_by_name(value: &str) -> Option<PaperSpec> {
    paper_code(value).and_then(paper_from_code)
}

impl ExcelPrintSettings {
    pub(crate) fn paper_spec(&self) -> PaperSpec {
        match (self.paper_width_mm, self.paper_height_mm) {
            (Some(width_mm), Some(height_mm)) if width_mm > 0.0 && height_mm > 0.0 => PaperSpec {
                label: self.paper.clone(),
                width_mm,
                height_mm,
            },
            _ => paper_from_code(self.paper_code).unwrap_or(PaperSpec {
                // Unknown printer-specific codes cannot be represented by CSS.  A4
                // is the safe physical fallback, while the original code remains in
                // the OOXML and the diagnostic label remains visible in the DOM.
                label: self.paper.clone(),
                width_mm: 210.0,
                height_mm: 297.0,
            }),
        }
    }

    pub(crate) fn displayed_page_number(&self, zero_based_page: usize) -> usize {
        if self.use_first_page_number {
            self.first_page_number.max(1) as usize + zero_based_page
        } else {
            zero_based_page + 1
        }
    }
}

pub(crate) fn ordered_page_coordinates(
    row_pages: usize,
    column_pages: usize,
    order: PageOrder,
) -> Vec<(usize, usize)> {
    let mut pages = Vec::with_capacity(row_pages.saturating_mul(column_pages));
    match order {
        PageOrder::DownThenOver => {
            for column in 0..column_pages {
                for row in 0..row_pages {
                    pages.push((row, column));
                }
            }
        }
        PageOrder::OverThenDown => {
            for row in 0..row_pages {
                for column in 0..column_pages {
                    pages.push((row, column));
                }
            }
        }
    }
    pages
}

/// Greedily paginate variable-height items without ever dropping an over-sized
/// item.  The returned half-open ranges are deterministic and cover every item.
pub(crate) fn paginate_item_costs(costs: &[usize], page_capacity: usize) -> Vec<(usize, usize)> {
    if costs.is_empty() {
        return Vec::new();
    }
    let capacity = page_capacity.max(1);
    let mut pages = Vec::new();
    let mut start = 0usize;
    let mut used = 0usize;
    for (index, cost) in costs.iter().copied().enumerate() {
        let cost = cost.max(1);
        if index > start && used.saturating_add(cost) > capacity {
            pages.push((start, index));
            start = index;
            used = 0;
        }
        used = used.saturating_add(cost);
    }
    pages.push((start, costs.len()));
    pages
}

fn column_number(text: &str) -> Option<i32> {
    let mut value = 0i32;
    let mut any = false;
    for ch in text.chars() {
        if !ch.is_ascii_alphabetic() {
            return None;
        }
        any = true;
        value = value.checked_mul(26)? + (ch.to_ascii_uppercase() as i32 - 'A' as i32 + 1);
    }
    any.then_some(value)
}

fn strip_sheet(reference: &str) -> &str {
    // A sheet name may contain escaped apostrophes.  The last `!` is the range delimiter.
    reference
        .rsplit_once('!')
        .map(|(_, tail)| tail)
        .unwrap_or(reference)
}

fn parse_cell(reference: &str) -> Option<(i32, i32)> {
    let cleaned = strip_sheet(reference).replace('$', "");
    let split = cleaned.find(|ch: char| ch.is_ascii_digit())?;
    let (column, row) = cleaned.split_at(split);
    Some((row.parse().ok()?, column_number(column)?))
}

fn formula_union(formula: &str) -> Vec<&str> {
    // Commas inside quoted sheet names are not union separators.  Apostrophes inside
    // a sheet name are escaped as two apostrophes, so keep both inside the quote.
    let mut result = Vec::new();
    let mut start = 0usize;
    let mut quoted = false;
    let bytes = formula.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' if quoted && bytes.get(i + 1) == Some(&b'\'') => i += 2,
            b'\'' => {
                quoted = !quoted;
                i += 1;
            }
            b',' if !quoted => {
                result.push(formula[start..i].trim());
                start = i + 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    result.push(formula[start..].trim());
    result.into_iter().filter(|item| !item.is_empty()).collect()
}

pub(crate) fn parse_print_areas(formula: &str) -> Vec<CellRange> {
    let formula = formula.trim().strip_prefix('=').unwrap_or(formula.trim());
    let mut result = Vec::new();
    for area in formula_union(formula) {
        let range = strip_sheet(area.trim());
        let (start, end) = range.split_once(':').unwrap_or((range, range));
        if let (Some((ar, ac)), Some((br, bc))) = (parse_cell(start), parse_cell(end)) {
            result.push(CellRange {
                r0: ar.min(br),
                c0: ac.min(bc),
                r1: ar.max(br),
                c1: ac.max(bc),
            });
            continue;
        }
        let start = start.replace('$', "");
        let end = end.replace('$', "");
        if let (Ok(ar), Ok(br)) = (start.parse::<i32>(), end.parse::<i32>()) {
            result.push(CellRange {
                r0: ar.min(br),
                c0: 1,
                r1: ar.max(br),
                c1: XLSX_MAX_COLUMNS,
            });
        } else if let (Some(ac), Some(bc)) = (column_number(&start), column_number(&end)) {
            result.push(CellRange {
                r0: 1,
                c0: ac.min(bc),
                r1: XLSX_MAX_ROWS,
                c1: ac.max(bc),
            });
        }
    }
    result
}

/// Parse a single Excel print area.  Callers that render a workbook must use
/// `parse_print_areas`; this compatibility helper intentionally never invents a
/// bounding rectangle for a disjoint union.
pub(crate) fn parse_print_area(formula: &str) -> Option<CellRange> {
    parse_print_areas(formula).into_iter().next()
}

pub(crate) fn parse_print_titles(formula: &str) -> RepeatTitles {
    let mut result = RepeatTitles::default();
    for area in formula_union(formula) {
        let range = strip_sheet(area.trim()).replace('$', "");
        let Some((start, end)) = range.split_once(':') else {
            continue;
        };
        if let (Ok(a), Ok(b)) = (start.parse::<i32>(), end.parse::<i32>()) {
            result.rows = Some((a.min(b), a.max(b)));
        } else if let (Some(a), Some(b)) = (column_number(start), column_number(end)) {
            result.columns = Some((a.min(b), a.max(b)));
        }
    }
    result
}

fn defined_formula<'a>(model: &'a Value, sheet: usize, kind: &str) -> Option<&'a str> {
    model
        .get("definedNames")?
        .as_array()?
        .iter()
        .find(|item| {
            item.get("kind").and_then(Value::as_str) == Some(kind)
                && item.get("localSheetId").and_then(Value::as_u64) == Some(sheet as u64)
        })?
        .get("formula")?
        .as_str()
}

fn manual_breaks(value: &Value) -> Vec<i32> {
    let mut breaks: Vec<i32> = value
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| !matches!(attr(item, "man"), Some("0" | "false")))
        .filter_map(|item| number_attr(item, "id").map(|id| id as i32 + 1))
        .collect();
    breaks.sort_unstable();
    breaks.dedup();
    breaks
}

pub(crate) fn settings_from_page_model(model: &Value, sheet_index: usize) -> ExcelPrintSettings {
    let mut out = ExcelPrintSettings::default();
    let Some(sheet) = model
        .get("worksheets")
        .and_then(Value::as_array)
        .and_then(|sheets| sheets.get(sheet_index))
    else {
        return out;
    };
    let setup = &sheet["pageSetup"];
    let requested_paper_code = attr(setup, "paperSize")
        .and_then(paper_code)
        .or_else(|| number_attr(setup, "paperSize").map(|value| value as u16));
    if let Some(code) = requested_paper_code {
        out.paper_code = code;
        if let Some(spec) = paper_from_code(code) {
            out.paper = spec.label;
            out.paper_width_mm = Some(spec.width_mm);
            out.paper_height_mm = Some(spec.height_mm);
        } else {
            out.paper = format!("Paper {code}");
            out.paper_width_mm = None;
            out.paper_height_mm = None;
        }
    }
    if let (Some(width), Some(height)) = (
        attr(setup, "paperWidth").and_then(measurement_mm),
        attr(setup, "paperHeight").and_then(measurement_mm),
    ) {
        out.paper = "Custom".to_string();
        out.paper_width_mm = Some(width);
        out.paper_height_mm = Some(height);
    }
    if matches!(attr(setup, "orientation"), Some("landscape")) {
        out.orientation = "landscape".to_string();
    }
    out.scale_percent = number_attr(setup, "scale").map(|v| v.clamp(10.0, 400.0));
    out.fit_to_width = number_attr(setup, "fitToWidth").map(|v| v.max(0.0) as u32);
    out.fit_to_height = number_attr(setup, "fitToHeight").map(|v| v.max(0.0) as u32);
    out.page_order = if matches!(attr(setup, "pageOrder"), Some("overThenDown")) {
        PageOrder::OverThenDown
    } else {
        PageOrder::DownThenOver
    };
    out.first_page_number = number_attr(setup, "firstPageNumber")
        .map(|value| value.max(1.0) as u32)
        .unwrap_or(1);
    out.use_first_page_number = bool_attr(setup, "useFirstPageNumber");
    out.black_and_white = bool_attr(setup, "blackAndWhite");
    out.draft = bool_attr(setup, "draft");
    out.horizontal_dpi = number_attr(setup, "horizontalDpi").map(|value| value.max(1.0) as u32);
    out.vertical_dpi = number_attr(setup, "verticalDpi").map(|value| value.max(1.0) as u32);
    out.copies = number_attr(setup, "copies")
        .map(|value| value.clamp(1.0, 32767.0) as u32)
        .unwrap_or(1);
    out.printed_errors = match attr(setup, "errors") {
        Some("blank") => PrintedErrors::Blank,
        Some("dash") => PrintedErrors::Dash,
        Some("NA") | Some("na") => PrintedErrors::NotAvailable,
        _ => PrintedErrors::Displayed,
    };
    out.printed_comments = match attr(setup, "cellComments") {
        Some("asDisplayed") => PrintedComments::AsDisplayed,
        Some("atEnd") => PrintedComments::AtEnd,
        _ => PrintedComments::None,
    };

    let margins = &sheet["pageMargins"];
    for (name, target) in [
        ("left", &mut out.margin_left_px),
        ("right", &mut out.margin_right_px),
        ("top", &mut out.margin_top_px),
        ("bottom", &mut out.margin_bottom_px),
        ("header", &mut out.header_px),
        ("footer", &mut out.footer_px),
    ] {
        if let Some(inches) = number_attr(margins, name) {
            *target = inches.max(0.0) * 96.0;
        }
    }
    let options = &sheet["printOptions"];
    out.horizontal_centered = bool_attr(options, "horizontalCentered");
    out.vertical_centered = bool_attr(options, "verticalCentered");
    out.print_grid_lines = bool_attr(options, "gridLines");
    out.print_headings = bool_attr(options, "headings");

    let header = &sheet["headerFooter"];
    out.odd_header = header
        .get("oddHeader")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    out.odd_footer = header
        .get("oddFooter")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    out.even_header = header
        .get("evenHeader")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    out.even_footer = header
        .get("evenFooter")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    out.first_header = header
        .get("firstHeader")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    out.first_footer = header
        .get("firstFooter")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    out.different_first = bool_attr(header, "differentFirst");
    out.different_odd_even = bool_attr(header, "differentOddEven");
    out.scale_header_footer = bool_attr_opt(header, "scaleWithDoc").unwrap_or(true);
    out.align_header_footer_with_margins =
        bool_attr_opt(header, "alignWithMargins").unwrap_or(true);
    out.row_breaks = manual_breaks(&sheet["rowBreaks"]);
    out.column_breaks = manual_breaks(&sheet["colBreaks"]);
    out.print_areas = defined_formula(model, sheet_index, "printArea")
        .map(parse_print_areas)
        .unwrap_or_default();
    out.repeat_titles = defined_formula(model, sheet_index, "printTitles")
        .map(parse_print_titles)
        .unwrap_or_default();
    out.notes = sheet
        .get("notes")
        .and_then(|notes| notes.get("items"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|note| {
            let reference = note.get("ref")?.as_str()?.to_string();
            let (row, column) = parse_cell(&reference)?;
            Some(PrintNote {
                reference,
                row,
                column,
                author: note
                    .get("author")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                text: note
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            })
        })
        .collect();
    out
}

fn is_excel_error(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_uppercase().as_str(),
        "#NULL!"
            | "#DIV/0!"
            | "#VALUE!"
            | "#REF!"
            | "#NAME?"
            | "#NUM!"
            | "#N/A"
            | "#GETTING_DATA"
            | "#SPILL!"
            | "#CALC!"
            | "#FIELD!"
            | "#BLOCKED!"
            | "#CONNECT!"
            | "#UNKNOWN!"
            | "#BUSY!"
            | "#PYTHON!"
            | "#CIRC!"
    )
}

pub(crate) fn printed_cell_value(value: &str, mode: PrintedErrors) -> String {
    if !is_excel_error(value) || mode == PrintedErrors::Displayed {
        return value.to_string();
    }
    match mode {
        PrintedErrors::Blank => String::new(),
        PrintedErrors::Dash => "--".to_string(),
        PrintedErrors::NotAvailable => "#N/A".to_string(),
        PrintedErrors::Displayed => value.to_string(),
    }
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
        .replace('\n', "<br>")
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct HeaderStyle {
    font: String,
    size: Option<u16>,
    bold: bool,
    italic: bool,
    underline: u8,
    strike: bool,
    superscript: bool,
    subscript: bool,
    outline: bool,
    shadow: bool,
    color: Option<String>,
}

impl HeaderStyle {
    fn css(&self) -> String {
        let mut css = String::new();
        if !self.font.is_empty() {
            let font = self.font.replace(['"', '\'', ';', '<', '>'], "");
            if !font.trim().is_empty() {
                css.push_str("font-family:'");
                css.push_str(font.trim());
                css.push_str("';");
            }
        }
        if let Some(size) = self.size {
            css.push_str(&format!("font-size:{size}pt;"));
        }
        if self.bold {
            css.push_str("font-weight:bold;");
        }
        if self.italic {
            css.push_str("font-style:italic;");
        }
        let mut decorations = Vec::new();
        if self.underline != 0 {
            decorations.push("underline");
        }
        if self.strike {
            decorations.push("line-through");
        }
        if !decorations.is_empty() {
            css.push_str("text-decoration:");
            css.push_str(&decorations.join(" "));
            css.push(';');
        }
        if self.underline == 2 {
            css.push_str("text-decoration-style:double;");
        }
        if self.superscript {
            css.push_str("vertical-align:super;font-size:smaller;");
        } else if self.subscript {
            css.push_str("vertical-align:sub;font-size:smaller;");
        }
        if self.outline {
            css.push_str("-webkit-text-stroke:.35px currentColor;");
        }
        if self.shadow {
            css.push_str("text-shadow:1px 1px 0 rgba(0,0,0,.35);");
        }
        if let Some(color) = &self.color {
            css.push_str("color:#");
            css.push_str(color);
            css.push(';');
        }
        css
    }
}

fn styled_html(style: &HeaderStyle, html: &str) -> String {
    let css = style.css();
    if css.is_empty() {
        html.to_string()
    } else {
        format!("<span style=\"{css}\">{html}</span>")
    }
}

fn page_with_offset(page: usize, chars: &[char], cursor: &mut usize) -> usize {
    if *cursor >= chars.len() || !matches!(chars[*cursor], '+' | '-') {
        return page;
    }
    let sign = if chars[*cursor] == '-' {
        -1isize
    } else {
        1isize
    };
    *cursor += 1;
    let start = *cursor;
    while *cursor < chars.len() && chars[*cursor].is_ascii_digit() {
        *cursor += 1;
    }
    let offset = chars[start..*cursor]
        .iter()
        .collect::<String>()
        .parse::<isize>()
        .unwrap_or(0);
    (page as isize + sign * offset).max(1) as usize
}

fn parse_header_footer(
    source: &str,
    sheet: &str,
    file: &str,
    page: usize,
    pages: usize,
    html: bool,
) -> [String; 3] {
    let mut sections = [String::new(), String::new(), String::new()];
    let mut section = 1usize;
    let mut style = HeaderStyle::default();
    let mut literal = String::new();
    let chars: Vec<char> = source.chars().collect();
    let mut i = 0usize;
    let flush =
        |sections: &mut [String; 3], section: usize, style: &HeaderStyle, text: &mut String| {
            if text.is_empty() {
                return;
            }
            if html {
                sections[section].push_str(&styled_html(style, &escape_html(text)));
            } else {
                sections[section].push_str(text);
            }
            text.clear();
        };
    let field = |sections: &mut [String; 3], section: usize, style: &HeaderStyle, value: String| {
        if html {
            sections[section].push_str(&styled_html(style, &value));
        } else {
            sections[section].push_str(&value);
        }
    };
    while i < chars.len() {
        if chars[i] != '&' || i + 1 >= chars.len() {
            literal.push(chars[i]);
            i += 1;
            continue;
        }
        flush(&mut sections, section, &style, &mut literal);
        let code = chars[i + 1];
        i += 2;
        if code == '[' {
            let start = i;
            while i < chars.len() && chars[i] != ']' {
                i += 1;
            }
            if i < chars.len() {
                let raw_token = chars[start..i].iter().collect::<String>();
                let token = raw_token.to_ascii_lowercase();
                i += 1;
                let value = match token.as_str() {
                    "page" => Some(page.to_string()),
                    "pages" => Some(pages.to_string()),
                    "tab" => Some(if html {
                        escape_html(sheet)
                    } else {
                        sheet.to_string()
                    }),
                    "file" | "path" => Some(if html {
                        escape_html(file)
                    } else {
                        file.to_string()
                    }),
                    "date" if html => {
                        Some("<time data-excel-field=\"date\"></time>".to_string())
                    }
                    "time" if html => {
                        Some("<time data-excel-field=\"time\"></time>".to_string())
                    }
                    "date" => Some("Date".to_string()),
                    "time" => Some("Time".to_string()),
                    "picture" if html => Some("<span class=\"hf-picture-missing\" data-excel-header-picture=\"preserved\" aria-hidden=\"true\"></span>".to_string()),
                    "picture" => Some(String::new()),
                    _ => None,
                };
                if let Some(value) = value {
                    field(&mut sections, section, &style, value);
                    continue;
                }
                literal.push_str("&[");
                literal.push_str(&raw_token);
                literal.push(']');
                continue;
            }
            literal.push_str("&[");
            literal.extend(chars[start..].iter());
            break;
        }
        match code.to_ascii_uppercase() {
            'L' => section = 0,
            'C' => section = 1,
            'R' => section = 2,
            'P' => {
                let number = page_with_offset(page, &chars, &mut i);
                field(&mut sections, section, &style, number.to_string());
            }
            'N' => field(&mut sections, section, &style, pages.to_string()),
            'A' => field(
                &mut sections,
                section,
                &style,
                if html {
                    escape_html(sheet)
                } else {
                    sheet.to_string()
                },
            ),
            'F' | 'Z' => field(
                &mut sections,
                section,
                &style,
                if html {
                    escape_html(file)
                } else {
                    file.to_string()
                },
            ),
            'D' => field(
                &mut sections,
                section,
                &style,
                if html {
                    "<time data-excel-field=\"date\"></time>".to_string()
                } else {
                    "Date".to_string()
                },
            ),
            'T' => field(
                &mut sections,
                section,
                &style,
                if html {
                    "<time data-excel-field=\"time\"></time>".to_string()
                } else {
                    "Time".to_string()
                },
            ),
            'G' => {
                if html {
                    // A missing VML relationship must never become a network request or
                    // broken-image glyph.  The marker is hidden, while OOXML keeps &G.
                    sections[section].push_str("<span class=\"hf-picture-missing\" data-excel-header-picture=\"preserved\" aria-hidden=\"true\"></span>");
                }
            }
            '&' => literal.push('&'),
            'B' => style.bold = !style.bold,
            'I' => style.italic = !style.italic,
            'U' => style.underline = if style.underline == 1 { 0 } else { 1 },
            'E' => style.underline = if style.underline == 2 { 0 } else { 2 },
            'S' => style.strike = !style.strike,
            'O' => style.outline = !style.outline,
            'H' => style.shadow = !style.shadow,
            'X' => {
                style.superscript = !style.superscript;
                if style.superscript {
                    style.subscript = false;
                }
            }
            'Y' => {
                style.subscript = !style.subscript;
                if style.subscript {
                    style.superscript = false;
                }
            }
            'K' => {
                let end = (i + 6).min(chars.len());
                let directive = &chars[i..end];
                let color = directive.iter().collect::<String>();
                if directive.len() == 6 && directive.iter().all(|ch| ch.is_ascii_hexdigit()) {
                    style.color = Some(color.to_ascii_uppercase());
                    i = end;
                } else if directive.len() == 6
                    && directive[..2].iter().all(|ch| ch.is_ascii_digit())
                    && matches!(directive[2], '+' | '-')
                    && directive[3..].iter().all(|ch| ch.is_ascii_digit())
                {
                    // Theme+tint colors need workbook theme resolution. Consume the
                    // complete directive without leaking it into printed text; the
                    // source code remains byte-for-byte preserved in OOXML.
                    i = end;
                }
            }
            '"' => {
                let start = i;
                while i < chars.len() && chars[i] != '"' {
                    i += 1;
                }
                let descriptor = chars[start..i].iter().collect::<String>();
                i = (i + 1).min(chars.len());
                let mut fields = descriptor.splitn(2, ',');
                style.font = fields.next().unwrap_or_default().to_string();
                let face = fields.next().unwrap_or_default().to_ascii_lowercase();
                if !face.is_empty() {
                    style.bold = face.contains("bold");
                    style.italic = face.contains("italic");
                }
            }
            digit if digit.is_ascii_digit() => {
                let mut number = String::from(digit);
                while i < chars.len() && chars[i].is_ascii_digit() {
                    number.push(chars[i]);
                    i += 1;
                }
                style.size = number.parse::<u16>().ok().filter(|value| *value > 0);
            }
            other => literal.push(other),
        }
    }
    flush(&mut sections, section, &style, &mut literal);
    sections
}

/// Render escaped inline HTML for the complete Excel header/footer mini-language.
pub(crate) fn render_header_footer_html(
    source: &str,
    sheet: &str,
    file: &str,
    page: usize,
    pages: usize,
) -> [String; 3] {
    parse_header_footer(source, sheet, file, page, pages, true)
}

pub(crate) fn expand_header_footer(
    source: &str,
    sheet: &str,
    file: &str,
    page: usize,
    pages: usize,
) -> [String; 3] {
    parse_header_footer(source, sheet, file, page, pages, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_excel_page_contract() {
        let model = json!({
            "definedNames": [
                {"kind":"printArea","localSheetId":0,"formula":"'Data Set'!$B$3:$F$40,'Data Set'!$H$2:$J$5"},
                {"kind":"printTitles","localSheetId":0,"formula":"'Data Set'!$1:$2,'Data Set'!$A:$B"}
            ],
            "worksheets": [{
                "pageMargins":{"left":"0.25","right":"0.5","top":"0.75","bottom":"1","header":"0.2","footer":"0.3"},
                "pageSetup":{"paperSize":"11","orientation":"landscape","fitToWidth":"1","fitToHeight":"0",
                    "pageOrder":"overThenDown","firstPageNumber":"7","useFirstPageNumber":"1",
                    "blackAndWhite":"1","draft":"1","errors":"dash","cellComments":"atEnd","copies":"3"},
                "printOptions":{"gridLines":"1","headings":"1","horizontalCentered":"1"},
                "rowBreaks":{"items":[{"id":"9","man":"1"}]},
                "colBreaks":{"items":[{"id":"2","man":"1"}]},
                "headerFooter":{"attributes":{"differentFirst":"1","scaleWithDoc":"0","alignWithMargins":"0"},"oddHeader":"&L&F&C&A&R&P/&N","firstFooter":"first"},
                "notes":{"items":[{"ref":"C4","author":"Alice","text":"Review this"}]}
            }]
        });
        let settings = settings_from_page_model(&model, 0);
        assert_eq!(settings.paper, "A5");
        assert_eq!(settings.orientation, "landscape");
        assert_eq!(settings.fit_to_width, Some(1));
        assert_eq!(
            settings.print_areas[0],
            CellRange {
                r0: 3,
                c0: 2,
                r1: 40,
                c1: 6
            }
        );
        assert_eq!(settings.print_areas.len(), 2);
        assert_eq!(settings.repeat_titles.rows, Some((1, 2)));
        assert_eq!(settings.repeat_titles.columns, Some((1, 2)));
        assert_eq!(settings.row_breaks, vec![10]);
        assert_eq!(settings.column_breaks, vec![3]);
        assert_eq!(settings.margin_left_px, 24.0);
        assert!(settings.print_grid_lines && settings.horizontal_centered);
        assert!(settings.print_headings && settings.black_and_white && settings.draft);
        assert!(settings.different_first);
        assert_eq!(settings.page_order, PageOrder::OverThenDown);
        assert_eq!(settings.displayed_page_number(0), 7);
        assert_eq!(settings.printed_errors, PrintedErrors::Dash);
        assert_eq!(settings.printed_comments, PrintedComments::AtEnd);
        assert_eq!(settings.copies, 3);
        assert_eq!(settings.notes[0].reference, "C4");
        assert!(!settings.scale_header_footer && !settings.align_header_footer_with_margins);
    }

    #[test]
    fn expands_sections_and_page_fields() {
        assert_eq!(
            expand_header_footer("&L&F&C&A&RPage &P of &N", "Data", "book.xlsx", 2, 7),
            ["book.xlsx", "Data", "Page 2 of 7"]
        );
    }

    #[test]
    fn disjoint_areas_do_not_create_a_bounding_rectangle() {
        assert_eq!(
            parse_print_areas("='A, B'!$A$1:$B$2,'A, B'!$XFD$1048576"),
            vec![
                CellRange {
                    r0: 1,
                    c0: 1,
                    r1: 2,
                    c1: 2
                },
                CellRange {
                    r0: 1_048_576,
                    c0: 16_384,
                    r1: 1_048_576,
                    c1: 16_384
                },
            ]
        );
        assert_eq!(
            parse_print_area("Sheet1!A1:B2,Sheet1!Z99:Z100"),
            Some(CellRange {
                r0: 1,
                c0: 1,
                r1: 2,
                c1: 2
            })
        );
        assert_eq!(
            parse_print_areas("Sheet1!$3:$4,Sheet1!$B:$C"),
            vec![
                CellRange {
                    r0: 3,
                    c0: 1,
                    r1: 4,
                    c1: XLSX_MAX_COLUMNS
                },
                CellRange {
                    r0: 1,
                    c0: 2,
                    r1: XLSX_MAX_ROWS,
                    c1: 3
                },
            ]
        );
    }

    #[test]
    fn maps_every_editor_paper_and_custom_measurements() {
        for (name, width, height) in [
            ("letter", 215.9, 279.4),
            ("legal", 215.9, 355.6),
            ("tabloid", 279.4, 431.8),
            ("ledger", 279.4, 431.8),
            ("a3", 297.0, 420.0),
            ("a4", 210.0, 297.0),
            ("a5", 148.0, 210.0),
            ("b4-jis", 257.0, 364.0),
            ("b5-jis", 182.0, 257.0),
            ("folio", 215.9, 330.2),
        ] {
            let paper = paper_spec_by_name(name).unwrap();
            assert!((paper.width_mm - width).abs() < 0.001, "{name}");
            assert!((paper.height_mm - height).abs() < 0.001, "{name}");
        }
        let settings = settings_from_page_model(
            &json!({"worksheets":[{
                "pageSetup":{"paperSize":"255","paperWidth":"8.25in","paperHeight":"300mm"}
            }]}),
            0,
        );
        let custom = settings.paper_spec();
        assert_eq!(custom.label, "Custom");
        assert!((custom.width_mm - 209.55).abs() < 0.001);
        assert_eq!(custom.height_mm, 300.0);
    }

    #[test]
    fn header_footer_renders_formatting_fields_and_safe_picture_marker() {
        let html = render_header_footer_html(
            "&L&\"Aptos,Bold Italic\"&14&KFF0000&O&HHi &P+2&C&& &D&R&G",
            "<Data>",
            "book&1.xlsx",
            5,
            9,
        );
        assert!(html[0].contains("font-family:'Aptos'"));
        assert!(html[0].contains("font-weight:bold"));
        assert!(html[0].contains("font-style:italic"));
        assert!(html[0].contains("font-size:14pt"));
        assert!(html[0].contains("color:#FF0000"));
        assert!(html[0].contains("-webkit-text-stroke"));
        assert!(html[0].contains("text-shadow"));
        assert!(html[0].contains("Hi "));
        assert!(html[0].contains(">7</span>"));
        assert!(html[1].contains("&amp;"));
        assert!(html[1].contains("data-excel-field=\"date\""));
        assert!(html[2].contains("data-excel-header-picture=\"preserved\""));
    }

    #[test]
    fn applies_excel_error_print_modes_only_to_errors() {
        assert_eq!(printed_cell_value("#DIV/0!", PrintedErrors::Blank), "");
        assert_eq!(printed_cell_value("#REF!", PrintedErrors::Dash), "--");
        assert_eq!(
            printed_cell_value("#VALUE!", PrintedErrors::NotAvailable),
            "#N/A"
        );
        assert_eq!(
            printed_cell_value("ordinary", PrintedErrors::Blank),
            "ordinary"
        );
    }

    #[test]
    fn page_order_matches_excel_down_and_over_semantics() {
        assert_eq!(
            ordered_page_coordinates(2, 3, PageOrder::DownThenOver),
            vec![(0, 0), (1, 0), (0, 1), (1, 1), (0, 2), (1, 2)]
        );
        assert_eq!(
            ordered_page_coordinates(2, 3, PageOrder::OverThenDown),
            vec![(0, 0), (0, 1), (0, 2), (1, 0), (1, 1), (1, 2)]
        );
    }

    #[test]
    fn comment_pagination_keeps_long_items_and_all_boundaries() {
        assert_eq!(paginate_item_costs(&[], 10), vec![]);
        assert_eq!(
            paginate_item_costs(&[2, 3, 7, 1], 6),
            vec![(0, 2), (2, 3), (3, 4)]
        );
        assert_eq!(paginate_item_costs(&[99], 6), vec![(0, 1)]);
    }
}
