//! Runtime enforcement for SpreadsheetML workbook/sheet protection.
//!
//! OOXML protection is not encryption, but Excel still treats it as an editing contract.  This
//! module keeps that contract separate from the lossless XML editor: mutations are classified by
//! the corresponding `sheetProtection` flag and rejected before they reach IronCalc/native
//! journals. Password validation supports both the legacy 16-bit verifier and modern salted
//! spin-hashes.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use roxmltree::Document;
use serde_json::{Map, Value};
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha384, Sha512};
use std::collections::{BTreeMap, BTreeSet, HashMap};

const MAX_PROTECTION_SPINS: u32 = 10_000_000;
const DEFAULT_PROTECTION_SPINS: u32 = 100_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rect {
    r0: u32,
    c0: u32,
    r1: u32,
    c1: u32,
}

#[derive(Debug, Default)]
struct SheetLockMap {
    /// `cellXfs` protection state. Excel's default is locked, including a missing style index.
    styles: Vec<bool>,
    column_styles: Vec<(u32, u32, bool)>,
    row_styles: BTreeMap<u32, bool>,
    cell_styles: HashMap<(u32, u32), bool>,
}

impl SheetLockMap {
    fn style_locked(&self, index: usize) -> bool {
        self.styles.get(index).copied().unwrap_or(true)
    }

    fn column_locked(&self, column: u32) -> bool {
        self.column_styles
            .iter()
            .rev()
            .find(|(first, last, _)| *first <= column && column <= *last)
            .map(|(_, _, locked)| *locked)
            .unwrap_or_else(|| self.style_locked(0))
    }

    fn baseline_locked(&self, row: Option<u32>, column: u32) -> bool {
        row.and_then(|row| self.row_styles.get(&row).copied())
            .unwrap_or_else(|| self.column_locked(column))
    }
}

impl Rect {
    fn contains(self, other: Self) -> bool {
        self.r0 <= other.r0 && self.c0 <= other.c0 && self.r1 >= other.r1 && self.c1 >= other.c1
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    EditCells,
    FormatCells,
    FormatRows,
    FormatColumns,
    InsertRows,
    InsertColumns,
    DeleteRows,
    DeleteColumns,
    Sort,
    AutoFilter,
    PivotTables,
    Objects,
    Scenarios,
}

impl Action {
    fn flag(self) -> Option<&'static str> {
        Some(match self {
            Self::EditCells => return None,
            Self::FormatCells => "formatCells",
            Self::FormatRows => "formatRows",
            Self::FormatColumns => "formatColumns",
            Self::InsertRows => "insertRows",
            Self::InsertColumns => "insertColumns",
            Self::DeleteRows => "deleteRows",
            Self::DeleteColumns => "deleteColumns",
            Self::Sort => "sort",
            Self::AutoFilter => "autoFilter",
            Self::PivotTables => "pivotTables",
            Self::Objects => "objects",
            Self::Scenarios => "scenarios",
        })
    }

    fn label(self) -> &'static str {
        match self {
            Self::EditCells => "编辑锁定单元格",
            Self::FormatCells => "设置单元格格式",
            Self::FormatRows => "设置行格式",
            Self::FormatColumns => "设置列格式",
            Self::InsertRows => "插入行",
            Self::InsertColumns => "插入列",
            Self::DeleteRows => "删除行",
            Self::DeleteColumns => "删除列",
            Self::Sort => "排序",
            Self::AutoFilter => "自动筛选",
            Self::PivotTables => "修改数据透视表",
            Self::Objects => "修改对象",
            Self::Scenarios => "修改方案",
        }
    }

    fn default_blocked(self) -> bool {
        !matches!(self, Self::Objects | Self::Scenarios)
    }

    fn requires_unlocked_target(self) -> bool {
        matches!(
            self,
            Self::EditCells | Self::DeleteRows | Self::DeleteColumns | Self::Sort
        )
    }
}

fn attr<'a>(value: &'a Value, name: &str) -> Option<&'a str> {
    value
        .get(name)
        .or_else(|| value.get("attributes").and_then(|attrs| attrs.get(name)))
        .and_then(Value::as_str)
}

fn truthy(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "on" | "yes"
        )
    })
}

fn sheet_protection_active(value: &Value) -> bool {
    value.is_object() && truthy(attr(value, "sheet"))
}

fn legacy_password_hash(password: &str) -> u16 {
    let characters = password.encode_utf16().collect::<Vec<_>>();
    let mut hash = 0u16;
    for character in characters.iter().rev() {
        hash = ((hash >> 14) & 0x0001) | ((hash << 1) & 0x7fff);
        hash ^= *character;
    }
    hash = ((hash >> 14) & 0x0001) | ((hash << 1) & 0x7fff);
    hash ^ characters.len() as u16 ^ 0xce4b
}

fn digest(algorithm: &str, bytes: &[u8]) -> Result<Vec<u8>, String> {
    Ok(match algorithm.trim().to_ascii_uppercase().as_str() {
        "SHA-1" | "SHA1" => Sha1::digest(bytes).to_vec(),
        "SHA-256" | "SHA256" => Sha256::digest(bytes).to_vec(),
        "SHA-384" | "SHA384" => Sha384::digest(bytes).to_vec(),
        "SHA-512" | "SHA512" => Sha512::digest(bytes).to_vec(),
        other => return Err(format!("unsupported protection hash algorithm: {other}")),
    })
}

