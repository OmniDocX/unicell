//! AI 助手上下文层。
//!
//! 目标是让模型用最少的 token 看懂一张表，并且只能通过受控的操作集去改它。
//!
//! 分三级读取，模型自己按需下钻，而不是一次性把几万格灌进上下文：
//!   L0 `digest`  —— 工作簿/工作表概览：已用区域、表头、每列类型与统计。几百 token。
//!   L1 `slice`   —— 指定 A1 区域的行主序数据，公式单独列出。
//!   L2 `detail`  —— 单个单元格的公式源码、数字格式、样式与依赖。
//!
//! 地址一律用 A1（`Sheet1!B2:D10`）。模型写区域字符串的正确率远高于四个整数，
//! 而且同一个字符串可以直接嵌进它生成的公式里。
//!
//! 写入只经过 `CellOp` 这几种操作。不暴露任何直接改 OOXML 的口子——
//! 未识别的 x14 扩展与 DrawingML 的无损保留是这个项目最贵的资产，
//! 让模型去拼 XML 只会把它毁掉。

use ironcalc::base::UserModel;
use serde_json::{Value, json};

/// 单次切片返回的单元格上限，避免模型一句话把整张表拉进上下文。
pub const MAX_SLICE_CELLS: i64 = 20_000;
/// 统计每列时最多实际读取的行数；超过就转为抽样并在结果里标注。
const MAX_PROFILE_ROWS: i32 = 2_000;
/// 摘要里每列给出的样例值个数。
const SAMPLE_VALUES: usize = 3;
/// distinct 计数的上限，超过就只报“>N”，避免为大表建巨大的集合。
const MAX_DISTINCT: usize = 1_000;
/// 计算下游重算差异时允许快照的单元格数。
pub const MAX_DIFF_CELLS: i64 = 60_000;

// ============================== A1 地址 ==============================

/// 1 → A，27 → AA。
pub fn col_to_letters(mut n: i32) -> String {
    let mut out = String::new();
    while n > 0 {
        let rem = ((n - 1) % 26) as u8;
        out.insert(0, (b'A' + rem) as char);
        n = (n - 1) / 26;
    }
    out
}

/// A → 1，AA → 27。非纯字母或越界返回 None。
pub fn letters_to_col(text: &str) -> Option<i32> {
    if text.is_empty() || !text.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let mut col = 0i32;
    for byte in text.bytes() {
        col = col
            .checked_mul(26)?
            .checked_add((byte.to_ascii_uppercase() - b'A' + 1) as i32)?;
        if col > 16_384 {
            return None;
        }
    }
    Some(col)
}

/// `B2` / `$B$2` → (row, col)。
pub fn parse_cell(text: &str) -> Option<(i32, i32)> {
    let cleaned = text.trim().replace('$', "");
    let split = cleaned.find(|c: char| c.is_ascii_digit())?;
    let (letters, digits) = cleaned.split_at(split);
    let col = letters_to_col(letters)?;
    let row: i32 = digits.parse().ok()?;
    (1..=1_048_576).contains(&row).then_some((row, col))
}

/// 解析出的区域引用。`sheet_name` 为 None 表示"当前工作表"。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedRange {
    pub sheet_name: Option<String>,
    pub r0: i32,
    pub c0: i32,
    pub r1: i32,
    pub c1: i32,
}

/// 支持 `A1`、`A1:C9`、`Sheet1!A1:C9`、`'我的表'!A1`。
/// 起止点会被归一化，写反了（C9:A1）也能正确解析。
pub fn parse_range(text: &str) -> Option<ParsedRange> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (sheet_name, body) = match trimmed.rsplit_once('!') {
        Some((name, rest)) => {
            let name = name.trim().trim_matches('\'').to_string();
            if name.is_empty() {
                return None;
            }
            (Some(name), rest)
        }
        None => (None, trimmed),
    };
    let (start, end) = body.split_once(':').unwrap_or((body, body));
    let (r0, c0) = parse_cell(start)?;
    let (r1, c1) = parse_cell(end)?;
    Some(ParsedRange {
        sheet_name,
        r0: r0.min(r1),
        c0: c0.min(c1),
        r1: r0.max(r1),
        c1: c0.max(c1),
    })
}

pub fn format_cell(row: i32, col: i32) -> String {
    format!("{}{}", col_to_letters(col), row)
}

pub fn format_range(r0: i32, c0: i32, r1: i32, c1: i32) -> String {
    if r0 == r1 && c0 == c1 {
        format_cell(r0, c0)
    } else {
        format!("{}:{}", format_cell(r0, c0), format_cell(r1, c1))
    }
}

/// 带表名的完整引用，供返回给模型时使用（它下一轮可以原样传回来）。
pub fn qualify(sheet_name: &str, r0: i32, c0: i32, r1: i32, c1: i32) -> String {
    let quoted = if sheet_name.contains(|c: char| c.is_whitespace() || c == '\'') {
        format!("'{}'", sheet_name.replace('\'', "''"))
    } else {
        sheet_name.to_string()
    };
    format!("{quoted}!{}", format_range(r0, c0, r1, c1))
}