/// Builds a modern Excel SHA-512 password record using fresh operating-system entropy.
/// `prefix == Some("workbook")` emits the workbookProtection attribute names; `None` emits
/// sheetProtection/protectedRange names. Protection is an editing guard rather than encryption,
/// but the verifier is still generated with the same salted spin-hash contract as Excel.
pub fn password_hash_attributes(
    password: &str,
    prefix: Option<&str>,
) -> Result<Map<String, Value>, String> {
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt)
        .map_err(|error| format!("password salt generation failed: {error}"))?;
    let mut input = salt.to_vec();
    for unit in password.encode_utf16() {
        input.extend_from_slice(&unit.to_le_bytes());
    }
    let mut hash = Sha512::digest(&input).to_vec();
    for index in 0..DEFAULT_PROTECTION_SPINS {
        let mut round = hash;
        round.extend_from_slice(&index.to_le_bytes());
        hash = Sha512::digest(&round).to_vec();
    }
    let field = |name: &str| match prefix {
        Some(prefix) => format!("{prefix}{}{}", name[..1].to_ascii_uppercase(), &name[1..]),
        None => name.to_string(),
    };
    Ok(Map::from_iter([
        (field("algorithmName"), Value::String("SHA-512".into())),
        (field("hashValue"), Value::String(STANDARD.encode(hash))),
        (field("saltValue"), Value::String(STANDARD.encode(salt))),
        (
            field("spinCount"),
            Value::String(DEFAULT_PROTECTION_SPINS.to_string()),
        ),
    ]))
}

fn prefixed_attr<'a>(value: &'a Value, name: &str) -> Option<&'a str> {
    attr(value, name).or_else(|| {
        let workbook_name = format!("workbook{}{}", name[..1].to_ascii_uppercase(), &name[1..]);
        attr(value, &workbook_name)
    })
}

/// Validates an Excel protection password. A protection record without a password is removable
/// with an empty password, matching Excel's UI.
pub fn verify_password(protection: &Value, password: Option<&str>) -> Result<bool, String> {
    let password = password.unwrap_or_default();
    if let Some(expected) = prefixed_attr(protection, "hashValue") {
        let algorithm = prefixed_attr(protection, "algorithmName")
            .ok_or("protected record has hashValue without algorithmName")?;
        let salt = STANDARD
            .decode(
                prefixed_attr(protection, "saltValue")
                    .ok_or("protected record has no saltValue")?,
            )
            .map_err(|error| format!("invalid protection saltValue: {error}"))?;
        let spins = prefixed_attr(protection, "spinCount")
            .ok_or("protected record has no spinCount")?
            .parse::<u32>()
            .map_err(|_| "invalid protection spinCount".to_string())?;
        if spins > MAX_PROTECTION_SPINS {
            return Err(format!(
                "protection spinCount exceeds {MAX_PROTECTION_SPINS}"
            ));
        }
        let mut input = salt;
        for unit in password.encode_utf16() {
            input.extend_from_slice(&unit.to_le_bytes());
        }
        let mut hash = digest(algorithm, &input)?;
        for index in 0..spins {
            let mut round = hash;
            round.extend_from_slice(&index.to_le_bytes());
            hash = digest(algorithm, &round)?;
        }
        let expected = STANDARD
            .decode(expected)
            .map_err(|error| format!("invalid protection hashValue: {error}"))?;
        return Ok(hash == expected);
    }
    if let Some(expected) =
        attr(protection, "password").or_else(|| attr(protection, "workbookPassword"))
    {
        let expected = u16::from_str_radix(expected.trim_start_matches("0x"), 16)
            .map_err(|_| "invalid legacy protection password hash".to_string())?;
        return Ok(legacy_password_hash(password) == expected);
    }
    Ok(password.is_empty())
}

fn column_index(value: &str) -> Option<u32> {
    let mut result = 0u32;
    let mut found = false;
    for byte in value.bytes() {
        if !byte.is_ascii_alphabetic() {
            return None;
        }
        found = true;
        result = result
            .checked_mul(26)?
            .checked_add(u32::from(byte.to_ascii_uppercase() - b'A' + 1))?;
    }
    found.then_some(result)
}

fn a1_cell(value: &str) -> Option<(u32, u32)> {
    let value = value.trim().trim_matches('$');
    let split = value.find(|character: char| character.is_ascii_digit())?;
    let column = column_index(&value[..split].replace('$', ""))?;
    let row = value[split..].replace('$', "").parse::<u32>().ok()?;
    (row > 0 && column > 0).then_some((row, column))
}

fn a1_rect(value: &str) -> Option<Rect> {
    let reference = value
        .rsplit_once('!')
        .map_or(value, |(_, reference)| reference);
    let mut parts = reference.split(':');
    let (r0, c0) = a1_cell(parts.next()?)?;
    let (r1, c1) = match parts.next() {
        Some(value) => a1_cell(value)?,
        None => (r0, c0),
    };
    (parts.next().is_none()).then_some(Rect {
        r0: r0.min(r1),
        c0: c0.min(c1),
        r1: r0.max(r1),
        c1: c0.max(c1),
    })
}

fn local_name<'a, 'input>(node: roxmltree::Node<'a, 'input>) -> &'input str {
    node.tag_name().name()
}

fn xml_bool(value: Option<&str>, default: bool) -> bool {
    value.map_or(default, |value| truthy(Some(value)))
}

fn normalize_part_path(base_part: &str, target: &str) -> String {
    let target = target.replace('\\', "/");
    let combined = if target.starts_with('/') {
        target.trim_start_matches('/').to_string()
    } else {
        let directory = base_part
            .rsplit_once('/')
            .map_or("", |(directory, _)| directory);
        if directory.is_empty() {
            target
        } else {
            format!("{directory}/{target}")
        }
    };
    let mut segments = Vec::new();
    for segment in combined.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            value => segments.push(value),
        }
    }
    segments.join("/")
}

fn relationships_part(part: &str) -> String {
    let (directory, file) = part.rsplit_once('/').unwrap_or(("", part));
    if directory.is_empty() {
        format!("_rels/{file}.rels")
    } else {
        format!("{directory}/_rels/{file}.rels")
    }
}

fn styles_part<'a>(model: &'a Value, parts: &'a BTreeMap<String, Vec<u8>>) -> Option<&'a [u8]> {
    let workbook_part = model.get("workbookPart").and_then(Value::as_str)?;
    let rels = parts.get(&relationships_part(workbook_part))?;
    let document = Document::parse(std::str::from_utf8(rels).ok()?).ok()?;
    let target = document.descendants().find_map(|node| {
        (node.is_element()
            && local_name(node) == "Relationship"
            && node
                .attribute("Type")
                .is_some_and(|kind| kind.ends_with("/styles")))
        .then(|| node.attribute("Target"))
        .flatten()
    })?;
    parts
        .get(&normalize_part_path(workbook_part, target))
        .map(Vec::as_slice)
}

fn style_protection(node: roxmltree::Node<'_, '_>, inherited: bool) -> bool {
    let apply = xml_bool(node.attribute("applyProtection"), true);
    if !apply {
        return inherited;
    }
    node.children()
        .find(|child| child.is_element() && local_name(*child) == "protection")
        .map_or(inherited, |protection| {
            xml_bool(protection.attribute("locked"), true)
        })
}