// ============================== 单元格分类 ==============================

/// 摘要里用的粗粒度类型。比 IronCalc 的内部类型少，够模型判断"这列能不能求和"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellKinds {
    pub numbers: u32,
    pub texts: u32,
    pub booleans: u32,
    pub errors: u32,
    pub formulas: u32,
}

impl CellKinds {
    fn total(&self) -> u32 {
        self.numbers + self.texts + self.booleans + self.errors
    }

    /// 占比超过八成才敢给这一列定性，否则算 mixed。
    fn dominant(&self) -> &'static str {
        let total = self.total();
        if total == 0 {
            return "empty";
        }
        let threshold = (total as f32 * 0.8).ceil() as u32;
        if self.numbers >= threshold {
            "number"
        } else if self.texts >= threshold {
            "text"
        } else if self.booleans >= threshold {
            "boolean"
        } else if self.errors >= threshold {
            "error"
        } else {
            "mixed"
        }
    }
}

/// 显示值以 `#` 开头且形如 `#NAME?` 才算错误值，避免把 `#1 号` 这种文本误判。
pub fn is_error_text(formatted: &str) -> bool {
    formatted.starts_with('#')
        && formatted.len() >= 4
        && formatted
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "#/!?_".contains(c))
}

fn classify(
    model: &UserModel,
    sheet: u32,
    row: i32,
    col: i32,
) -> Result<(&'static str, String, String), String> {
    let content = model.get_cell_content(sheet, row, col)?;
    let formatted = model.get_formatted_cell_value(sheet, row, col)?;
    if content.is_empty() && formatted.is_empty() {
        return Ok(("empty", content, formatted));
    }
    if is_error_text(&formatted) {
        return Ok(("error", content, formatted));
    }
    let debug = format!("{:?}", model.get_cell_type(sheet, row, col)?);
    let kind = if debug.contains("Number") {
        "number"
    } else if debug.contains("Bool") {
        "boolean"
    } else {
        "text"
    };
    Ok((kind, content, formatted))
}

// ============================== L0 摘要 ==============================

struct ColumnProfile {
    col: i32,
    header: Option<String>,
    kinds: CellKinds,
    distinct: std::collections::HashSet<String>,
    distinct_overflow: bool,
    min: Option<f64>,
    max: Option<f64>,
    samples: Vec<String>,
}

impl ColumnProfile {
    fn new(col: i32) -> Self {
        Self {
            col,
            header: None,
            kinds: CellKinds::default(),
            distinct: std::collections::HashSet::new(),
            distinct_overflow: false,
            min: None,
            max: None,
            samples: Vec::new(),
        }
    }

    fn observe(&mut self, kind: &str, content: &str, formatted: &str) {
        match kind {
            "number" => self.kinds.numbers += 1,
            "boolean" => self.kinds.booleans += 1,
            "error" => self.kinds.errors += 1,
            "empty" => return,
            _ => self.kinds.texts += 1,
        }
        if content.starts_with('=') {
            self.kinds.formulas += 1;
        }
        if self.distinct.len() < MAX_DISTINCT {
            self.distinct.insert(formatted.to_string());
        } else {
            self.distinct_overflow = true;
        }
        if kind == "number" {
            if let Some(number) = parse_number(content, formatted) {
                self.min = Some(self.min.map_or(number, |m: f64| m.min(number)));
                self.max = Some(self.max.map_or(number, |m: f64| m.max(number)));
            }
        }
        if self.samples.len() < SAMPLE_VALUES && !formatted.is_empty() {
            self.samples.push(truncate(formatted, 48));
        }
    }

    fn to_json(&self) -> Value {
        let distinct = if self.distinct_overflow {
            json!(format!(">{MAX_DISTINCT}"))
        } else {
            json!(self.distinct.len())
        };
        let mut out = json!({
            "col": col_to_letters(self.col),
            "type": self.kinds.dominant(),
            "nonEmpty": self.kinds.total(),
            "distinct": distinct,
            "samples": self.samples,
        });
        if let Some(header) = &self.header {
            out["header"] = json!(header);
        }
        if self.kinds.formulas > 0 {
            out["formulas"] = json!(self.kinds.formulas);
        }
        if self.kinds.errors > 0 {
            out["errors"] = json!(self.kinds.errors);
        }
        if let (Some(min), Some(max)) = (self.min, self.max) {
            out["min"] = json!(min);
            out["max"] = json!(max);
        }
        out
    }
}

fn parse_number(content: &str, formatted: &str) -> Option<f64> {
    content.trim().parse::<f64>().ok().or_else(|| {
        let cleaned: String = formatted
            .chars()
            .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
            .collect();
        cleaned.parse::<f64>().ok()
    })
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let head: String = text.chars().take(max_chars).collect();
    format!("{head}…")
}

/// 单张工作表的 L0 摘要。
pub fn sheet_digest(model: &UserModel, sheet: u32, name: &str) -> Result<Value, String> {
    let dimension = model
        .get_model()
        .workbook
        .worksheet(sheet)
        .map_err(|e| e.to_string())?
        .dimension();
    let (min_row, max_row) = (dimension.min_row.max(1), dimension.max_row.max(1));
    let (min_col, max_col) = (dimension.min_column.max(1), dimension.max_column.max(1));

    // 空表：dimension 会退化成 1×1，用内容判断，别谎报一个 A1:A1 的已用区域。
    let first_is_empty = model.get_cell_content(sheet, min_row, min_col)?.is_empty();
    if min_row == max_row && min_col == max_col && first_is_empty {
        return Ok(json!({
            "sheet": name, "index": sheet, "empty": true,
            "usedRange": Value::Null, "rows": 0, "columns": [],
        }));
    }

    let total_rows = max_row - min_row + 1;
    let sampled = total_rows > MAX_PROFILE_ROWS;
    let step = if sampled {
        ((total_rows as f64) / (MAX_PROFILE_ROWS as f64)).ceil() as i32
    } else {
        1
    };

    let mut profiles: Vec<ColumnProfile> = (min_col..=max_col).map(ColumnProfile::new).collect();

    // 表头判定：首行全是非空文本，且下一行至少有一格不是文本，就认为首行是表头。
    let mut header_is_text = true;
    let mut body_differs = false;
    for col in min_col..=max_col {
        let (kind, _, formatted) = classify(model, sheet, min_row, col)?;
        if kind != "text" || formatted.trim().is_empty() {
            header_is_text = false;
        }
        if max_row > min_row {
            let (below, _, _) = classify(model, sheet, min_row + 1, col)?;
            if below != "text" && below != "empty" {
                body_differs = true;
            }
        }
    }
    let header_row = (header_is_text && body_differs).then_some(min_row);

    if let Some(header_row) = header_row {
        for profile in &mut profiles {
            let text = model.get_formatted_cell_value(sheet, header_row, profile.col)?;
            if !text.trim().is_empty() {
                profile.header = Some(truncate(text.trim(), 48));
            }
        }
    }

    let body_start = header_row.map_or(min_row, |r| r + 1);
    let mut scanned_rows = 0i32;
    let mut row = body_start;
    while row <= max_row {
        for profile in &mut profiles {
            let (kind, content, formatted) = classify(model, sheet, row, profile.col)?;
            profile.observe(kind, &content, &formatted);
        }
        scanned_rows += 1;
        row += step;
    }

    let columns: Vec<Value> = profiles
        .iter()
        .filter(|p| p.kinds.total() > 0 || p.header.is_some())
        .map(ColumnProfile::to_json)
        .collect();

    let mut out = json!({
        "sheet": name,
        "index": sheet,
        "usedRange": qualify(name, min_row, min_col, max_row, max_col),
        "rows": total_rows,
        "columns": columns,
    });
    if let Some(header_row) = header_row {
        out["headerRow"] = json!(header_row);
    }
    if sampled {
        // 抽样时统计是估算值，必须说清楚，否则模型会把 nonEmpty 当精确数字用。
        out["sampling"] = json!({
            "sampled": true, "everyNthRow": step, "scannedRows": scanned_rows,
            "note": "行数过多，列统计为抽样估算；需要精确值请对具体区域用 slice 或 stats",
        });
    }
    Ok(out)
}

/// 整个工作簿的 L0 摘要。
pub fn workbook_digest(model: &UserModel) -> Result<Value, String> {
    let names = model.get_model().workbook.get_worksheet_names();
    let mut sheets = Vec::with_capacity(names.len());
    for (index, name) in names.iter().enumerate() {
        sheets.push(sheet_digest(model, index as u32, name)?);
    }
    Ok(json!({ "sheetCount": names.len(), "sheets": sheets }))
}

// ============================== L1 切片 ==============================