fn parse_style_locks(styles_xml: Option<&[u8]>) -> Vec<bool> {
    let Some(styles_xml) = styles_xml.and_then(|bytes| std::str::from_utf8(bytes).ok()) else {
        return vec![true];
    };
    let Ok(document) = Document::parse(styles_xml) else {
        return vec![true];
    };
    let base = document
        .descendants()
        .find(|node| node.is_element() && local_name(*node) == "cellStyleXfs")
        .map(|container| {
            container
                .children()
                .filter(|node| node.is_element() && local_name(*node) == "xf")
                .map(|node| style_protection(node, true))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut result = document
        .descendants()
        .find(|node| node.is_element() && local_name(*node) == "cellXfs")
        .map(|container| {
            container
                .children()
                .filter(|node| node.is_element() && local_name(*node) == "xf")
                .map(|node| {
                    let inherited = node
                        .attribute("xfId")
                        .and_then(|value| value.parse::<usize>().ok())
                        .and_then(|index| base.get(index).copied())
                        .unwrap_or(true);
                    style_protection(node, inherited)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if result.is_empty() {
        result.push(true);
    }
    result
}

fn parse_sheet_lock_map(
    model: &Value,
    sheet: &Value,
    parts: &BTreeMap<String, Vec<u8>>,
) -> Result<SheetLockMap, String> {
    let part = sheet
        .get("part")
        .and_then(Value::as_str)
        .ok_or("protected worksheet has no OOXML part path")?;
    let xml = parts
        .get(part)
        .ok_or_else(|| format!("missing protected worksheet part {part}"))?;
    let xml = std::str::from_utf8(xml).map_err(|_| format!("{part} is not UTF-8 XML"))?;
    let document = Document::parse(xml).map_err(|error| format!("{part}: {error}"))?;
    let mut result = SheetLockMap {
        styles: parse_style_locks(styles_part(model, parts)),
        ..Default::default()
    };
    for node in document.descendants().filter(|node| node.is_element()) {
        match local_name(node) {
            "col" => {
                let Some(first) = node
                    .attribute("min")
                    .and_then(|value| value.parse::<u32>().ok())
                else {
                    continue;
                };
                let Some(last) = node
                    .attribute("max")
                    .and_then(|value| value.parse::<u32>().ok())
                else {
                    continue;
                };
                let Some(style) = node
                    .attribute("style")
                    .and_then(|value| value.parse::<usize>().ok())
                else {
                    continue;
                };
                result.column_styles.push((
                    first.min(last),
                    first.max(last),
                    result.style_locked(style),
                ));
            }
            "row" => {
                let Some(row) = node
                    .attribute("r")
                    .and_then(|value| value.parse::<u32>().ok())
                else {
                    continue;
                };
                let Some(style) = node
                    .attribute("s")
                    .and_then(|value| value.parse::<usize>().ok())
                else {
                    continue;
                };
                result.row_styles.insert(row, result.style_locked(style));
            }
            "c" => {
                let Some((row, column)) = node.attribute("r").and_then(a1_cell) else {
                    continue;
                };
                let style = node
                    .attribute("s")
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);
                result
                    .cell_styles
                    .insert((row, column), result.style_locked(style));
            }
            _ => {}
        }
    }
    Ok(result)
}

fn authorized_ranges(sheet: &Value, password: Option<&str>) -> Result<Vec<Rect>, String> {
    let Some(ranges) = sheet.get("protectedRanges").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut result = Vec::new();
    for range in ranges {
        if attr(range, "securityDescriptor").is_some() || !verify_password(range, password)? {
            continue;
        }
        if let Some(reference) = attr(range, "sqref") {
            result.extend(reference.split_ascii_whitespace().filter_map(a1_rect));
        }
    }
    Ok(result)
}

fn push_rect_boundaries(
    target: Rect,
    rect: Rect,
    rows: &mut BTreeSet<u32>,
    columns: &mut BTreeSet<u32>,
) {
    if rect.r1 < target.r0 || rect.r0 > target.r1 || rect.c1 < target.c0 || rect.c0 > target.c1 {
        return;
    }
    rows.insert(rect.r0.max(target.r0));
    if let Some(after) = rect.r1.min(target.r1).checked_add(1) {
        rows.insert(after);
    }
    columns.insert(rect.c0.max(target.c0));
    if let Some(after) = rect.c1.min(target.c1).checked_add(1) {
        columns.insert(after);
    }
}

fn target_is_editable(lock_map: &SheetLockMap, target: Rect, allowed: &[Rect]) -> bool {
    let mut row_boundaries = BTreeSet::from([target.r0, target.r1.saturating_add(1)]);
    let mut column_boundaries = BTreeSet::from([target.c0, target.c1.saturating_add(1)]);
    for (&row, _) in lock_map.row_styles.range(target.r0..=target.r1) {
        row_boundaries.insert(row);
        row_boundaries.insert(row.saturating_add(1));
    }
    for &(row, column) in lock_map.cell_styles.keys() {
        if target.r0 <= row && row <= target.r1 && target.c0 <= column && column <= target.c1 {
            row_boundaries.insert(row);
            row_boundaries.insert(row.saturating_add(1));
            column_boundaries.insert(column);
            column_boundaries.insert(column.saturating_add(1));
        }
    }
    for &(first, last, _) in &lock_map.column_styles {
        if last >= target.c0 && first <= target.c1 {
            column_boundaries.insert(first.max(target.c0));
            column_boundaries.insert(last.min(target.c1).saturating_add(1));
        }
    }
    for &rect in allowed {
        push_rect_boundaries(target, rect, &mut row_boundaries, &mut column_boundaries);
    }
    let rows = row_boundaries.into_iter().collect::<Vec<_>>();
    let columns = column_boundaries.into_iter().collect::<Vec<_>>();
    for row_pair in rows.windows(2) {
        let row = row_pair[0];
        if row > target.r1 {
            continue;
        }
        for column_pair in columns.windows(2) {
            let column = column_pair[0];
            if column > target.c1 {
                continue;
            }
            let locked = lock_map
                .cell_styles
                .get(&(row, column))
                .copied()
                .unwrap_or_else(|| lock_map.baseline_locked(Some(row), column));
            if locked
                && !allowed.iter().any(|rect| {
                    rect.r0 <= row && row <= rect.r1 && rect.c0 <= column && column <= rect.c1
                })
            {
                return false;
            }
        }
    }
    true
}

fn number(body: &Value, name: &str) -> Option<u32> {
    body.get(name)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
}

fn declared_ranges(path: &str, body: &Value) -> Option<Vec<Rect>> {
    if path == "/api/batch" {
        return body
            .get("cells")?
            .as_array()?
            .iter()
            .map(|cell| {
                Some(Rect {
                    r0: number(cell, "r")?,
                    c0: number(cell, "c")?,
                    r1: number(cell, "r")?,
                    c1: number(cell, "c")?,
                })
            })
            .collect();
    }
    if path == "/api/paste" {
        let row = number(body, "row")?;
        let column = number(body, "col")?;
        let special = body.get("special").and_then(Value::as_str).unwrap_or("all");
        let transpose = special.starts_with("transpose");
        let (mut height, mut width) = body
            .get("unicell")
            .map(|payload| {
                (
                    number(payload, "height").unwrap_or(1).max(1),
                    number(payload, "width").unwrap_or(1).max(1),
                )
            })
            .unwrap_or_else(|| {
                let text = body.get("text").and_then(Value::as_str).unwrap_or_default();
                let mut rows = text.split('\n').collect::<Vec<_>>();
                if rows.len() > 1
                    && rows
                        .last()
                        .is_some_and(|line| line.trim_end_matches('\r').is_empty())
                {
                    rows.pop();
                }
                let height = rows.len().max(1) as u32;
                let width = rows
                    .iter()
                    .map(|line| line.trim_end_matches('\r').split('\t').count())
                    .max()
                    .unwrap_or(1)
                    .max(1) as u32;
                (height, width)
            });
        if transpose {
            std::mem::swap(&mut height, &mut width);
        }
        return Some(vec![Rect {
            r0: row,
            c0: column,
            r1: row.saturating_add(height - 1).min(1_048_576),
            c1: column.saturating_add(width - 1).min(16_384),
        }]);
    }
    if let (Some(row), Some(column)) = (number(body, "row"), number(body, "col")) {
        return Some(vec![Rect {
            r0: row,
            c0: column,
            r1: row,
            c1: column,
        }]);
    }
    if let (Some(r0), Some(c0), Some(r1), Some(c1)) = (
        number(body, "r0"),
        number(body, "c0"),
        number(body, "r1"),
        number(body, "c1"),
    ) {
        return Some(vec![Rect { r0, c0, r1, c1 }]);
    }
    if let (Some(r0), Some(c0), Some(r1), Some(c1)) = (
        number(body, "dr0"),
        number(body, "dc0"),
        number(body, "dr1"),
        number(body, "dc1"),
    ) {
        return Some(vec![Rect { r0, c0, r1, c1 }]);
    }
    if path == "/api/rows" && body.get("op").and_then(Value::as_str) == Some("delete") {
        let first = number(body, "row")?;
        let count = number(body, "count").unwrap_or(1).max(1);
        return Some(vec![Rect {
            r0: first,
            c0: 1,
            r1: first.saturating_add(count - 1).min(1_048_576),
            c1: 16_384,
        }]);
    }
    if path == "/api/cols" && body.get("op").and_then(Value::as_str) == Some("delete") {
        let first = number(body, "col")?;
        let count = number(body, "count").unwrap_or(1).max(1);
        return Some(vec![Rect {
            r0: 1,
            c0: first,
            r1: 1_048_576,
            c1: first.saturating_add(count - 1).min(16_384),
        }]);
    }
    None
}

fn actions(path: &str, body: &Value) -> Vec<Action> {
    match path {
        "/api/input" | "/api/inputrange" | "/api/batch" | "/api/rich-text" | "/api/replace"
        | "/api/autofill" => vec![Action::EditCells],
        "/api/paste" => match body.get("special").and_then(Value::as_str) {
            Some("formats" | "transpose-formats") => vec![Action::FormatCells],
            Some("values" | "formulas" | "transpose-values" | "transpose-formulas") => {
                vec![Action::EditCells]
            }
            _ => vec![Action::EditCells, Action::FormatCells],
        },
        "/api/cf" if body.get("op").and_then(Value::as_str) == Some("list") => vec![],
        "/api/style" | "/api/border" | "/api/fontname" | "/api/copystyle" | "/api/cf"
        | "/api/dv" | "/api/merge" => vec![Action::FormatCells],
        "/api/clear" => match body.get("what").and_then(Value::as_str) {
            Some("contents") => vec![Action::EditCells],
            Some("formatting") => vec![Action::FormatCells],
            _ => vec![Action::EditCells, Action::FormatCells],
        },
        "/api/rows" => match body.get("op").and_then(Value::as_str) {
            Some("insert") => vec![Action::InsertRows],
            Some("delete") => vec![Action::DeleteRows],
            _ => vec![Action::FormatRows],
        },
        "/api/cols" => match body.get("op").and_then(Value::as_str) {
            Some("insert") => vec![Action::InsertColumns],
            Some("delete") => vec![Action::DeleteColumns],
            _ => vec![Action::FormatColumns],
        },
        "/api/rowheight" => vec![Action::FormatRows],
        "/api/colwidth" => vec![Action::FormatColumns],
        "/api/sort" => vec![Action::Sort],
        "/api/filter" => vec![Action::AutoFilter],
        "/api/pivot-caches"
        | "/api/pivot-tables"
        | "/api/pivot-local-refresh"
        | "/api/slicers"
        | "/api/timelines" => vec![Action::PivotTables],
        "/api/objects" | "/api/native-drawing/validate" => vec![Action::Objects],
        "/api/tables" => vec![Action::AutoFilter, Action::FormatCells],
        "/api/what-if"
            if matches!(
                body.get("op").and_then(Value::as_str),
                Some("create" | "update" | "delete")
            ) =>
        {
            vec![Action::Scenarios]
        }
        _ => Vec::new(),
    }
}

fn action_is_blocked(protection: &Value, action: Action) -> bool {
    action.flag().is_none_or(|flag| {
        attr(protection, flag).map_or(action.default_blocked(), |value| truthy(Some(value)))
    })
}

fn enforce_mutation_inner(
    model: &Value,
    parts: Option<&BTreeMap<String, Vec<u8>>>,
    path: &str,
    body: &Value,
) -> Result<(), String> {
    if path == "/api/page-review" {
        return Ok(());
    }
    if matches!(path, "/api/sheet" | "/api/names")
        && model.get("workbookProtection").is_some_and(|protection| {
            protection.is_object() && truthy(attr(protection, "lockStructure"))
        })
    {
        return Err("工作簿结构受保护，不能新增、删除、重命名工作表或名称".into());
    }
    let actions = actions(path, body);
    if actions.is_empty() {
        return Ok(());
    }
    let sheet_index = body
        .get("sheet")
        .and_then(Value::as_u64)
        .map(|value| value as usize);
    let worksheets = model
        .get("worksheets")
        .and_then(Value::as_array)
        .ok_or("page-review model has no worksheets")?;
    let selected = worksheets
        .iter()
        .enumerate()
        .filter(|(index, _)| sheet_index.is_none_or(|sheet| sheet == *index));
    let password = body.get("protectionPassword").and_then(Value::as_str);
    for (_, sheet) in selected {
        let protection = sheet.get("sheetProtection").unwrap_or(&Value::Null);
        if !sheet_protection_active(protection) {
            continue;
        }
        for action in &actions {
            let blocked = action_is_blocked(protection, *action);
            if action.requires_unlocked_target() {
                if let Some(targets) = declared_ranges(path, body) {
                    let allowed = authorized_ranges(sheet, password)?;
                    let all_editable = if let Some(parts) = parts {
                        let lock_map = parse_sheet_lock_map(model, sheet, parts)?;
                        targets
                            .into_iter()
                            .all(|target| target_is_editable(&lock_map, target, &allowed))
                    } else {
                        targets
                            .into_iter()
                            .all(|target| allowed.iter().any(|range| range.contains(target)))
                    };
                    // Excel requires both permissions for operations such as Sort/Delete:
                    // the sheetProtection flag must allow the command and every cell touched by
                    // the command must be unlocked (or covered by an authorized protected range).
                    // Do not let an allowed command flag bypass the locked-cell check below.
                    if !all_editable {
                        return Err(format!(
                            "worksheet protection blocks {} because the target contains locked cells",
                            action.label()
                        ));
                    }
                    if all_editable && (!blocked || *action == Action::EditCells) {
                        continue;
                    }
                }
            }
            if !blocked {
                continue;
            }
            return Err(format!("工作表受保护，不能{}", action.label()));
        }
    }
    Ok(())
}

/// Rejects a mutating API request that Excel sheet/workbook protection would disallow.
///
/// This model-only entry point is kept for callers without an OOXML package. Protected ranges are
/// still honored, while ordinary cells conservatively remain locked (Excel's default).
pub fn enforce_mutation(model: &Value, path: &str, body: &Value) -> Result<(), String> {
    enforce_mutation_inner(model, None, path, body)
}

/// Full protection enforcement using the materialized workbook package. In addition to protected
/// ranges, this resolves `cellXfs` protection through cell, row and column styles so cells formatted
/// as unlocked stay editable exactly as they do in Excel.
pub fn enforce_mutation_with_parts(
    model: &Value,
    parts: &BTreeMap<String, Vec<u8>>,
    path: &str,
    body: &Value,
) -> Result<(), String> {
    enforce_mutation_inner(model, Some(parts), path, body)
}

fn request_password<'a>(request: &'a Value, kind: &str, name: Option<&str>) -> Option<&'a str> {
    let scoped = request.get("currentPasswords");
    if kind == "workbook"
        && let Some(password) = scoped
            .and_then(|value| value.get("workbook"))
            .and_then(Value::as_str)
    {
        return Some(password);
    }
    if kind == "worksheet"
        && let Some(name) = name
        && let Some(worksheets) = scoped.and_then(|value| value.get("worksheets"))
    {
        if let Some(password) = worksheets.get(name).and_then(Value::as_str) {
            return Some(password);
        }
        if let Some(password) = worksheets.as_array().and_then(|entries| {
            entries.iter().find_map(|entry| {
                (entry.get("sheet").and_then(Value::as_str) == Some(name))
                    .then(|| entry.get("password").and_then(Value::as_str))
                    .flatten()
            })
        }) {
            return Some(password);
        }
    }
    request.get("password").and_then(Value::as_str)
}

fn identity_scalar_eq(left: Option<&Value>, right: Option<&Value>) -> bool {
    match (left, right) {
        (Some(Value::String(left)), Some(Value::String(right))) => left == right,
        (Some(Value::Number(left)), Some(Value::Number(right))) => left == right,
        (Some(Value::String(left)), Some(Value::Number(right)))
        | (Some(Value::Number(right)), Some(Value::String(left))) => left == &right.to_string(),
        _ => false,
    }
}

fn worksheet_patch_matches(
    worksheet: &Value,
    sheet: &Value,
    sheet_name: &str,
    sheet_index: usize,
) -> bool {
    worksheet
        .get("sheet")
        .and_then(Value::as_str)
        .is_some_and(|name| name == sheet_name || name.parse::<usize>().ok() == Some(sheet_index))
        || worksheet.get("name").and_then(Value::as_str) == Some(sheet_name)
        || identity_scalar_eq(worksheet.get("sheetId"), sheet.get("sheetId"))
        || worksheet
            .get("part")
            .and_then(Value::as_str)
            .is_some_and(|part| sheet.get("part").and_then(Value::as_str) == Some(part))
        || worksheet
            .get("localSheetId")
            .and_then(Value::as_u64)
            .is_some_and(|value| value as usize == sheet_index)
}

fn matching_worksheet_patches<'a>(
    patch: &'a Value,
    sheet: &Value,
    sheet_name: &str,
    sheet_index: usize,
) -> Vec<&'a Value> {
    patch
        .get("worksheets")
        .and_then(Value::as_array)
        .map(|worksheets| {
            worksheets
                .iter()
                .filter(|worksheet| {
                    worksheet_patch_matches(worksheet, sheet, sheet_name, sheet_index)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn worksheet_patch_touches_protection(worksheet: &Value) -> bool {
    worksheet.get("sheetProtection").is_some() || worksheet.get("protectedRanges").is_some()
}

fn worksheet_patch_requires_protected_sheet_password(
    worksheet: &Value,
    protection: &Value,
) -> bool {
    // Page Setup, print names, headers/footers and manual breaks have no corresponding
    // `Allow...` flag in Worksheet.Protect. Excel therefore requires the sheet to be
    // unprotected before those properties can be changed.
    const PAGE_LAYOUT_FIELDS: &[&str] = &[
        "pageMargins",
        "pageSetup",
        "printOptions",
        "headerFooter",
        "rowBreaks",
        "colBreaks",
        "printArea",
        "printTitles",
    ];
    if PAGE_LAYOUT_FIELDS
        .iter()
        .any(|field| worksheet.get(*field).is_some())
    {
        return true;
    }
    // Legacy notes are VML drawing objects; threaded comments share the same user-facing
    // review operation. Honour sheetProtection/@objects when deciding whether they are editable.
    (worksheet.get("notes").is_some() || worksheet.get("threadedComments").is_some())
        && action_is_blocked(protection, Action::Objects)
}

/// Requires the current password before an existing protection record can be changed/reset.
pub fn authorize_protection_edit(model: &Value, request: &Value) -> Result<(), String> {
    let reset = request.get("op").and_then(Value::as_str) == Some("reset");
    let patch = request.get("patch").unwrap_or(request);
    if (reset || patch.get("workbookProtection").is_some())
        && let Some(protection) = model
            .get("workbookProtection")
            .filter(|value| value.is_object())
        && !verify_password(protection, request_password(request, "workbook", None))?
    {
        return Err("工作簿保护密码不正确".into());
    }
    if let Some(worksheets) = model.get("worksheets").and_then(Value::as_array) {
        for (index, sheet) in worksheets.iter().enumerate() {
            let name = sheet.get("name").and_then(Value::as_str).unwrap_or("?");
            let protection = sheet.get("sheetProtection").unwrap_or(&Value::Null);
            if !sheet_protection_active(protection) {
                continue;
            }
            let worksheet_patches = matching_worksheet_patches(patch, sheet, name, index);
            let requires_password = reset
                || worksheet_patches.iter().any(|worksheet| {
                    worksheet_patch_touches_protection(worksheet)
                        || worksheet_patch_requires_protected_sheet_password(worksheet, protection)
                });
            if requires_password
                && !verify_password(
                    protection,
                    request_password(request, "worksheet", Some(name)),
                )?
            {
                return Err(format!("工作表“{}”保护密码不正确", name));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn legacy_password_matches_excel_verifier() {
        assert_eq!(legacy_password_hash("password"), 0x83af);
        assert!(verify_password(&json!({"password":"83AF"}), Some("password")).unwrap());
        assert!(!verify_password(&json!({"password":"83AF"}), Some("wrong")).unwrap());
    }

    #[test]
    fn generated_modern_hash_round_trips_through_excel_verifier() {
        let sheet = Value::Object(password_hash_attributes("强密码-42", None).unwrap());
        assert!(verify_password(&sheet, Some("强密码-42")).unwrap());
        assert!(!verify_password(&sheet, Some("wrong")).unwrap());
        let workbook = Value::Object(password_hash_attributes("book", Some("workbook")).unwrap());
        assert!(verify_password(&workbook, Some("book")).unwrap());
        assert!(!verify_password(&workbook, Some("wrong")).unwrap());
    }

    #[test]
    fn protected_sheet_blocks_locked_edits_but_allows_declared_input_range() {
        let model = json!({"worksheets":[{
            "name":"Sheet1",
            "sheetProtection":{"sheet":"1"},
            "protectedRanges":[{"name":"Input", "sqref":"B2:C4"}]
        }]});
        assert!(
            enforce_mutation(&model, "/api/input", &json!({"sheet":0,"row":2,"col":2})).is_ok()
        );
        assert!(
            enforce_mutation(&model, "/api/input", &json!({"sheet":0,"row":5,"col":2})).is_err()
        );
        assert!(
            enforce_mutation(
                &model,
                "/api/style",
                &json!({"sheet":0,"r0":2,"c0":2,"r1":2,"c1":2})
            )
            .is_err()
        );
    }

    #[test]
    fn explicit_allow_flags_match_excel_sheet_protection_semantics() {
        let model = json!({"worksheets":[{
            "sheetProtection":{"sheet":"1", "sort":"0", "autoFilter":"0"},
            "protectedRanges":[]
        }]});
        assert!(enforce_mutation(&model, "/api/sort", &json!({"sheet":0})).is_ok());
        assert!(enforce_mutation(&model, "/api/filter", &json!({"sheet":0})).is_ok());
        assert!(enforce_mutation(&model, "/api/objects", &json!({"sheet":0})).is_ok());
        let objects_locked = json!({"worksheets":[{
            "sheetProtection":{"sheet":"1", "objects":"1"}, "protectedRanges":[]
        }]});
        assert!(enforce_mutation(&objects_locked, "/api/objects", &json!({"sheet":0})).is_err());
    }

    #[test]
    fn conditional_format_manager_list_is_read_only_but_mutations_require_format_permission() {
        let model = json!({"worksheets":[{
            "sheetProtection":{"sheet":"1", "formatCells":"1"},
            "protectedRanges":[]
        }]});
        assert!(enforce_mutation(&model, "/api/cf", &json!({"sheet":0,"op":"list"})).is_ok());
        assert!(
            enforce_mutation(
                &model,
                "/api/cf",
                &json!({"sheet":0,"op":"update","index":0,"range":"A1:A2"})
            )
            .is_err()
        );
    }

    #[test]
    fn materialized_styles_allow_only_excel_unlocked_cells() {
        let model = json!({
            "workbookPart":"xl/workbook.xml",
            "worksheets":[{
                "name":"Sheet1",
                "part":"xl/worksheets/sheet1.xml",
                "sheetProtection":{"sheet":"1","sort":"0","deleteRows":"0"},
                "protectedRanges":[{"name":"Open", "sqref":"E1:F2"}]
            }]
        });
        let mut parts = BTreeMap::new();
        parts.insert(
            "xl/_rels/workbook.xml.rels".into(),
            br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdStyles" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#.to_vec(),
        );
        parts.insert(
            "xl/styles.xml".into(),
            br#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><cellStyleXfs count="1"><xf><protection locked="1"/></xf></cellStyleXfs><cellXfs count="2"><xf xfId="0"/><xf xfId="0" applyProtection="1"><protection locked="0"/></xf></cellXfs></styleSheet>"#.to_vec(),
        );
        parts.insert(
            "xl/worksheets/sheet1.xml".into(),
            br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><cols><col min="3" max="3" style="1"/></cols><sheetData><row r="2" s="1" customFormat="1"><c r="B2" s="0"/></row></sheetData><sheetProtection sheet="1"/></worksheet>"#.to_vec(),
        );

        let check = |r0, c0, r1, c1| {
            enforce_mutation_with_parts(
                &model,
                &parts,
                "/api/inputrange",
                &json!({"sheet":0,"r0":r0,"c0":c0,"r1":r1,"c1":c1}),
            )
        };
        assert!(
            check(1, 3, 100, 3).is_ok(),
            "unlocked column style must cover blank cells"
        );
        assert!(
            check(2, 1, 2, 1).is_ok(),
            "unlocked row style must cover blank cells"
        );
        assert!(
            check(2, 2, 2, 2).is_err(),
            "explicit locked cell style overrides the row"
        );
        assert!(check(1, 1, 1, 1).is_err(), "default style remains locked");
        assert!(
            check(1, 5, 2, 6).is_ok(),
            "password-free protected range stays editable"
        );
        assert!(
            check(1, 3, 2, 4).is_err(),
            "a mixed unlocked/locked paste is rejected atomically"
        );
        assert!(
            enforce_mutation_with_parts(
                &model,
                &parts,
                "/api/sort",
                &json!({"sheet":0,"r0":1,"c0":3,"r1":100,"c1":3}),
            )
            .is_ok(),
            "allowed sorting still requires and accepts a fully unlocked range"
        );
        assert!(
            enforce_mutation_with_parts(
                &model,
                &parts,
                "/api/sort",
                &json!({"sheet":0,"r0":1,"c0":1,"r1":1,"c1":3}),
            )
            .is_err(),
            "allowed sorting must reject a range containing locked cells"
        );
        assert!(
            enforce_mutation_with_parts(
                &model,
                &parts,
                "/api/rows",
                &json!({"sheet":0,"op":"delete","row":2,"count":1}),
            )
            .is_err(),
            "row deletion requires every cell in the row to be unlocked"
        );
    }

    #[test]
    fn protection_editor_verifies_only_the_records_touched_by_the_patch() {
        let model = json!({
            "workbookProtection":{"lockStructure":"1","workbookPassword":"83AF"},
            "worksheets":[
                {"name":"One","sheetId":1,"part":"xl/worksheets/sheet1.xml",
                 "sheetProtection":{"sheet":"1","objects":"1","password":"83AF"}},
                {"name":"Two","sheetId":2,"part":"xl/worksheets/sheet2.xml",
                 "sheetProtection":{"sheet":"1","password":"CDD6"}}
            ]
        });
        let one = json!({"op":"update","password":"password","patch":{"worksheets":[{
            "sheet":"One","sheetProtection":{"sort":"0"}
        }]}});
        assert!(authorize_protection_edit(&model, &one).is_ok());
        let wrong_sheet = json!({"op":"update","password":"password","patch":{"worksheets":[{
            "sheet":"Two","sheetProtection":{"sort":"0"}
        }]}});
        assert!(authorize_protection_edit(&model, &wrong_sheet).is_err());
        let workbook = json!({"op":"update","password":"password","patch":{
            "workbookProtection":{"lockStructure":"0"}
        }});
        assert!(authorize_protection_edit(&model, &workbook).is_ok());

        // The editor identifies worksheets by native sheetId/part, not only by display name.
        let protected_layout = json!({"op":"update","patch":{"worksheets":[{
            "sheetId":1,"part":"xl/worksheets/sheet1.xml","pageSetup":{"paperSize":"9"}
        }]}});
        assert!(authorize_protection_edit(&model, &protected_layout).is_err());
        let protected_layout_authorized = json!({"op":"update","password":"password","patch":{"worksheets":[{
            "sheetId":1,"part":"xl/worksheets/sheet1.xml","pageSetup":{"paperSize":"9"}
        }]}});
        assert!(authorize_protection_edit(&model, &protected_layout_authorized).is_ok());

        let protected_note = json!({"op":"update","patch":{"worksheets":[{
            "part":"xl/worksheets/sheet1.xml","notes":{"upsert":[{"ref":"A1","text":"blocked"}]}
        }]}});
        assert!(authorize_protection_edit(&model, &protected_note).is_err());
        let comments_allowed = json!({
            "worksheets":[{"name":"One","sheetId":1,"part":"xl/worksheets/sheet1.xml",
                "sheetProtection":{"sheet":"1","objects":"0","password":"83AF"}}]
        });
        assert!(authorize_protection_edit(&comments_allowed, &protected_note).is_ok());
    }
}