/// 行主序切片。显示值放 `rows`，公式单独放 `formulas`，
/// 这样模型既能读到"用户看到的东西"，又能在需要时看到公式而不必为每格背一个对象。
pub fn slice(
    model: &UserModel,
    sheet: u32,
    name: &str,
    r0: i32,
    c0: i32,
    r1: i32,
    c1: i32,
) -> Result<Value, String> {
    let cells = (r1 - r0 + 1) as i64 * (c1 - c0 + 1) as i64;
    if cells > MAX_SLICE_CELLS {
        return Err(format!(
            "区域 {} 有 {cells} 个单元格，超过单次上限 {MAX_SLICE_CELLS}；请缩小范围或先用 digest 看概览",
            format_range(r0, c0, r1, c1)
        ));
    }
    let mut rows = Vec::with_capacity((r1 - r0 + 1) as usize);
    let mut formulas = serde_json::Map::new();
    let mut errors = Vec::new();
    for row in r0..=r1 {
        let mut line = Vec::with_capacity((c1 - c0 + 1) as usize);
        for col in c0..=c1 {
            let (kind, content, formatted) = classify(model, sheet, row, col)?;
            if content.starts_with('=') {
                formulas.insert(format_cell(row, col), json!(content));
            }
            if kind == "error" {
                errors.push(json!({ "ref": format_cell(row, col), "value": formatted }));
            }
            line.push(if kind == "empty" {
                Value::Null
            } else {
                json!(formatted)
            });
        }
        rows.push(Value::Array(line));
    }
    let mut out = json!({
        "range": qualify(name, r0, c0, r1, c1),
        "origin": format_cell(r0, c0),
        "rows": rows,
    });
    if !formulas.is_empty() {
        out["formulas"] = Value::Object(formulas);
    }
    if !errors.is_empty() {
        out["errors"] = json!(errors);
    }
    Ok(out)
}

// ============================== L2 单元格详情 ==============================

#[derive(Debug, Clone, PartialEq, Eq)]
struct FormulaRange {
    sheet: Option<u32>,
    sheet_name: String,
    r0: i32,
    c0: i32,
    r1: i32,
    c1: i32,
}

fn parse_formula_cell_at(formula: &str, start: usize) -> Option<(usize, i32, i32)> {
    let bytes = formula.as_bytes();
    if start >= bytes.len()
        || (start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_'))
    {
        return None;
    }
    let mut end = start;
    if bytes.get(end) == Some(&b'$') {
        end += 1;
    }
    let column_start = end;
    while bytes.get(end).is_some_and(u8::is_ascii_alphabetic) {
        end += 1;
    }
    if end == column_start || end - column_start > 3 {
        return None;
    }
    if bytes.get(end) == Some(&b'$') {
        end += 1;
    }
    let row_start = end;
    while bytes.get(end).is_some_and(u8::is_ascii_digit) {
        end += 1;
    }
    if end == row_start
        || bytes
            .get(end)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_' || *byte == b'(')
    {
        return None;
    }
    let column = letters_to_col(formula[column_start..row_start].trim_end_matches('$'))?;
    let row = formula[row_start..end].parse::<i32>().ok()?;
    (1..=1_048_576).contains(&row).then_some((end, row, column))
}

fn formula_sheet_qualifier(formula: &str, cell_start: usize) -> Option<String> {
    if cell_start == 0 || formula.as_bytes().get(cell_start - 1) != Some(&b'!') {
        return None;
    }
    let prefix = &formula[..cell_start - 1];
    if prefix.ends_with('\'') {
        let bytes = prefix.as_bytes();
        let closing = bytes.len() - 1;
        let mut cursor = closing;
        while cursor > 0 {
            cursor -= 1;
            if bytes[cursor] != b'\'' {
                continue;
            }
            if cursor > 0 && bytes[cursor - 1] == b'\'' {
                cursor -= 1;
                continue;
            }
            return Some(prefix[cursor + 1..closing].replace("''", "'"));
        }
        return None;
    }
    let mut start = prefix.len();
    for (index, ch) in prefix.char_indices().rev() {
        if ch.is_alphanumeric() || matches!(ch, '_' | '.') {
            start = index;
        } else {
            break;
        }
    }
    (start < prefix.len()).then(|| prefix[start..].to_string())
}

fn canonical_sheet(
    names: &[String],
    current_sheet: u32,
    qualifier: Option<String>,
) -> (Option<u32>, String) {
    if let Some(qualifier) = qualifier {
        if let Some((index, name)) = names
            .iter()
            .enumerate()
            .find(|(_, name)| name.eq_ignore_ascii_case(&qualifier))
        {
            return (Some(index as u32), name.clone());
        }
        return (None, qualifier);
    }
    let name = names
        .get(current_sheet as usize)
        .cloned()
        .unwrap_or_else(|| format!("Sheet{}", current_sheet + 1));
    (Some(current_sheet), name)
}

fn formula_ranges(formula: &str, current_sheet: u32, names: &[String]) -> Vec<FormulaRange> {
    let bytes = formula.as_bytes();
    let mut out = Vec::new();
    let mut index = usize::from(bytes.first() == Some(&b'='));
    let mut in_string = false;
    while index < bytes.len() {
        if bytes[index] == b'"' {
            if in_string && bytes.get(index + 1) == Some(&b'"') {
                index += 2;
                continue;
            }
            in_string = !in_string;
            index += 1;
            continue;
        }
        if bytes[index] == b'\'' {
            let mut cursor = index + 1;
            while cursor < bytes.len() {
                if bytes[cursor] != b'\'' {
                    cursor += 1;
                    continue;
                }
                if bytes.get(cursor + 1) == Some(&b'\'') {
                    cursor += 2;
                    continue;
                }
                if bytes.get(cursor + 1) == Some(&b'!') {
                    index = cursor + 2;
                }
                break;
            }
            if index > cursor {
                continue;
            }
        }
        if in_string {
            let ch = formula[index..].chars().next().unwrap();
            index += ch.len_utf8();
            continue;
        }
        let Some((first_end, first_row, first_col)) = parse_formula_cell_at(formula, index) else {
            let ch = formula[index..].chars().next().unwrap();
            index += ch.len_utf8();
            continue;
        };
        // A short sheet name such as S1 is syntactically cell-like.  When immediately
        // followed by ! it is a qualifier, not an unqualified precedent.
        if bytes.get(first_end) == Some(&b'!') {
            index = first_end + 1;
            continue;
        }
        let (end, second_row, second_col) = if bytes.get(first_end) == Some(&b':') {
            parse_formula_cell_at(formula, first_end + 1)
                .map(|(end, row, col)| (end, row, col))
                .unwrap_or((first_end, first_row, first_col))
        } else {
            (first_end, first_row, first_col)
        };
        let (sheet, sheet_name) = canonical_sheet(
            names,
            current_sheet,
            formula_sheet_qualifier(formula, index),
        );
        out.push(FormulaRange {
            sheet,
            sheet_name,
            r0: first_row.min(second_row),
            c0: first_col.min(second_col),
            r1: first_row.max(second_row),
            c1: first_col.max(second_col),
        });
        index = end;
    }
    out
}

fn direct_precedents(formula: &str, current_sheet: u32, names: &[String]) -> Vec<String> {
    let mut refs = std::collections::BTreeSet::new();
    for reference in formula_ranges(formula, current_sheet, names) {
        refs.insert(qualify(
            &reference.sheet_name,
            reference.r0,
            reference.c0,
            reference.r1,
            reference.c1,
        ));
    }
    refs.into_iter().collect()
}

fn direct_dependents(
    model: &UserModel,
    target_sheet: u32,
    target_row: i32,
    target_col: i32,
    names: &[String],
) -> Result<(Vec<String>, bool), String> {
    const MAX_DEPENDENTS: usize = 200;
    let mut out = Vec::new();
    for (sheet_index, sheet_name) in names.iter().enumerate() {
        let sheet = sheet_index as u32;
        let dimension = model
            .get_model()
            .workbook
            .worksheet(sheet)
            .map_err(|error| error.to_string())?
            .dimension();
        for row in dimension.min_row.max(1)..=dimension.max_row.max(1) {
            for col in dimension.min_column.max(1)..=dimension.max_column.max(1) {
                if sheet == target_sheet && row == target_row && col == target_col {
                    continue;
                }
                let content = model.get_cell_content(sheet, row, col)?;
                if !content.starts_with('=') {
                    continue;
                }
                let references_target = formula_ranges(&content, sheet, names).iter().any(|r| {
                    r.sheet == Some(target_sheet)
                        && target_row >= r.r0
                        && target_row <= r.r1
                        && target_col >= r.c0
                        && target_col <= r.c1
                });
                if references_target {
                    out.push(qualify(sheet_name, row, col, row, col));
                    if out.len() >= MAX_DEPENDENTS {
                        return Ok((out, true));
                    }
                }
            }
        }
    }
    Ok((out, false))
}

/// 单格 L2 详情：保留公式源码、屏幕显示值、关键样式和 A1 依赖关系。
pub fn detail(
    model: &UserModel,
    sheet: u32,
    name: &str,
    requested_row: i32,
    requested_col: i32,
) -> Result<Value, String> {
    let names = model.get_model().workbook.get_worksheet_names();
    let mut anchor = (requested_row, requested_col);
    let mut merged = None;
    for area in model.get_merged_cells(sheet)? {
        let Some(area) = parse_range(&area) else {
            continue;
        };
        if requested_row >= area.r0
            && requested_row <= area.r1
            && requested_col >= area.c0
            && requested_col <= area.c1
        {
            anchor = (area.r0, area.c0);
            merged = Some((area.r0, area.c0, area.r1, area.c1));
            break;
        }
    }
    let (row, col) = anchor;
    let (kind, content, formatted) = classify(model, sheet, row, col)?;
    let style = model.get_cell_style(sheet, row, col)?;
    let (horizontal, vertical, wrap_text) = style
        .alignment
        .as_ref()
        .map(|alignment| {
            (
                format!("{:?}", alignment.horizontal).to_lowercase(),
                format!("{:?}", alignment.vertical).to_lowercase(),
                alignment.wrap_text,
            )
        })
        .unwrap_or_else(|| ("general".into(), "bottom".into(), false));
    let precedents = if content.starts_with('=') {
        direct_precedents(&content, sheet, &names)
    } else {
        Vec::new()
    };
    let (dependents, dependents_truncated) = direct_dependents(model, sheet, row, col, &names)?;
    let mut out = json!({
        "ref": qualify(name, row, col, row, col),
        "content": content,
        "formatted": formatted,
        "kind": kind,
        "numberFormat": style.num_fmt,
        "alignment": {
            "horizontal": horizontal,
            "vertical": vertical,
            "wrapText": wrap_text,
        },
        "font": {
            "name": style.font.name,
            "size": style.font.sz,
            "bold": style.font.b,
            "italic": style.font.i,
            "underline": style.font.u,
            "strike": style.font.strike,
            "color": model.resolve_color(&style.font.color),
        },
        "precedents": precedents,
        "dependents": dependents,
    });
    if requested_row != row || requested_col != col {
        out["requestedRef"] = json!(qualify(
            name,
            requested_row,
            requested_col,
            requested_row,
            requested_col,
        ));
    }
    if let Some((r0, c0, r1, c1)) = merged {
        out["merged"] = json!({
            "range": qualify(name, r0, c0, r1, c1),
            "anchor": qualify(name, r0, c0, r0, c0),
        });
    }
    if dependents_truncated {
        out["dependentsTruncated"] = json!(true);
    }
    Ok(out)
}

// ============================== 错误扫描 ==============================

/// 扫描一张表里所有错误值单元格，附带产生它的公式。
/// 模型改完表后据此自我纠正，而不是让它去界面上找红字。
pub fn scan_errors(
    model: &UserModel,
    sheet: u32,
    name: &str,
    limit: usize,
) -> Result<Vec<Value>, String> {
    let dimension = model
        .get_model()
        .workbook
        .worksheet(sheet)
        .map_err(|e| e.to_string())?
        .dimension();
    let mut out = Vec::new();
    for row in dimension.min_row..=dimension.max_row {
        for col in dimension.min_column..=dimension.max_column {
            let formatted = model.get_formatted_cell_value(sheet, row, col)?;
            if !is_error_text(&formatted) {
                continue;
            }
            let content = model.get_cell_content(sheet, row, col)?;
            out.push(json!({
                "ref": qualify(name, row, col, row, col),
                "value": formatted,
                "formula": if content.starts_with('=') { json!(content) } else { Value::Null },
            }));
            if out.len() >= limit {
                return Ok(out);
            }
        }
    }
    Ok(out)
}

// ============================== 写入操作 ==============================

/// 统一 typed-op 协议中的单元格子操作：值、公式、区域和清除。
/// 样式、图表和透视表由主路由组合到同一事务，但仍复用各自经过验证的原生编辑器，
/// 不允许模型接触 OOXML，也不为 AI 建立绕过验证的写入后门。
#[derive(Debug, Clone)]
pub enum CellOp {
    /// 字面值。即使长得像公式（以 = 开头）也当文本写入。
    SetValue {
        sheet: u32,
        row: i32,
        col: i32,
        value: String,
    },
    /// 公式。必须以 = 开头，让"算式"和"字面量"在协议层就分开。
    SetFormula {
        sheet: u32,
        row: i32,
        col: i32,
        formula: String,
    },
    /// 清空内容。
    Clear { sheet: u32, row: i32, col: i32 },
}

impl CellOp {
    pub fn target(&self) -> (u32, i32, i32) {
        match self {
            CellOp::SetValue {
                sheet, row, col, ..
            }
            | CellOp::SetFormula {
                sheet, row, col, ..
            }
            | CellOp::Clear { sheet, row, col } => (*sheet, *row, *col),
        }
    }

    pub fn payload(&self) -> &str {
        match self {
            CellOp::SetValue { value, .. } => value,
            CellOp::SetFormula { formula, .. } => formula,
            CellOp::Clear { .. } => "",
        }
    }
}

/// 把 JSON 请求解析成操作序列。`resolve_sheet` 负责把表名映射成索引，
/// 返回 None 表示表不存在——这种错误要在任何写入发生之前就报出来。
pub fn parse_ops(
    items: &[Value],
    default_sheet: u32,
    resolve_sheet: impl Fn(&str) -> Option<u32>,
) -> Result<Vec<CellOp>, String> {
    let mut ops = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let at = |msg: String| format!("ops[{index}]: {msg}");
        let kind = item
            .get("op")
            .and_then(Value::as_str)
            .ok_or_else(|| at("缺少 op".into()))?;
        let reference = item
            .get("ref")
            .and_then(Value::as_str)
            .ok_or_else(|| at("缺少 ref（A1 地址，如 Sheet1!B2）".into()))?;
        let parsed =
            parse_range(reference).ok_or_else(|| at(format!("无法解析的 A1 地址：{reference}")))?;
        let sheet = match &parsed.sheet_name {
            Some(name) => resolve_sheet(name).ok_or_else(|| at(format!("工作表不存在：{name}")))?,
            None => default_sheet,
        };

        match kind {
            "setValue" | "setFormula" => {
                let single = parsed.r0 == parsed.r1 && parsed.c0 == parsed.c1;
                if !single {
                    return Err(at(format!(
                        "{kind} 只接受单个单元格，区域请用 setRange：{reference}"
                    )));
                }
                if kind == "setFormula" {
                    let formula = item
                        .get("formula")
                        .and_then(Value::as_str)
                        .ok_or_else(|| at("setFormula 缺少 formula".into()))?;
                    if !formula.starts_with('=') {
                        return Err(at(format!("公式必须以 = 开头：{formula}")));
                    }
                    ops.push(CellOp::SetFormula {
                        sheet,
                        row: parsed.r0,
                        col: parsed.c0,
                        formula: formula.to_string(),
                    });
                } else {
                    let value = item
                        .get("value")
                        .map(stringify_scalar)
                        .ok_or_else(|| at("setValue 缺少 value".into()))?;
                    ops.push(CellOp::SetValue {
                        sheet,
                        row: parsed.r0,
                        col: parsed.c0,
                        value,
                    });
                }
            }
            "setRange" => {
                let rows = item
                    .get("values")
                    .and_then(Value::as_array)
                    .ok_or_else(|| at("setRange 缺少 values（二维数组）".into()))?;
                let height = (parsed.r1 - parsed.r0 + 1) as usize;
                let width = (parsed.c1 - parsed.c0 + 1) as usize;
                if rows.len() != height {
                    return Err(at(format!(
                        "values 有 {} 行，区域 {reference} 需要 {height} 行",
                        rows.len()
                    )));
                }
                for (dr, line) in rows.iter().enumerate() {
                    let cells = line
                        .as_array()
                        .ok_or_else(|| at(format!("values[{dr}] 不是数组")))?;
                    if cells.len() != width {
                        return Err(at(format!(
                            "values[{dr}] 有 {} 列，区域需要 {width} 列",
                            cells.len()
                        )));
                    }
                    for (dc, cell) in cells.iter().enumerate() {
                        let row = parsed.r0 + dr as i32;
                        let col = parsed.c0 + dc as i32;
                        if cell.is_null() {
                            ops.push(CellOp::Clear { sheet, row, col });
                        } else {
                            ops.push(CellOp::SetValue {
                                sheet,
                                row,
                                col,
                                value: stringify_scalar(cell),
                            });
                        }
                    }
                }
            }
            "clear" => {
                for row in parsed.r0..=parsed.r1 {
                    for col in parsed.c0..=parsed.c1 {
                        ops.push(CellOp::Clear { sheet, row, col });
                    }
                }
            }
            other => {
                return Err(at(format!(
                    "不支持的操作 {other}；可用：setValue / setFormula / setRange / clear"
                )));
            }
        }
    }
    Ok(ops)
}

/// JSON 标量 → 写入字符串。数字与布尔保留原样，避免被引号包成文本。
fn stringify_scalar(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a1_round_trips_through_letters_and_numbers() {
        assert_eq!(col_to_letters(1), "A");
        assert_eq!(col_to_letters(26), "Z");
        assert_eq!(col_to_letters(27), "AA");
        assert_eq!(col_to_letters(16_384), "XFD");
        for col in [1, 26, 27, 702, 703, 16_384] {
            assert_eq!(letters_to_col(&col_to_letters(col)), Some(col));
        }
        assert_eq!(letters_to_col(""), None);
        assert_eq!(letters_to_col("A1"), None);
        assert_eq!(letters_to_col("XFE"), None);
    }

    #[test]
    fn ranges_parse_sheet_names_absolute_marks_and_reversed_corners() {
        assert_eq!(
            parse_range("B2"),
            Some(ParsedRange {
                sheet_name: None,
                r0: 2,
                c0: 2,
                r1: 2,
                c1: 2
            })
        );
        assert_eq!(
            parse_range("$C$9:$A$1"),
            Some(ParsedRange {
                sheet_name: None,
                r0: 1,
                c0: 1,
                r1: 9,
                c1: 3
            })
        );
        assert_eq!(
            parse_range("Sheet1!A1:C3").map(|r| (r.sheet_name, r.r1, r.c1)),
            Some((Some("Sheet1".into()), 3, 3))
        );
        assert_eq!(
            parse_range("'我的 表'!A1").and_then(|r| r.sheet_name),
            Some("我的 表".into())
        );
        assert_eq!(parse_range(""), None);
        assert_eq!(parse_range("!A1"), None);
        assert_eq!(parse_range("A"), None);
    }

    #[test]
    fn qualify_quotes_only_when_the_sheet_name_needs_it() {
        assert_eq!(qualify("Sheet1", 1, 1, 2, 3), "Sheet1!A1:C2");
        assert_eq!(qualify("我的 表", 1, 1, 1, 1), "'我的 表'!A1");
    }

    #[test]
    fn error_text_detection_ignores_ordinary_hash_prefixed_text() {
        assert!(is_error_text("#DIV/0!"));
        assert!(is_error_text("#NAME?"));
        assert!(is_error_text("#CIRC!"));
        assert!(!is_error_text("#1 号"));
        assert!(!is_error_text("#"));
        assert!(!is_error_text("abc"));
    }

    #[test]
    fn dominant_type_needs_a_clear_majority() {
        let mut kinds = CellKinds {
            numbers: 9,
            texts: 1,
            ..Default::default()
        };
        assert_eq!(kinds.dominant(), "number");
        kinds.texts = 3;
        assert_eq!(kinds.dominant(), "mixed");
        assert_eq!(CellKinds::default().dominant(), "empty");
    }

    #[test]
    fn ops_reject_bad_addresses_formulas_and_shape_mismatches() {
        let resolve = |name: &str| (name == "Sheet1").then_some(0u32);

        let ops = parse_ops(
            &[json!({ "op": "setFormula", "ref": "Sheet1!B2", "formula": "=SUM(A1:A9)" })],
            0,
            resolve,
        )
        .unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].target(), (0, 2, 2));

        let missing_sheet = parse_ops(
            &[json!({ "op": "setValue", "ref": "Nope!A1", "value": 1 })],
            0,
            resolve,
        );
        assert!(missing_sheet.unwrap_err().contains("工作表不存在"));

        let plain_formula = parse_ops(
            &[json!({ "op": "setFormula", "ref": "A1", "formula": "SUM(A1)" })],
            0,
            resolve,
        );
        assert!(plain_formula.unwrap_err().contains("必须以 = 开头"));

        let area_as_single = parse_ops(
            &[json!({ "op": "setValue", "ref": "A1:B2", "value": 1 })],
            0,
            resolve,
        );
        assert!(area_as_single.unwrap_err().contains("setRange"));

        let wrong_shape = parse_ops(
            &[json!({ "op": "setRange", "ref": "A1:B2", "values": [[1, 2]] })],
            0,
            resolve,
        );
        assert!(wrong_shape.unwrap_err().contains("需要 2 行"));

        let unknown = parse_ops(&[json!({ "op": "nuke", "ref": "A1" })], 0, resolve);
        assert!(unknown.unwrap_err().contains("不支持的操作"));
    }

    #[test]
    fn set_range_expands_row_major_and_maps_null_to_clear() {
        let ops = parse_ops(
            &[json!({ "op": "setRange", "ref": "B2:C3", "values": [[1, "x"], [null, true]] })],
            3,
            |_| None,
        )
        .unwrap();
        assert_eq!(ops.len(), 4);
        assert_eq!(ops[0].target(), (3, 2, 2));
        assert_eq!(ops[0].payload(), "1");
        assert_eq!(ops[1].target(), (3, 2, 3));
        assert_eq!(ops[1].payload(), "x");
        assert!(matches!(ops[2], CellOp::Clear { row: 3, col: 2, .. }));
        assert_eq!(ops[3].payload(), "true");
    }

    #[test]
    fn clear_expands_over_the_whole_area() {
        let ops = parse_ops(&[json!({ "op": "clear", "ref": "A1:B3" })], 0, |_| None).unwrap();
        assert_eq!(ops.len(), 6);
        assert!(ops.iter().all(|op| matches!(op, CellOp::Clear { .. })));
    }

    #[test]
    fn formula_references_are_qualified_and_ignore_strings_and_function_names() {
        let names = vec![
            "Sheet1".to_string(),
            "我的 表".to_string(),
            "S1".to_string(),
            "C1".to_string(),
        ];
        let refs = direct_precedents(
            "=SUM(A1:B2,'我的 表'!$C$3,S1!D4,'C1'!E5)+\"F6\"+LOG10(100)",
            0,
            &names,
        );
        assert_eq!(
            refs,
            vec![
                "'我的 表'!C3".to_string(),
                "C1!E5".to_string(),
                "S1!D4".to_string(),
                "Sheet1!A1:B2".to_string(),
            ]
        );
    }

    #[test]
    fn detail_returns_a1_precedents_dependents_and_merge_anchor() {
        let mut model = UserModel::new_empty("detail", "en", "UTC", "en").unwrap();
        model.set_user_input(0, 1, 1, "5").unwrap();
        model.set_user_input(0, 1, 2, "=A1*2").unwrap();
        model.merge_cells_range(0, 2, 1, 3, 2).unwrap();
        model.set_user_input(0, 2, 1, "merged").unwrap();

        let source = detail(&model, 0, "Sheet1", 1, 1).unwrap();
        assert_eq!(source["dependents"], json!(["Sheet1!B1"]));

        let formula = detail(&model, 0, "Sheet1", 1, 2).unwrap();
        assert_eq!(formula["precedents"], json!(["Sheet1!A1"]));
        assert_eq!(formula["formatted"], "10");

        let merged = detail(&model, 0, "Sheet1", 3, 2).unwrap();
        assert_eq!(merged["ref"], "Sheet1!A2");
        assert_eq!(merged["requestedRef"], "Sheet1!B3");
        assert_eq!(merged["merged"]["range"], "Sheet1!A2:B3");
        assert_eq!(merged["merged"]["anchor"], "Sheet1!A2");
    }
}
