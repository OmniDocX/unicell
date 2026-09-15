//! Deterministic local PivotTable/PivotCache refresh engine.
//!
//! This module intentionally has no dependency on `AppState`, IronCalc, a filesystem, or a
//! network provider.  Callers pass either a simple `{ fields, rows }` table or an IronCalc-style
//! worksheet snapshot (`cells: [{ r, c, v }]`) plus a pivot layout.  The result contains:
//!
//! * an immediately renderable two-dimensional pivot result;
//! * a typed PivotCache records model and deterministic OOXML records payload;
//! * differential patches accepted by `native_pivot_table_edit` and
//!   `native_pivot_cache_edit`, plus a cache-field/record rewrite plan for the package layer.
//!
//! Worksheet caches are supported.  OLAP/CUBE, MDX, VertiPaq/Data Model, calculated members,
//! and DAX are rejected explicitly: silently presenting stale server aggregates as a local
//! refresh would be substantially worse than a precise boundary diagnostic.

use roxmltree::{Document, Node};
use serde_json::{Map, Number, Value, json};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::ops::Range;

type PivotResult<T> = Result<T, PivotRefreshError>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PivotRefreshError {
    pub code: &'static str,
    pub path: String,
    pub message: String,
}

impl PivotRefreshError {
    fn new(code: &'static str, path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code,
            path: path.into(),
            message: message.into(),
        }
    }

    pub(crate) fn as_json(&self) -> Value {
        json!({ "code": self.code, "path": self.path, "message": self.message })
    }
}

impl fmt::Display for PivotRefreshError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} at {}: {}",
            self.code, self.path, self.message
        )
    }
}

impl std::error::Error for PivotRefreshError {}

fn error<T>(
    code: &'static str,
    path: impl Into<String>,
    message: impl Into<String>,
) -> PivotResult<T> {
    Err(PivotRefreshError::new(code, path, message))
}

#[derive(Clone, Debug)]
struct SourceTable {
    fields: Vec<String>,
    rows: Vec<Vec<Value>>,
    date_1904: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum DateUnit {
    Years,
    Quarters,
    Months,
    Weeks,
    Days,
    Hours,
    Minutes,
    Seconds,
}

impl DateUnit {
    fn parse(value: &str, path: &str) -> PivotResult<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "year" | "years" => Ok(Self::Years),
            "quarter" | "quarters" => Ok(Self::Quarters),
            "month" | "months" => Ok(Self::Months),
            "week" | "weeks" => Ok(Self::Weeks),
            "day" | "days" => Ok(Self::Days),
            "hour" | "hours" => Ok(Self::Hours),
            "minute" | "minutes" => Ok(Self::Minutes),
            "second" | "seconds" => Ok(Self::Seconds),
            other => error(
                "PIVOT_DATE_GROUP",
                path,
                format!("unsupported date grouping unit {other}"),
            ),
        }
    }

    fn suffix(self) -> &'static str {
        match self {
            Self::Years => "Years",
            Self::Quarters => "Quarters",
            Self::Months => "Months",
            Self::Weeks => "Weeks",
            Self::Days => "Days",
            Self::Hours => "Hours",
            Self::Minutes => "Minutes",
            Self::Seconds => "Seconds",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct DateParts {
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
}

#[derive(Clone, Debug)]
struct CacheField {
    name: String,
    source_index: usize,
    date_group: Option<DateUnit>,
}

#[derive(Clone, Debug)]
struct PreparedTable {
    source: SourceTable,
    fields: Vec<CacheField>,
    rows: Vec<Vec<Value>>,
    date_groups: BTreeMap<(usize, DateUnit), usize>,
    warnings: Vec<Value>,
}

#[derive(Clone, Debug)]
struct AxisField {
    index: usize,
    name: String,
    subtotal: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Aggregate {
    Sum,
    Count,
    Average,
    Min,
    Max,
    DistinctCount,
}

impl Aggregate {
    fn parse(value: &str, path: &str) -> PivotResult<Self> {
        match value
            .trim()
            .to_ascii_lowercase()
            .replace(['_', ' '], "")
            .as_str()
        {
            "sum" => Ok(Self::Sum),
            "count" | "counta" => Ok(Self::Count),
            "avg" | "average" => Ok(Self::Average),
            "min" | "minimum" => Ok(Self::Min),
            "max" | "maximum" => Ok(Self::Max),
            "distinctcount" | "distinct" => Ok(Self::DistinctCount),
            other => error(
                "PIVOT_AGGREGATE",
                path,
                format!("unsupported aggregate {other}"),
            ),
        }
    }

    fn caption(self) -> &'static str {
        match self {
            Self::Sum => "Sum",
            Self::Count => "Count",
            Self::Average => "Average",
            Self::Min => "Min",
            Self::Max => "Max",
            Self::DistinctCount => "Distinct Count",
        }
    }

    /// The current native editor exposes the ECMA-376 subtotal enumeration.  Distinct Count is
    /// stored in an x14 extension in real Excel files, so `count` is used only as a structural
    /// fallback in the native patch while `localAggregation` retains the exact local semantics.
    fn native_subtotal(self) -> &'static str {
        match self {
            Self::Sum => "sum",
            Self::Count | Self::DistinctCount => "count",
            Self::Average => "average",
            Self::Min => "min",
            Self::Max => "max",
        }
    }
}

#[derive(Clone, Debug)]
struct ValueField {
    index: usize,
    name: String,
    aggregate: Aggregate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EmptyAxisPolicy {
    Blank,
    Skip,
    Zero,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EmptyValuePolicy {
    Blank,
    Zero,
}

#[derive(Clone, Copy, Debug)]
struct EmptyPolicy {
    axis: EmptyAxisPolicy,
    values: EmptyValuePolicy,
}

impl Default for EmptyPolicy {
    fn default() -> Self {
        Self {
            axis: EmptyAxisPolicy::Blank,
            values: EmptyValuePolicy::Blank,
        }
    }
}

#[derive(Clone, Debug)]
struct SortSpec {
    axis: String,
    level: usize,
    descending: bool,
    by_value: bool,
    value_index: usize,
    custom: Vec<Value>,
}

#[derive(Clone, Debug, Default)]
struct AggregateState {
    sum: f64,
    numeric_count: u64,
    non_blank_count: u64,
    min: Option<Value>,
    max: Option<Value>,
    distinct: BTreeMap<String, Value>,
}

impl AggregateState {
    fn add(&mut self, value: &Value) {
        if is_blank(value) {
            return;
        }
        self.non_blank_count += 1;
        if let Some(number) = numeric_value(value) {
            self.sum += number;
            self.numeric_count += 1;
        }
        if self
            .min
            .as_ref()
            .is_none_or(|current| compare_values(value, current) == Ordering::Less)
        {
            self.min = Some(value.clone());
        }
        if self
            .max
            .as_ref()
            .is_none_or(|current| compare_values(value, current) == Ordering::Greater)
        {
            self.max = Some(value.clone());
        }
        self.distinct
            .entry(canonical_value(value))
            .or_insert_with(|| value.clone());
    }

    fn merge(&mut self, other: &Self) {
        self.sum += other.sum;
        self.numeric_count += other.numeric_count;
        self.non_blank_count += other.non_blank_count;
        if let Some(value) = &other.min {
            if self
                .min
                .as_ref()
                .is_none_or(|current| compare_values(value, current) == Ordering::Less)
            {
                self.min = Some(value.clone());
            }
        }
        if let Some(value) = &other.max {
            if self
                .max
                .as_ref()
                .is_none_or(|current| compare_values(value, current) == Ordering::Greater)
            {
                self.max = Some(value.clone());
            }
        }
        for (key, value) in &other.distinct {
            self.distinct
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }
    }

    fn finish(&self, aggregate: Aggregate, policy: EmptyValuePolicy) -> Value {
        match aggregate {
            Aggregate::Sum => {
                if self.numeric_count == 0 && policy == EmptyValuePolicy::Blank {
                    Value::Null
                } else {
                    finite_number(self.sum)
                }
            }
            Aggregate::Count => Value::from(self.non_blank_count),
            Aggregate::Average => {
                if self.numeric_count == 0 {
                    if policy == EmptyValuePolicy::Zero {
                        Value::from(0)
                    } else {
                        Value::Null
                    }
                } else {
                    finite_number(self.sum / self.numeric_count as f64)
                }
            }
            Aggregate::Min => self.min.clone().unwrap_or_else(|| {
                if policy == EmptyValuePolicy::Zero {
                    Value::from(0)
                } else {
                    Value::Null
                }
            }),
            Aggregate::Max => self.max.clone().unwrap_or_else(|| {
                if policy == EmptyValuePolicy::Zero {
                    Value::from(0)
                } else {
                    Value::Null
                }
            }),
            Aggregate::DistinctCount => Value::from(self.distinct.len() as u64),
        }
    }
}

#[derive(Clone, Debug)]
struct DetailBucket {
    row_key: Vec<Value>,
    column_key: Vec<Value>,
    values: Vec<AggregateState>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EntryKind {
    Detail,
    Subtotal,
    Grand,
}

#[derive(Clone, Debug)]
struct AxisEntry {
    kind: EntryKind,
    key: Vec<Value>,
    prefix_len: usize,
}

fn finite_number(value: f64) -> Value {
    if value.fract() == 0.0 {
        if value >= 0.0 && value <= u64::MAX as f64 {
            return Value::from(value as u64);
        }
        if value >= i64::MIN as f64 && value <= i64::MAX as f64 {
            return Value::from(value as i64);
        }
    }
    Number::from_f64(value)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

fn is_blank(value: &Value) -> bool {
    value.is_null() || value.as_str().is_some_and(str::is_empty)
}

fn numeric_value(value: &Value) -> Option<f64> {
    value.as_f64().or_else(|| {
        value
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .and_then(|text| text.parse::<f64>().ok())
            .filter(|value| value.is_finite())
    })
}

fn canonical_value(value: &Value) -> String {
    match value {
        Value::Null => "z:".to_string(),
        Value::Bool(value) => format!("b:{}", u8::from(*value)),
        Value::Number(value) => format!("n:{value}"),
        Value::String(value) => format!(
            "s:{}",
            serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
        ),
        Value::Array(_) | Value::Object(_) => format!(
            "j:{}",
            serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
        ),
    }
}

fn canonical_tuple(values: &[Value]) -> String {
    values
        .iter()
        .map(canonical_value)
        .collect::<Vec<_>>()
        .join("\u{1f}")
}

fn value_rank(value: &Value) -> u8 {
    match value {
        Value::Null => 0,
        Value::Bool(_) => 1,
        Value::Number(_) => 2,
        Value::String(_) => 3,
        Value::Array(_) => 4,
        Value::Object(_) => 5,
    }
}

fn compare_values(left: &Value, right: &Value) -> Ordering {
    if let (Some(left), Some(right)) = (numeric_value(left), numeric_value(right)) {
        return left.partial_cmp(&right).unwrap_or(Ordering::Equal);
    }
    let rank = value_rank(left).cmp(&value_rank(right));
    if rank != Ordering::Equal {
        return rank;
    }
    match (left, right) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Bool(left), Value::Bool(right)) => left.cmp(right),
        (Value::String(left), Value::String(right)) => left
            .to_lowercase()
            .cmp(&right.to_lowercase())
            .then_with(|| left.cmp(right)),
        _ => canonical_value(left).cmp(&canonical_value(right)),
    }
}

fn compare_tuples(left: &[Value], right: &[Value]) -> Ordering {
    left.iter()
        .zip(right)
        .map(|(left, right)| compare_values(left, right))
        .find(|ordering| *ordering != Ordering::Equal)
        .unwrap_or_else(|| left.len().cmp(&right.len()))
}

fn display_text(value: &Value) -> String {
    match value {
        Value::Null => "(blank)".to_string(),
        Value::Bool(value) => if *value { "TRUE" } else { "FALSE" }.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Array(_) | Value::Object(_) => serde_json::to_string(value).unwrap_or_default(),
    }
}

fn value_at<'a>(object: &'a Map<String, Value>, names: &[&str]) -> Option<&'a Value> {
    names.iter().find_map(|name| object.get(*name))
}

fn parse_field_names(value: &Value, path: &str) -> PivotResult<Vec<String>> {
    let fields = value
        .as_array()
        .ok_or_else(|| PivotRefreshError::new("PIVOT_SOURCE", path, "fields must be an array"))?;
    if fields.is_empty() {
        return error(
            "PIVOT_SOURCE",
            path,
            "at least one source field is required",
        );
    }
    let mut output = Vec::with_capacity(fields.len());
    let mut unique = BTreeSet::new();
    for (index, field) in fields.iter().enumerate() {
        let name = field
            .as_str()
            .map(str::to_string)
            .or_else(|| {
                field
                    .as_object()
                    .and_then(|field| value_at(field, &["name", "label", "field"]))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .ok_or_else(|| {
                PivotRefreshError::new(
                    "PIVOT_SOURCE",
                    format!("{path}/{index}"),
                    "field must be a string or an object with a name",
                )
            })?;
        let name = name.trim().to_string();
        if name.is_empty() {
            return error(
                "PIVOT_SOURCE",
                format!("{path}/{index}"),
                "field name cannot be empty",
            );
        }
        if !unique.insert(name.to_ascii_lowercase()) {
            return error(
                "PIVOT_SOURCE",
                format!("{path}/{index}"),
                format!("duplicate source field {name}"),
            );
        }
        output.push(name);
    }
    Ok(output)
}

fn normalize_rows(value: &Value, fields: &[String], path: &str) -> PivotResult<Vec<Vec<Value>>> {
    let rows = value
        .as_array()
        .ok_or_else(|| PivotRefreshError::new("PIVOT_SOURCE", path, "rows must be an array"))?;
    let mut output = Vec::with_capacity(rows.len());
    for (row_index, row) in rows.iter().enumerate() {
        if let Some(values) = row.as_array() {
            if values.len() > fields.len() {
                return error(
                    "PIVOT_SOURCE",
                    format!("{path}/{row_index}"),
                    format!(
                        "row has {} cells but the source has {} fields",
                        values.len(),
                        fields.len()
                    ),
                );
            }
            let mut normalized = values.clone();
            normalized.resize(fields.len(), Value::Null);
            output.push(normalized);
        } else if let Some(object) = row.as_object() {
            output.push(
                fields
                    .iter()
                    .map(|field| object.get(field).cloned().unwrap_or(Value::Null))
                    .collect(),
            );
        } else {
            return error(
                "PIVOT_SOURCE",
                format!("{path}/{row_index}"),
                "each row must be an array or an object keyed by field name",
            );
        }
    }
    Ok(output)
}

fn parse_range(value: Option<&Value>) -> PivotResult<Option<(i64, i64, i64, i64)>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if let Some(range) = value.as_array() {
        if range.len() != 4 {
            return error(
                "PIVOT_SOURCE",
                "/source/range",
                "range array must be [firstRow, firstColumn, lastRow, lastColumn]",
            );
        }
        let numbers: Option<Vec<i64>> = range.iter().map(Value::as_i64).collect();
        let numbers = numbers.ok_or_else(|| {
            PivotRefreshError::new(
                "PIVOT_SOURCE",
                "/source/range",
                "range coordinates must be integers",
            )
        })?;
        return Ok(Some((numbers[0], numbers[1], numbers[2], numbers[3])));
    }
    let object = value.as_object().ok_or_else(|| {
        PivotRefreshError::new(
            "PIVOT_SOURCE",
            "/source/range",
            "range must be an array or object",
        )
    })?;
    let coordinate = |names: &[&str]| {
        value_at(object, names)
            .and_then(Value::as_i64)
            .ok_or_else(|| {
                PivotRefreshError::new(
                    "PIVOT_SOURCE",
                    "/source/range",
                    format!("missing integer coordinate {}", names[0]),
                )
            })
    };
    Ok(Some((
        coordinate(&["r0", "minRow", "firstRow"])?,
        coordinate(&["c0", "minCol", "firstColumn"])?,
        coordinate(&["r1", "maxRow", "lastRow"])?,
        coordinate(&["c1", "maxCol", "lastColumn"])?,
    )))
}

fn cell_coordinate(cell: &Map<String, Value>, names: &[&str], path: &str) -> PivotResult<i64> {
    value_at(cell, names)
        .and_then(Value::as_i64)
        .ok_or_else(|| PivotRefreshError::new("PIVOT_SOURCE", path, "cell coordinate is missing"))
}

fn parse_snapshot_source(
    source: &Map<String, Value>,
    source_path: &str,
) -> PivotResult<SourceTable> {
    let cells = source
        .get("cells")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            PivotRefreshError::new(
                "PIVOT_SOURCE",
                format!("{source_path}/cells"),
                "worksheet snapshot cells must be an array",
            )
        })?;
    let mut sparse = BTreeMap::<(i64, i64), Value>::new();
    for (index, cell) in cells.iter().enumerate() {
        let cell = cell.as_object().ok_or_else(|| {
            PivotRefreshError::new(
                "PIVOT_SOURCE",
                format!("{source_path}/cells/{index}"),
                "snapshot cell must be an object",
            )
        })?;
        let row = cell_coordinate(
            cell,
            &["r", "row"],
            &format!("{source_path}/cells/{index}/r"),
        )?;
        let column = cell_coordinate(
            cell,
            &["c", "col", "column"],
            &format!("{source_path}/cells/{index}/c"),
        )?;
        let value = value_at(cell, &["raw", "value", "v", "f"])
            .cloned()
            .unwrap_or(Value::Null);
        if sparse.insert((row, column), value).is_some() {
            return error(
                "PIVOT_SOURCE",
                format!("{source_path}/cells/{index}"),
                format!("duplicate snapshot cell at row {row}, column {column}"),
            );
        }
    }
    let inferred = sparse
        .keys()
        .fold(None::<(i64, i64, i64, i64)>, |range, (row, column)| {
            Some(match range {
                None => (*row, *column, *row, *column),
                Some((r0, c0, r1, c1)) => {
                    (r0.min(*row), c0.min(*column), r1.max(*row), c1.max(*column))
                }
            })
        });
    let (r0, c0, r1, c1) = parse_range(source.get("range"))?
        .or(inferred)
        .ok_or_else(|| {
            PivotRefreshError::new(
                "PIVOT_SOURCE",
                format!("{source_path}/cells"),
                "worksheet snapshot is empty and has no explicit range",
            )
        })?;
    if r0 > r1 || c0 > c1 {
        return error(
            "PIVOT_SOURCE",
            format!("{source_path}/range"),
            "range start must not be after range end",
        );
    }
    let header_row = source
        .get("headerRow")
        .and_then(Value::as_i64)
        .unwrap_or(r0);
    let fields = if let Some(fields) = source.get("fields") {
        parse_field_names(fields, &format!("{source_path}/fields"))?
    } else {
        (c0..=c1)
            .map(|column| {
                sparse
                    .get(&(header_row, column))
                    .map(display_text)
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_else(|| format!("Column{}", column - c0 + 1))
            })
            .collect()
    };
    if fields.len() != usize::try_from(c1 - c0 + 1).unwrap_or(usize::MAX) {
        return error(
            "PIVOT_SOURCE",
            format!("{source_path}/fields"),
            "explicit field count must equal the worksheet range width",
        );
    }
    let first_data_row = source
        .get("firstDataRow")
        .and_then(Value::as_i64)
        .unwrap_or(header_row + 1);
    let rows = (first_data_row..=r1)
        .map(|row| {
            (c0..=c1)
                .map(|column| sparse.get(&(row, column)).cloned().unwrap_or(Value::Null))
                .collect::<Vec<_>>()
        })
        .collect();
    Ok(SourceTable {
        fields,
        rows,
        date_1904: source
            .get("date1904")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn parse_source(input: &Value) -> PivotResult<SourceTable> {
    let root = input.as_object().ok_or_else(|| {
        PivotRefreshError::new(
            "PIVOT_REQUEST",
            "/",
            "pivot refresh input must be an object",
        )
    })?;
    let (source_value, source_path) = if let Some(source) = root.get("source") {
        (source, "/source")
    } else if let Some(snapshot) = root.get("worksheet").or_else(|| root.get("snapshot")) {
        (snapshot, "/worksheet")
    } else {
        (input, "")
    };
    let source = source_value.as_object().ok_or_else(|| {
        PivotRefreshError::new("PIVOT_SOURCE", source_path, "source must be an object")
    })?;
    if let Some(kind) = value_at(source, &["kind", "type", "sourceType"]).and_then(Value::as_str) {
        let kind = kind.to_ascii_lowercase();
        if ["olap", "cube", "datamodel", "data model", "vertipaq", "mdx"]
            .iter()
            .any(|needle| kind.contains(needle))
        {
            return error(
                "PIVOT_OLAP_UNSUPPORTED",
                format!("{source_path}/kind"),
                "local refresh supports worksheet caches only; OLAP/Data Model requires its native provider, MDX/DAX engine, and credentials",
            );
        }
    }
    if source.contains_key("cells") {
        return parse_snapshot_source(source, source_path);
    }
    let fields = parse_field_names(
        source.get("fields").ok_or_else(|| {
            PivotRefreshError::new(
                "PIVOT_SOURCE",
                format!("{source_path}/fields"),
                "source fields are required",
            )
        })?,
        &format!("{source_path}/fields"),
    )?;
    let rows = normalize_rows(
        source.get("rows").ok_or_else(|| {
            PivotRefreshError::new(
                "PIVOT_SOURCE",
                format!("{source_path}/rows"),
                "source rows are required",
            )
        })?,
        &fields,
        &format!("{source_path}/rows"),
    )?;
    Ok(SourceTable {
        fields,
        rows,
        date_1904: source
            .get("date1904")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let year = year - i32::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = month as i32;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day as i32 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    (era * 146097 + day_of_era - 719468) as i64
}

fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let days = days + 719468;
    let era = if days >= 0 { days } else { days - 146096 } / 146097;
    let day_of_era = days - era * 146097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    (
        (year + i64::from(month <= 2)) as i32,
        month as u32,
        day as u32,
    )
}

fn valid_date(year: i32, month: u32, day: u32) -> bool {
    if !(1..=12).contains(&month) || day == 0 {
        return false;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let max = match month {
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    day <= max
}

fn parse_date(value: &Value, date_1904: bool) -> Option<DateParts> {
    if let Some(serial) = value.as_f64().filter(|value| value.is_finite()) {
        let days = serial.floor() as i64;
        let fraction = (serial - serial.floor()).max(0.0);
        let base = if date_1904 {
            days_from_civil(1904, 1, 1)
        } else {
            days_from_civil(1899, 12, 30)
        };
        let (year, month, day) = civil_from_days(base + days);
        let seconds = (fraction * 86_400.0).round().clamp(0.0, 86_399.0) as u32;
        return Some(DateParts {
            year,
            month,
            day,
            hour: seconds / 3600,
            minute: seconds % 3600 / 60,
            second: seconds % 60,
        });
    }
    let text = value.as_str()?.trim();
    let date = text.get(0..10)?;
    let bytes = date.as_bytes();
    if bytes.get(4) != Some(&b'-') || bytes.get(7) != Some(&b'-') {
        return None;
    }
    let year = date.get(0..4)?.parse::<i32>().ok()?;
    let month = date.get(5..7)?.parse::<u32>().ok()?;
    let day = date.get(8..10)?.parse::<u32>().ok()?;
    if !valid_date(year, month, day) {
        return None;
    }
    let time = text
        .get(10..)
        .map(|tail| tail.trim_start_matches(['T', ' ']))
        .unwrap_or("");
    let mut parts = time.split(':');
    let hour = parts
        .next()
        .filter(|value| !value.is_empty())
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let minute = parts
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let second = parts
        .next()
        .and_then(|value| value.trim_end_matches('Z').split('.').next())
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    (hour < 24 && minute < 60 && second < 60).then_some(DateParts {
        year,
        month,
        day,
        hour,
        minute,
        second,
    })
}

fn iso_week(date: DateParts) -> (i32, u32) {
    // Monday=1 ... Sunday=7; 1970-01-01 was Thursday.
    let days = days_from_civil(date.year, date.month, date.day);
    let weekday = (days + 3).rem_euclid(7) + 1;
    let thursday = days + (4 - weekday);
    let (week_year, _, _) = civil_from_days(thursday);
    let first_thursday = {
        let january_fourth = days_from_civil(week_year, 1, 4);
        let weekday = (january_fourth + 3).rem_euclid(7) + 1;
        january_fourth + (4 - weekday)
    };
    (week_year, ((thursday - first_thursday) / 7 + 1) as u32)
}

fn grouped_date_value(date: DateParts, unit: DateUnit) -> Value {
    match unit {
        DateUnit::Years => Value::from(date.year),
        DateUnit::Quarters => Value::from((date.month - 1) / 3 + 1),
        DateUnit::Months => Value::from(date.month),
        DateUnit::Weeks => {
            let (year, week) = iso_week(date);
            Value::String(format!("{year:04}-W{week:02}"))
        }
        DateUnit::Days => Value::from(date.day),
        DateUnit::Hours => Value::from(date.hour),
        DateUnit::Minutes => Value::from(date.minute),
        DateUnit::Seconds => Value::from(date.second),
    }
}

fn resolve_source_field(fields: &[String], value: &Value, path: &str) -> PivotResult<usize> {
    let value = value
        .as_object()
        .and_then(|object| value_at(object, &["field", "name", "index"]))
        .unwrap_or(value);
    if let Some(index) = value.as_u64().and_then(|value| usize::try_from(value).ok()) {
        if index < fields.len() {
            return Ok(index);
        }
        return error(
            "PIVOT_FIELD",
            path,
            format!(
                "field index {index} is outside {} source fields",
                fields.len()
            ),
        );
    }
    let name = value.as_str().ok_or_else(|| {
        PivotRefreshError::new(
            "PIVOT_FIELD",
            path,
            "field reference must be a name, index, or object with field/name/index",
        )
    })?;
    if let Some(index) = fields.iter().position(|field| field == name) {
        return Ok(index);
    }
    let mut matches = fields
        .iter()
        .enumerate()
        .filter(|(_, field)| field.eq_ignore_ascii_case(name));
    let first = matches.next().map(|(index, _)| index);
    if first.is_some() && matches.next().is_none() {
        return Ok(first.unwrap());
    }
    error("PIVOT_FIELD", path, format!("unknown source field {name}"))
}

fn collect_inline_date_groups(
    input: &Map<String, Value>,
    source_fields: &[String],
    groups: &mut BTreeSet<(usize, DateUnit)>,
) -> PivotResult<()> {
    for collection_name in ["rows", "columns", "pages"] {
        let Some(collection) = input.get(collection_name) else {
            continue;
        };
        let collection = collection.as_array().ok_or_else(|| {
            PivotRefreshError::new(
                "PIVOT_LAYOUT",
                format!("/{collection_name}"),
                format!("{collection_name} must be an array"),
            )
        })?;
        for (index, field) in collection.iter().enumerate() {
            let Some(object) = field.as_object() else {
                continue;
            };
            let Some(group) = value_at(object, &["group", "dateGroup"]).and_then(Value::as_str)
            else {
                continue;
            };
            let source = resolve_source_field(
                source_fields,
                object
                    .get("field")
                    .or_else(|| object.get("name"))
                    .or_else(|| object.get("index"))
                    .ok_or_else(|| {
                        PivotRefreshError::new(
                            "PIVOT_FIELD",
                            format!("/{collection_name}/{index}"),
                            "grouped axis field requires field/name/index",
                        )
                    })?,
                &format!("/{collection_name}/{index}/field"),
            )?;
            groups.insert((
                source,
                DateUnit::parse(group, &format!("/{collection_name}/{index}/group"))?,
            ));
        }
    }
    Ok(())
}

fn prepare_table(input: &Map<String, Value>, source: SourceTable) -> PivotResult<PreparedTable> {
    let mut requested = BTreeSet::new();
    if let Some(groups) = input.get("dateGroups") {
        let groups = groups.as_array().ok_or_else(|| {
            PivotRefreshError::new(
                "PIVOT_DATE_GROUP",
                "/dateGroups",
                "dateGroups must be an array",
            )
        })?;
        for (index, group) in groups.iter().enumerate() {
            let group = group.as_object().ok_or_else(|| {
                PivotRefreshError::new(
                    "PIVOT_DATE_GROUP",
                    format!("/dateGroups/{index}"),
                    "date group must be an object",
                )
            })?;
            let source_index = resolve_source_field(
                &source.fields,
                group.get("field").ok_or_else(|| {
                    PivotRefreshError::new(
                        "PIVOT_DATE_GROUP",
                        format!("/dateGroups/{index}/field"),
                        "date group requires a source field",
                    )
                })?,
                &format!("/dateGroups/{index}/field"),
            )?;
            let units = value_at(group, &["by", "groups", "unit"]).ok_or_else(|| {
                PivotRefreshError::new(
                    "PIVOT_DATE_GROUP",
                    format!("/dateGroups/{index}/by"),
                    "date group requires by/unit",
                )
            })?;
            if let Some(units) = units.as_array() {
                for (unit_index, unit) in units.iter().enumerate() {
                    requested.insert((
                        source_index,
                        DateUnit::parse(
                            unit.as_str().ok_or_else(|| {
                                PivotRefreshError::new(
                                    "PIVOT_DATE_GROUP",
                                    format!("/dateGroups/{index}/by/{unit_index}"),
                                    "date grouping unit must be a string",
                                )
                            })?,
                            &format!("/dateGroups/{index}/by/{unit_index}"),
                        )?,
                    ));
                }
            } else {
                requested.insert((
                    source_index,
                    DateUnit::parse(
                        units.as_str().ok_or_else(|| {
                            PivotRefreshError::new(
                                "PIVOT_DATE_GROUP",
                                format!("/dateGroups/{index}/by"),
                                "date grouping unit must be a string or array",
                            )
                        })?,
                        &format!("/dateGroups/{index}/by"),
                    )?,
                ));
            }
        }
    }
    collect_inline_date_groups(input, &source.fields, &mut requested)?;

    let mut fields: Vec<CacheField> = source
        .fields
        .iter()
        .enumerate()
        .map(|(index, name)| CacheField {
            name: name.clone(),
            source_index: index,
            date_group: None,
        })
        .collect();
    let mut date_groups = BTreeMap::new();
    for (source_index, unit) in requested {
        let field_index = fields.len();
        fields.push(CacheField {
            name: format!("{} ({})", source.fields[source_index], unit.suffix()),
            source_index,
            date_group: Some(unit),
        });
        date_groups.insert((source_index, unit), field_index);
    }
    let mut rows = Vec::with_capacity(source.rows.len());
    let mut invalid_counts = BTreeMap::<(usize, DateUnit), usize>::new();
    for source_row in &source.rows {
        let mut row = source_row.clone();
        for field in fields.iter().skip(source.fields.len()) {
            let value = source_row
                .get(field.source_index)
                .and_then(|value| parse_date(value, source.date_1904))
                .map(|date| grouped_date_value(date, field.date_group.unwrap()))
                .unwrap_or_else(|| {
                    *invalid_counts
                        .entry((field.source_index, field.date_group.unwrap()))
                        .or_default() += 1;
                    Value::Null
                });
            row.push(value);
        }
        rows.push(row);
    }
    let warnings = invalid_counts
        .into_iter()
        .filter(|(_, count)| *count > 0)
        .map(|((field, unit), count)| {
            json!({
                "code":"PIVOT_DATE_BLANK",
                "field":source.fields[field],
                "group":unit.suffix(),
                "count":count,
                "message":"blank or invalid dates were grouped as (blank)"
            })
        })
        .collect();
    Ok(PreparedTable {
        source,
        fields,
        rows,
        date_groups,
        warnings,
    })
}

fn resolve_prepared_field(table: &PreparedTable, value: &Value, path: &str) -> PivotResult<usize> {
    if let Some(object) = value.as_object() {
        if let Some(group) = value_at(object, &["group", "dateGroup"]).and_then(Value::as_str) {
            let base = resolve_source_field(
                &table.source.fields,
                object
                    .get("field")
                    .or_else(|| object.get("name"))
                    .or_else(|| object.get("index"))
                    .ok_or_else(|| {
                        PivotRefreshError::new(
                            "PIVOT_FIELD",
                            path,
                            "grouped field requires field/name/index",
                        )
                    })?,
                path,
            )?;
            let unit = DateUnit::parse(group, path)?;
            return table
                .date_groups
                .get(&(base, unit))
                .copied()
                .ok_or_else(|| {
                    PivotRefreshError::new(
                        "PIVOT_DATE_GROUP",
                        path,
                        "date grouping field was not prepared",
                    )
                });
        }
    }
    if let Some(index) = value.as_u64().and_then(|value| usize::try_from(value).ok()) {
        if index < table.fields.len() {
            return Ok(index);
        }
        return error(
            "PIVOT_FIELD",
            path,
            format!(
                "field index {index} is outside {} cache fields",
                table.fields.len()
            ),
        );
    }
    let reference = value
        .as_object()
        .and_then(|object| value_at(object, &["field", "name", "index"]))
        .unwrap_or(value);
    if let Some(name) = reference.as_str() {
        if let Some(index) = table.fields.iter().position(|field| field.name == name) {
            return Ok(index);
        }
        if let Some((base_name, suffix)) = name.rsplit_once('.') {
            if let Some(base) = table
                .source
                .fields
                .iter()
                .position(|field| field.eq_ignore_ascii_case(base_name))
            {
                if let Ok(unit) = DateUnit::parse(suffix, path) {
                    if let Some(index) = table.date_groups.get(&(base, unit)) {
                        return Ok(*index);
                    }
                }
            }
        }
    }
    resolve_source_field(&table.source.fields, reference, path)
}

fn parse_axis(
    input: &Map<String, Value>,
    table: &PreparedTable,
    name: &str,
) -> PivotResult<Vec<AxisField>> {
    let Some(value) = input.get(name) else {
        return Ok(Vec::new());
    };
    let values = value.as_array().ok_or_else(|| {
        PivotRefreshError::new(
            "PIVOT_LAYOUT",
            format!("/{name}"),
            format!("{name} must be an array"),
        )
    })?;
    let mut output = Vec::with_capacity(values.len());
    let mut seen = BTreeSet::new();
    for (position, value) in values.iter().enumerate() {
        let index = resolve_prepared_field(table, value, &format!("/{name}/{position}"))?;
        if !seen.insert(index) {
            return error(
                "PIVOT_LAYOUT",
                format!("/{name}/{position}"),
                format!(
                    "field {} appears twice on the {name} axis",
                    table.fields[index].name
                ),
            );
        }
        output.push(AxisField {
            index,
            name: value
                .as_object()
                .and_then(|value| value.get("caption"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| table.fields[index].name.clone()),
            subtotal: value
                .as_object()
                .and_then(|value| value.get("subtotal"))
                .and_then(Value::as_bool)
                .unwrap_or(true),
        });
    }
    Ok(output)
}

fn parse_value_fields(
    input: &Map<String, Value>,
    table: &PreparedTable,
) -> PivotResult<Vec<ValueField>> {
    let values = input
        .get("values")
        .or_else(|| input.get("data"))
        .ok_or_else(|| {
            PivotRefreshError::new(
                "PIVOT_LAYOUT",
                "/values",
                "at least one value field is required",
            )
        })?
        .as_array()
        .ok_or_else(|| {
            PivotRefreshError::new("PIVOT_LAYOUT", "/values", "values must be an array")
        })?;
    if values.is_empty() {
        return error(
            "PIVOT_LAYOUT",
            "/values",
            "at least one value field is required",
        );
    }
    let mut output = Vec::with_capacity(values.len());
    for (position, value) in values.iter().enumerate() {
        let object = value.as_object();
        let index = resolve_prepared_field(table, value, &format!("/values/{position}/field"))?;
        let aggregate = Aggregate::parse(
            object
                .and_then(|value| value_at(value, &["aggregate", "summary", "subtotal"]))
                .and_then(Value::as_str)
                .unwrap_or("sum"),
            &format!("/values/{position}/aggregate"),
        )?;
        let name = object
            .and_then(|value| value_at(value, &["caption", "name"]))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("{} of {}", aggregate.caption(), table.fields[index].name));
        output.push(ValueField {
            index,
            name,
            aggregate,
        });
    }
    Ok(output)
}

fn parse_empty_policy(input: &Map<String, Value>) -> PivotResult<EmptyPolicy> {
    let Some(value) = input.get("emptyPolicy") else {
        return Ok(EmptyPolicy::default());
    };
    let axis = value
        .as_object()
        .and_then(|object| object.get("axis"))
        .unwrap_or(value)
        .as_str()
        .unwrap_or("blank")
        .to_ascii_lowercase();
    let values = value
        .as_object()
        .and_then(|object| object.get("values"))
        .and_then(Value::as_str)
        .unwrap_or("blank")
        .to_ascii_lowercase();
    Ok(EmptyPolicy {
        axis: match axis.as_str() {
            "blank" | "include" => EmptyAxisPolicy::Blank,
            "skip" | "exclude" => EmptyAxisPolicy::Skip,
            "zero" => EmptyAxisPolicy::Zero,
            _ => {
                return error(
                    "PIVOT_EMPTY_POLICY",
                    "/emptyPolicy/axis",
                    "axis policy must be blank, skip, or zero",
                );
            }
        },
        values: match values.as_str() {
            "blank" | "skip" => EmptyValuePolicy::Blank,
            "zero" => EmptyValuePolicy::Zero,
            _ => {
                return error(
                    "PIVOT_EMPTY_POLICY",
                    "/emptyPolicy/values",
                    "value policy must be blank or zero",
                );
            }
        },
    })
}

fn equal_values(left: &Value, right: &Value) -> bool {
    compare_values(left, right) == Ordering::Equal
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
    let pattern: Vec<char> = pattern.to_lowercase().chars().collect();
    let value: Vec<char> = value.to_lowercase().chars().collect();
    let mut previous = vec![false; value.len() + 1];
    previous[0] = true;
    for token in pattern {
        let mut next = vec![false; value.len() + 1];
        if token == '*' {
            next[0] = previous[0];
        }
        for index in 1..=value.len() {
            next[index] = match token {
                '*' => previous[index] || next[index - 1],
                '?' => previous[index - 1],
                literal => previous[index - 1] && literal == value[index - 1],
            };
        }
        previous = next;
    }
    previous[value.len()]
}

fn filter_matches(value: &Value, filter: &Map<String, Value>, path: &str) -> PivotResult<bool> {
    let op = value_at(filter, &["op", "operator", "type"])
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            if filter.contains_key("values") {
                "in"
            } else {
                "eq"
            }
        })
        .to_ascii_lowercase()
        .replace(['_', ' '], "");
    let expected = value_at(filter, &["value", "criterion"])
        .cloned()
        .unwrap_or(Value::Null);
    let result = match op.as_str() {
        "eq" | "equals" => equal_values(value, &expected),
        "ne" | "notequals" => !equal_values(value, &expected),
        "gt" | "greaterthan" => compare_values(value, &expected) == Ordering::Greater,
        "gte" | "greaterthanorequal" => compare_values(value, &expected) != Ordering::Less,
        "lt" | "lessthan" => compare_values(value, &expected) == Ordering::Less,
        "lte" | "lessthanorequal" => compare_values(value, &expected) != Ordering::Greater,
        "between" => {
            let lower = value_at(filter, &["min", "from", "value"])
                .cloned()
                .unwrap_or(Value::Null);
            let upper = value_at(filter, &["max", "to", "value2"])
                .cloned()
                .unwrap_or(Value::Null);
            compare_values(value, &lower) != Ordering::Less
                && compare_values(value, &upper) != Ordering::Greater
        }
        "in" | "include" => value_at(filter, &["values", "include"])
            .and_then(Value::as_array)
            .ok_or_else(|| {
                PivotRefreshError::new("PIVOT_FILTER", path, "in filter requires values array")
            })?
            .iter()
            .any(|candidate| equal_values(value, candidate)),
        "notin" | "exclude" => !value_at(filter, &["values", "exclude"])
            .and_then(Value::as_array)
            .ok_or_else(|| {
                PivotRefreshError::new("PIVOT_FILTER", path, "notIn filter requires values array")
            })?
            .iter()
            .any(|candidate| equal_values(value, candidate)),
        "contains" => display_text(value)
            .to_lowercase()
            .contains(&display_text(&expected).to_lowercase()),
        "beginswith" | "startswith" => display_text(value)
            .to_lowercase()
            .starts_with(&display_text(&expected).to_lowercase()),
        "endswith" => display_text(value)
            .to_lowercase()
            .ends_with(&display_text(&expected).to_lowercase()),
        "wildcard" => wildcard_match(&display_text(&expected), &display_text(value)),
        "blank" | "isblank" => is_blank(value),
        "notblank" => !is_blank(value),
        other => {
            return error(
                "PIVOT_FILTER",
                path,
                format!("unsupported filter operator {other}"),
            );
        }
    };
    Ok(result)
}

fn apply_filters(
    input: &Map<String, Value>,
    table: &PreparedTable,
) -> PivotResult<Vec<Vec<Value>>> {
    let Some(filters) = input.get("filters") else {
        return Ok(table.rows.clone());
    };
    let filters = filters.as_array().ok_or_else(|| {
        PivotRefreshError::new("PIVOT_FILTER", "/filters", "filters must be an array")
    })?;
    let mut parsed = Vec::with_capacity(filters.len());
    for (position, filter) in filters.iter().enumerate() {
        let object = filter.as_object().ok_or_else(|| {
            PivotRefreshError::new(
                "PIVOT_FILTER",
                format!("/filters/{position}"),
                "filter must be an object",
            )
        })?;
        let index = resolve_prepared_field(table, filter, &format!("/filters/{position}/field"))?;
        parsed.push((index, object, format!("/filters/{position}")));
    }
    let mut output = Vec::new();
    'rows: for row in &table.rows {
        for (index, filter, path) in &parsed {
            if !filter_matches(row.get(*index).unwrap_or(&Value::Null), filter, path)? {
                continue 'rows;
            }
        }
        output.push(row.clone());
    }
    Ok(output)
}

fn parse_sort_specs(
    input: &Map<String, Value>,
    rows: &[AxisField],
    columns: &[AxisField],
    values: &[ValueField],
) -> PivotResult<Vec<SortSpec>> {
    let Some(sort) = input.get("sort") else {
        return Ok(Vec::new());
    };
    let owned;
    let sort = if let Some(array) = sort.as_array() {
        array
    } else if sort.is_object() {
        owned = vec![sort.clone()];
        &owned
    } else {
        return error("PIVOT_SORT", "/sort", "sort must be an object or array");
    };
    let mut output = Vec::new();
    for (position, item) in sort.iter().enumerate() {
        let object = item.as_object().ok_or_else(|| {
            PivotRefreshError::new(
                "PIVOT_SORT",
                format!("/sort/{position}"),
                "sort entry must be an object",
            )
        })?;
        let axis = object
            .get("axis")
            .and_then(Value::as_str)
            .unwrap_or("row")
            .to_ascii_lowercase();
        let axis_fields = match axis.as_str() {
            "row" | "rows" => rows,
            "column" | "columns" | "col" => columns,
            _ => {
                return error(
                    "PIVOT_SORT",
                    format!("/sort/{position}/axis"),
                    "sort axis must be row or column",
                );
            }
        };
        if axis_fields.is_empty() {
            continue;
        }
        let level = if let Some(level) = object.get("level").and_then(Value::as_u64) {
            usize::try_from(level).unwrap_or(usize::MAX)
        } else if let Some(field) = object.get("field") {
            let name = field.as_str();
            axis_fields
                .iter()
                .position(|axis| {
                    name.is_some_and(|name| axis.name.eq_ignore_ascii_case(name))
                        || field.as_u64() == Some(axis.index as u64)
                })
                .ok_or_else(|| {
                    PivotRefreshError::new(
                        "PIVOT_SORT",
                        format!("/sort/{position}/field"),
                        "sort field is not present on the requested axis",
                    )
                })?
        } else {
            0
        };
        if level >= axis_fields.len() {
            return error(
                "PIVOT_SORT",
                format!("/sort/{position}/level"),
                format!(
                    "sort level {level} is outside {} axis fields",
                    axis_fields.len()
                ),
            );
        }
        let by_value = object
            .get("by")
            .and_then(Value::as_str)
            .is_some_and(|value| value.eq_ignore_ascii_case("value"));
        let value_index = if let Some(field) = value_at(object, &["valueField", "measure"]) {
            if let Some(index) = field.as_u64().and_then(|value| usize::try_from(value).ok()) {
                index
            } else if let Some(name) = field.as_str() {
                values
                    .iter()
                    .position(|value| value.name.eq_ignore_ascii_case(name))
                    .ok_or_else(|| {
                        PivotRefreshError::new(
                            "PIVOT_SORT",
                            format!("/sort/{position}/valueField"),
                            format!("unknown value field {name}"),
                        )
                    })?
            } else {
                usize::MAX
            }
        } else {
            0
        };
        if value_index >= values.len() {
            return error(
                "PIVOT_SORT",
                format!("/sort/{position}/valueField"),
                "value sort refers to an unavailable measure",
            );
        }
        let order = value_at(object, &["order", "direction"])
            .and_then(Value::as_str)
            .unwrap_or("asc");
        let descending = match order.to_ascii_lowercase().as_str() {
            "asc" | "ascending" => false,
            "desc" | "descending" => true,
            _ => {
                return error(
                    "PIVOT_SORT",
                    format!("/sort/{position}/order"),
                    "sort order must be ascending or descending",
                );
            }
        };
        output.push(SortSpec {
            axis: if axis.starts_with("col") {
                "column".to_string()
            } else {
                "row".to_string()
            },
            level,
            descending,
            by_value,
            value_index,
            custom: object
                .get("customList")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        });
    }
    Ok(output)
}

fn normalize_axis_key(mut value: Value, policy: EmptyAxisPolicy) -> Option<Value> {
    if !is_blank(&value) {
        return Some(value);
    }
    match policy {
        EmptyAxisPolicy::Blank => Some(Value::Null),
        EmptyAxisPolicy::Skip => None,
        EmptyAxisPolicy::Zero => {
            value = Value::from(0);
            Some(value)
        }
    }
}

fn aggregate_details(
    source_rows: &[Vec<Value>],
    rows: &[AxisField],
    columns: &[AxisField],
    values: &[ValueField],
    empty: EmptyPolicy,
) -> Vec<DetailBucket> {
    let mut buckets = BTreeMap::<String, DetailBucket>::new();
    'source: for source in source_rows {
        let mut row_key = Vec::with_capacity(rows.len());
        for field in rows {
            let Some(value) = normalize_axis_key(
                source.get(field.index).cloned().unwrap_or(Value::Null),
                empty.axis,
            ) else {
                continue 'source;
            };
            row_key.push(value);
        }
        let mut column_key = Vec::with_capacity(columns.len());
        for field in columns {
            let Some(value) = normalize_axis_key(
                source.get(field.index).cloned().unwrap_or(Value::Null),
                empty.axis,
            ) else {
                continue 'source;
            };
            column_key.push(value);
        }
        let key = format!(
            "{}\u{1e}{}",
            canonical_tuple(&row_key),
            canonical_tuple(&column_key)
        );
        let bucket = buckets.entry(key).or_insert_with(|| DetailBucket {
            row_key,
            column_key,
            values: vec![AggregateState::default(); values.len()],
        });
        for (index, field) in values.iter().enumerate() {
            bucket.values[index].add(source.get(field.index).unwrap_or(&Value::Null));
        }
    }
    buckets.into_values().collect()
}

fn total_for_key(
    details: &[DetailBucket],
    axis: &str,
    key: &[Value],
    value_index: usize,
) -> AggregateState {
    let mut result = AggregateState::default();
    for detail in details {
        let candidate = if axis == "column" {
            &detail.column_key
        } else {
            &detail.row_key
        };
        if candidate == key {
            result.merge(&detail.values[value_index]);
        }
    }
    result
}

fn sort_axis_keys(
    keys: &mut [Vec<Value>],
    axis: &str,
    sorts: &[SortSpec],
    details: &[DetailBucket],
    values: &[ValueField],
    empty: EmptyPolicy,
) {
    keys.sort_by(|left, right| {
        for sort in sorts.iter().filter(|sort| sort.axis == axis) {
            let mut ordering = if sort.by_value {
                let left_total = total_for_key(details, axis, left, sort.value_index)
                    .finish(values[sort.value_index].aggregate, empty.values);
                let right_total = total_for_key(details, axis, right, sort.value_index)
                    .finish(values[sort.value_index].aggregate, empty.values);
                compare_values(&left_total, &right_total)
            } else if !sort.custom.is_empty() {
                let rank = |value: &Value| {
                    sort.custom
                        .iter()
                        .position(|candidate| equal_values(value, candidate))
                        .unwrap_or(sort.custom.len())
                };
                rank(&left[sort.level])
                    .cmp(&rank(&right[sort.level]))
                    .then_with(|| compare_values(&left[sort.level], &right[sort.level]))
            } else {
                compare_values(&left[sort.level], &right[sort.level])
            };
            if sort.descending {
                ordering = ordering.reverse();
            }
            if ordering != Ordering::Equal {
                return ordering;
            }
        }
        compare_tuples(left, right)
    });
}

fn unique_axis_keys(details: &[DetailBucket], row_axis: bool) -> Vec<Vec<Value>> {
    let mut unique = BTreeMap::new();
    for detail in details {
        let key = if row_axis {
            &detail.row_key
        } else {
            &detail.column_key
        };
        unique
            .entry(canonical_tuple(key))
            .or_insert_with(|| key.clone());
    }
    let mut result: Vec<_> = unique.into_values().collect();
    result.sort_by(|left, right| compare_tuples(left, right));
    if result.is_empty() {
        result.push(Vec::new());
    }
    result
}

fn prefix_equal(left: &[Value], right: &[Value], length: usize) -> bool {
    left.get(..length) == right.get(..length)
}

fn build_axis_entries(
    keys: &[Vec<Value>],
    fields: &[AxisField],
    include_subtotals: bool,
    include_grand: bool,
) -> Vec<AxisEntry> {
    if fields.is_empty() {
        return vec![AxisEntry {
            kind: EntryKind::Detail,
            key: Vec::new(),
            prefix_len: 0,
        }];
    }
    let mut entries = Vec::new();
    for (index, key) in keys.iter().enumerate() {
        entries.push(AxisEntry {
            kind: EntryKind::Detail,
            key: key.clone(),
            prefix_len: fields.len(),
        });
        if include_subtotals {
            let next = keys.get(index + 1);
            for prefix_len in (1..fields.len()).rev() {
                if !fields[prefix_len - 1].subtotal {
                    continue;
                }
                if next.is_none_or(|next| !prefix_equal(key, next, prefix_len)) {
                    entries.push(AxisEntry {
                        kind: EntryKind::Subtotal,
                        key: key[..prefix_len].to_vec(),
                        prefix_len,
                    });
                }
            }
        }
    }
    if include_grand {
        entries.push(AxisEntry {
            kind: EntryKind::Grand,
            key: Vec::new(),
            prefix_len: 0,
        });
    }
    entries
}

fn entry_matches(entry: &AxisEntry, key: &[Value]) -> bool {
    match entry.kind {
        EntryKind::Grand => true,
        EntryKind::Detail => entry.key == key,
        EntryKind::Subtotal => prefix_equal(&entry.key, key, entry.prefix_len),
    }
}

fn combine_entries(
    details: &[DetailBucket],
    row_entries: &[AxisEntry],
    column_entries: &[AxisEntry],
    value_count: usize,
) -> Vec<Vec<Vec<AggregateState>>> {
    let mut output = vec![
        vec![vec![AggregateState::default(); value_count]; column_entries.len()];
        row_entries.len()
    ];
    for detail in details {
        let row_matches: Vec<_> = row_entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| entry_matches(entry, &detail.row_key).then_some(index))
            .collect();
        let column_matches: Vec<_> = column_entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| entry_matches(entry, &detail.column_key).then_some(index))
            .collect();
        for row in &row_matches {
            for column in &column_matches {
                for value in 0..value_count {
                    output[*row][*column][value].merge(&detail.values[value]);
                }
            }
        }
    }
    output
}

fn subtotal_settings(input: &Map<String, Value>) -> PivotResult<(bool, bool)> {
    let Some(value) = input.get("subtotals") else {
        return Ok((true, false));
    };
    if let Some(enabled) = value.as_bool() {
        return Ok((enabled, enabled));
    }
    let object = value.as_object().ok_or_else(|| {
        PivotRefreshError::new(
            "PIVOT_LAYOUT",
            "/subtotals",
            "subtotals must be a boolean or object",
        )
    })?;
    Ok((
        object.get("rows").and_then(Value::as_bool).unwrap_or(true),
        object
            .get("columns")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    ))
}

fn grand_total_settings(input: &Map<String, Value>) -> PivotResult<(bool, bool)> {
    let value = input.get("grandTotals").or_else(|| input.get("grandTotal"));
    let Some(value) = value else {
        return Ok((true, true));
    };
    if let Some(enabled) = value.as_bool() {
        return Ok((enabled, enabled));
    }
    let object = value.as_object().ok_or_else(|| {
        PivotRefreshError::new(
            "PIVOT_LAYOUT",
            "/grandTotals",
            "grandTotals must be a boolean or object",
        )
    })?;
    Ok((
        object.get("rows").and_then(Value::as_bool).unwrap_or(true),
        object
            .get("columns")
            .and_then(Value::as_bool)
            .unwrap_or(true),
    ))
}

fn entry_labels(entry: &AxisEntry, fields: &[AxisField]) -> Vec<Value> {
    if fields.is_empty() {
        return Vec::new();
    }
    match entry.kind {
        EntryKind::Detail => entry.key.clone(),
        EntryKind::Grand => {
            let mut output = vec![Value::Null; fields.len()];
            output[0] = Value::String("Grand Total".to_string());
            output
        }
        EntryKind::Subtotal => {
            let mut output = vec![Value::Null; fields.len()];
            for index in 0..entry.prefix_len {
                output[index] = entry.key[index].clone();
            }
            if entry.prefix_len > 0 {
                output[entry.prefix_len - 1] = Value::String(format!(
                    "{} Total",
                    display_text(&entry.key[entry.prefix_len - 1])
                ));
            }
            output
        }
    }
}

fn entry_kind_name(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::Detail => "detail",
        EntryKind::Subtotal => "subtotal",
        EntryKind::Grand => "grandTotal",
    }
}

fn render_table(
    rows: &[AxisField],
    columns: &[AxisField],
    values: &[ValueField],
    row_entries: &[AxisEntry],
    column_entries: &[AxisEntry],
    cells: &[Vec<Vec<AggregateState>>],
    empty: EmptyPolicy,
) -> (Vec<Vec<Value>>, usize) {
    let mut table = Vec::new();
    if !columns.is_empty() {
        for level in 0..columns.len() {
            let mut header = vec![Value::Null; rows.len()];
            for entry in column_entries {
                let value = match entry.kind {
                    EntryKind::Grand => {
                        (level == 0).then(|| Value::String("Grand Total".to_string()))
                    }
                    EntryKind::Detail => entry.key.get(level).cloned(),
                    EntryKind::Subtotal => {
                        if level < entry.prefix_len {
                            let value = entry.key[level].clone();
                            if level + 1 == entry.prefix_len {
                                Some(Value::String(format!("{} Total", display_text(&value))))
                            } else {
                                Some(value)
                            }
                        } else {
                            None
                        }
                    }
                }
                .unwrap_or(Value::Null);
                for _ in values {
                    header.push(value.clone());
                }
            }
            table.push(header);
        }
    }
    let mut measure_header: Vec<Value> = rows
        .iter()
        .map(|field| Value::String(field.name.clone()))
        .collect();
    for _ in column_entries {
        for value in values {
            measure_header.push(Value::String(value.name.clone()));
        }
    }
    table.push(measure_header);
    let header_rows = table.len();
    for (row_index, entry) in row_entries.iter().enumerate() {
        let mut rendered = entry_labels(entry, rows);
        for column_index in 0..column_entries.len() {
            for (value_index, value) in values.iter().enumerate() {
                rendered.push(
                    cells[row_index][column_index][value_index]
                        .finish(value.aggregate, empty.values),
                );
            }
        }
        table.push(rendered);
    }
    (table, header_rows)
}

fn xml_escape(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&apos;"),
            character => output.push(character),
        }
    }
    output
}

fn cache_value_kind(value: &Value, date_typed: bool) -> &'static str {
    match value {
        Value::Null => "missing",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) if date_typed => "date",
        Value::String(_) => "string",
        Value::Array(_) | Value::Object(_) => "string",
    }
}

fn cache_shared_item_xml(value: &Value, date_typed: bool) -> String {
    let kind = cache_value_kind(value, date_typed);
    let encoded = xml_escape(&display_text(value));
    match kind {
        "missing" => "<m/>".to_string(),
        "boolean" => format!("<b v=\"{}\"/>", u8::from(value.as_bool().unwrap_or(false))),
        "number" => format!("<n v=\"{encoded}\"/>"),
        "date" => format!("<d v=\"{encoded}\"/>"),
        _ => format!("<s v=\"{encoded}\"/>"),
    }
}

fn cache_records_model(table: &PreparedTable) -> Value {
    let date_sources: BTreeSet<_> = table
        .fields
        .iter()
        .filter_map(|field| field.date_group.map(|_| field.source_index))
        .collect();
    let mut shared_values: Vec<Vec<Value>> = vec![Vec::new(); table.fields.len()];
    let mut shared_indices: Vec<BTreeMap<String, usize>> =
        vec![BTreeMap::new(); table.fields.len()];
    for row in &table.rows {
        for (field, value) in row.iter().enumerate().take(table.fields.len()) {
            if is_blank(value) {
                continue;
            }
            let key = canonical_value(value);
            if !shared_indices[field].contains_key(&key) {
                let index = shared_values[field].len();
                shared_values[field].push(value.clone());
                shared_indices[field].insert(key, index);
            }
        }
    }
    let fields: Vec<_> = table
        .fields
        .iter()
        .enumerate()
        .map(|(field_index, field)| {
            let date_typed =
                field.date_group.is_none() && date_sources.contains(&field.source_index);
            let mut kinds = BTreeSet::new();
            let items: Vec<_> = shared_values[field_index]
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    let kind = cache_value_kind(value, date_typed);
                    kinds.insert(kind);
                    json!({ "index":index, "kind":kind, "value":value })
                })
                .collect();
            let contains_blank = table
                .rows
                .iter()
                .any(|row| row.get(field_index).is_none_or(is_blank));
            let shared_xml = shared_values[field_index]
                .iter()
                .map(|value| cache_shared_item_xml(value, date_typed))
                .collect::<String>();
            let group = field.date_group.map(|unit| {
                json!({
                    "base":field.source_index,
                    "unit":unit.suffix().to_ascii_lowercase(),
                    "rangePr": {
                        "groupBy":unit.suffix().to_ascii_lowercase(),
                        "autoStart":true,
                        "autoEnd":true
                    }
                })
            });
            json!({
                "index":field_index,
                "name":field.name,
                "sourceIndex":field.source_index,
                "dateGroup":group,
                "containsBlank":contains_blank,
                "containsString":kinds.contains("string"),
                "containsNumber":kinds.contains("number"),
                "containsDate":kinds.contains("date"),
                "containsBoolean":kinds.contains("boolean"),
                "sharedItems":items,
                "sharedItemsXml":format!("<sharedItems count=\"{}\"{}>{}</sharedItems>",
                    shared_values[field_index].len(),
                    if contains_blank { " containsBlank=\"1\"" } else { "" },
                    shared_xml)
            })
        })
        .collect();
    let records: Vec<_> = table
        .rows
        .iter()
        .enumerate()
        .map(|(row_index, row)| {
            let items: Vec<_> = table
                .fields
                .iter()
                .enumerate()
                .map(|(field_index, _)| {
                    let value = row.get(field_index).cloned().unwrap_or(Value::Null);
                    if is_blank(&value) {
                        json!({"kind":"missing"})
                    } else {
                        json!({
                            "kind":"shared",
                            "index":shared_indices[field_index][&canonical_value(&value)]
                        })
                    }
                })
                .collect();
            json!({ "sourceRow":row_index, "values":row, "items":items })
        })
        .collect();
    let record_body = records
        .iter()
        .map(|record| {
            let cells = record["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| {
                    if item["kind"] == "missing" {
                        "<m/>".to_string()
                    } else {
                        format!("<x v=\"{}\"/>", item["index"].as_u64().unwrap_or(0))
                    }
                })
                .collect::<String>();
            format!("<r>{cells}</r>")
        })
        .collect::<String>();
    let records_xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><pivotCacheRecords xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" count=\"{}\">{record_body}</pivotCacheRecords>",
        records.len()
    );
    json!({
        "fieldCount":fields.len(),
        "recordCount":records.len(),
        "fields":fields,
        "records":records,
        "recordsXml":records_xml,
    })
}

fn native_patches(
    input: &Map<String, Value>,
    table: &PreparedTable,
    rows: &[AxisField],
    columns: &[AxisField],
    pages: &[AxisField],
    values: &[ValueField],
    sorts: &[SortSpec],
    row_grand: bool,
    column_grand: bool,
    cache: &Value,
) -> Value {
    let mut column_indices: Vec<Value> = columns
        .iter()
        .map(|field| Value::from(field.index))
        .collect();
    if values.len() > 1 {
        column_indices.push(Value::from(-2));
    }
    let mut field_patches = Vec::new();
    for (axis_name, fields) in [("row", rows), ("column", columns)] {
        for (level, field) in fields.iter().enumerate() {
            let sort_type = sorts
                .iter()
                .find(|sort| sort.axis == axis_name && sort.level == level && !sort.by_value)
                .map(|sort| {
                    if sort.descending {
                        "descending"
                    } else {
                        "ascending"
                    }
                })
                .unwrap_or("ascending");
            field_patches.push(json!({
                "index":field.index,
                "subtotals":{"defaultSubtotal":field.subtotal},
                "sort":{"sortType":sort_type}
            }));
        }
    }
    let data: Vec<_> = values
        .iter()
        .map(|field| {
            json!({
                "fld":field.index,
                "name":field.name,
                "subtotal":field.aggregate.native_subtotal(),
                "showDataAs":"normal"
            })
        })
        .collect();
    let page_fields: Vec<_> = pages
        .iter()
        .map(|field| json!({"fld":field.index,"hier":-1,"name":field.name}))
        .collect();
    let pivot_table = json!({
        "display":{
            "rowGrandTotals":row_grand,
            "colGrandTotals":column_grand,
            "multipleFieldFilters":true,
            "preserveFormatting":true
        },
        "fields":field_patches,
        "axes":{
            "rows":rows.iter().map(|field| Value::from(field.index)).collect::<Vec<_>>(),
            "columns":column_indices,
            "pages":page_fields,
            "data":data
        }
    });
    let cache_records_patch = json!({
        "op":"replaceCacheFieldsAndRecords",
        "cacheFields":cache["fields"],
        "recordCount":cache["recordCount"],
        "records":cache["records"],
        "recordsXml":cache["recordsXml"],
        "requiresCacheFieldRewrite":table.fields.len() != table.source.fields.len()
    });
    json!({
        // These two members are directly consumable by the existing differential editors.
        "nativePivotTable":pivot_table,
        "nativePivotCache":{
            "refresh":{
                "refreshOnLoad":false,
                "enableRefresh":true,
                "backgroundQuery":false,
                "saveData":true
            }
        },
        // The package layer owns cacheFields/cacheRecords part creation and relationship wiring.
        "cacheRecords":cache_records_patch,
        "sourceRef":input.get("sourceRef").cloned().unwrap_or(Value::Null)
    })
}

fn axis_entries_json(entries: &[AxisEntry]) -> Value {
    Value::Array(
        entries
            .iter()
            .map(|entry| {
                json!({
                    "kind":entry_kind_name(entry.kind),
                    "key":entry.key,
                    "prefixLength":entry.prefix_len
                })
            })
            .collect(),
    )
}

/// Refresh a worksheet-backed pivot table entirely in memory.
///
/// Layout can be supplied at the request root or under `layout`.  Field references accept either
/// names or zero-based indexes.  The stable request surface is:
///
/// ```text
/// { source:{fields,rows}, rows:[], columns:[], pages:[], filters:[], values:[],
///   sort:[], emptyPolicy, subtotals, grandTotals, dateGroups:[] }
/// ```
pub(crate) fn refresh_pivot_local(input: &Value) -> PivotResult<Value> {
    let request = input.as_object().ok_or_else(|| {
        PivotRefreshError::new(
            "PIVOT_REQUEST",
            "/",
            "pivot refresh input must be an object",
        )
    })?;
    let layout = request
        .get("layout")
        .and_then(Value::as_object)
        .unwrap_or(request);
    let source = parse_source(input)?;
    let prepared = prepare_table(layout, source)?;
    let rows = parse_axis(layout, &prepared, "rows")?;
    let columns = parse_axis(layout, &prepared, "columns")?;
    let pages = parse_axis(layout, &prepared, "pages")?;
    let mut used = BTreeSet::new();
    for (name, fields) in [("rows", &rows), ("columns", &columns), ("pages", &pages)] {
        for field in fields {
            if !used.insert(field.index) {
                return error(
                    "PIVOT_LAYOUT",
                    format!("/{name}"),
                    format!("field {} cannot appear on multiple axes", field.name),
                );
            }
        }
    }
    let values = parse_value_fields(layout, &prepared)?;
    let empty = parse_empty_policy(layout)?;
    let filtered = apply_filters(layout, &prepared)?;
    let details = aggregate_details(&filtered, &rows, &columns, &values, empty);
    let sorts = parse_sort_specs(layout, &rows, &columns, &values)?;
    let mut row_keys = unique_axis_keys(&details, true);
    let mut column_keys = unique_axis_keys(&details, false);
    sort_axis_keys(&mut row_keys, "row", &sorts, &details, &values, empty);
    sort_axis_keys(&mut column_keys, "column", &sorts, &details, &values, empty);
    let (row_subtotals, column_subtotals) = subtotal_settings(layout)?;
    let (row_grand, column_grand) = grand_total_settings(layout)?;
    let row_entries = build_axis_entries(&row_keys, &rows, row_subtotals, row_grand);
    let column_entries = build_axis_entries(&column_keys, &columns, column_subtotals, column_grand);
    let combined = combine_entries(&details, &row_entries, &column_entries, values.len());
    let (table, header_rows) = render_table(
        &rows,
        &columns,
        &values,
        &row_entries,
        &column_entries,
        &combined,
        empty,
    );
    let cache = cache_records_model(&prepared);
    let mut warnings = prepared.warnings.clone();
    if values
        .iter()
        .any(|value| value.aggregate == Aggregate::DistinctCount)
    {
        warnings.push(json!({
            "code":"PIVOT_DISTINCT_COUNT_EXTENSION",
            "message":"local distinct-count values are exact; the native PivotTable patch uses the ECMA count fallback and requires the package layer to preserve/create the x14 distinctCount extension"
        }));
    }
    let patches = native_patches(
        layout,
        &prepared,
        &rows,
        &columns,
        &pages,
        &values,
        &sorts,
        row_grand,
        column_grand,
        &cache,
    );
    Ok(json!({
        "ok":true,
        "engine":"unicell-local-worksheet-pivot-v1",
        "source":{"fieldCount":prepared.source.fields.len(),"recordCount":prepared.source.rows.len()},
        "filteredRecordCount":filtered.len(),
        "fields":prepared.fields.iter().enumerate().map(|(index, field)| json!({
            "index":index,"name":field.name,"sourceIndex":field.source_index,
            "dateGroup":field.date_group.map(DateUnit::suffix)
        })).collect::<Vec<_>>(),
        "layout":{
            "rows":rows.iter().map(|field| json!({"index":field.index,"name":field.name})).collect::<Vec<_>>(),
            "columns":columns.iter().map(|field| json!({"index":field.index,"name":field.name})).collect::<Vec<_>>(),
            "pages":pages.iter().map(|field| json!({"index":field.index,"name":field.name})).collect::<Vec<_>>(),
            "values":values.iter().map(|field| json!({
                "index":field.index,"name":field.name,"aggregate":field.aggregate.caption()
            })).collect::<Vec<_>>()
        },
        "rowEntries":axis_entries_json(&row_entries),
        "columnEntries":axis_entries_json(&column_entries),
        "result":{"headerRows":header_rows,"rows":table},
        "cacheRecords":cache,
        "ooxmlPatches":patches,
        "warnings":warnings,
        "boundaries":{
            "worksheetCache":true,
            "olap":false,
            "dataModel":false,
            "mdx":false,
            "dax":false
        }
    }))
}

/// Convenience wrapper for HTTP handlers which still use `Result<Value, String>`.
pub(crate) fn refresh_pivot_local_json(input: &Value) -> Result<Value, String> {
    refresh_pivot_local(input).map_err(|error| error.to_string())
}

/// Structured diagnostic form for clients which need stable codes and JSON pointers.
pub(crate) fn diagnose_pivot_local(input: &Value) -> Value {
    match refresh_pivot_local(input) {
        Ok(value) => value,
        Err(error) => json!({"ok":false,"error":error.as_json()}),
    }
}

#[derive(Clone, Debug)]
struct PackageAttributeSpan {
    name: String,
    value: String,
    value_range: Range<usize>,
}

#[derive(Clone, Debug)]
struct PackageFragmentLayout {
    root_name: String,
    root_range: Range<usize>,
    children: Vec<(String, Range<usize>)>,
}

fn inherited_fragment_prefixes(fragment: &str) -> BTreeSet<String> {
    let bytes = fragment.as_bytes();
    let mut prefixes = BTreeSet::new();
    for (colon, byte) in bytes.iter().enumerate() {
        if *byte != b':' || colon == 0 {
            continue;
        }
        let mut start = colon;
        while start > 0
            && (bytes[start - 1].is_ascii_alphanumeric()
                || matches!(bytes[start - 1], b'_' | b'-' | b'.'))
        {
            start -= 1;
        }
        if start == colon {
            continue;
        }
        let prefix = &fragment[start..colon];
        if prefix != "xml"
            && prefix != "xmlns"
            && prefix
                .bytes()
                .next()
                .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        {
            prefixes.insert(prefix.to_string());
        }
    }
    prefixes
}

fn fragment_layout(fragment: &str) -> Result<PackageFragmentLayout, String> {
    let collect = |root: Node<'_, '_>, offset: usize| PackageFragmentLayout {
        root_name: package_local_name(root).to_string(),
        root_range: root.range().start - offset..root.range().end - offset,
        children: root
            .children()
            .filter(Node::is_element)
            .map(|node| {
                (
                    package_local_name(node).to_string(),
                    node.range().start - offset..node.range().end - offset,
                )
            })
            .collect(),
    };
    if let Ok(document) = Document::parse(fragment) {
        return Ok(collect(document.root_element(), 0));
    }
    let declarations = inherited_fragment_prefixes(fragment)
        .into_iter()
        .map(|prefix| format!(" xmlns:{prefix}=\"urn:unicell:inherited:{prefix}\""))
        .collect::<String>();
    let opening = format!("<unicell_wrapper{declarations}>");
    let wrapped = format!("{opening}{fragment}</unicell_wrapper>");
    let document =
        Document::parse(&wrapped).map_err(|error| format!("invalid XML fragment: {error}"))?;
    let root = document
        .root_element()
        .children()
        .find(Node::is_element)
        .ok_or_else(|| "wrapped XML fragment has no root element".to_string())?;
    if root.range().start != opening.len() || root.range().end < opening.len() {
        return Err("wrapped XML fragment offsets are inconsistent".to_string());
    }
    Ok(collect(root, opening.len()))
}

fn package_local_name<'a, 'input>(node: Node<'a, 'input>) -> &'a str {
    node.tag_name().name()
}

fn package_direct_child<'a, 'input>(
    node: Node<'a, 'input>,
    name: &str,
) -> Option<Node<'a, 'input>> {
    node.children()
        .find(|child| child.is_element() && package_local_name(*child) == name)
}

fn scan_package_attributes(tag: &str) -> Result<Vec<PackageAttributeSpan>, String> {
    let bytes = tag.as_bytes();
    if bytes.first() != Some(&b'<') || bytes.last() != Some(&b'>') {
        return Err("invalid XML start tag".to_string());
    }
    let mut cursor = 1usize;
    while cursor < bytes.len()
        && !bytes[cursor].is_ascii_whitespace()
        && !matches!(bytes[cursor], b'/' | b'>')
    {
        cursor += 1;
    }
    let mut attributes = Vec::new();
    while cursor < bytes.len() {
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= bytes.len() || matches!(bytes[cursor], b'/' | b'>') {
            break;
        }
        let name_start = cursor;
        while cursor < bytes.len()
            && !bytes[cursor].is_ascii_whitespace()
            && !matches!(bytes[cursor], b'=' | b'/' | b'>')
        {
            cursor += 1;
        }
        let name_end = cursor;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b'=') {
            return Err("malformed XML attribute".to_string());
        }
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let quote = match bytes.get(cursor) {
            Some(b'\'') => b'\'',
            Some(b'"') => b'"',
            _ => return Err("XML attribute value is not quoted".to_string()),
        };
        cursor += 1;
        let value_start = cursor;
        while cursor < bytes.len() && bytes[cursor] != quote {
            cursor += 1;
        }
        if cursor >= bytes.len() {
            return Err("unterminated XML attribute".to_string());
        }
        let value_end = cursor;
        cursor += 1;
        attributes.push(PackageAttributeSpan {
            name: tag[name_start..name_end].to_string(),
            value: tag[value_start..value_end].to_string(),
            value_range: value_start..value_end,
        });
    }
    Ok(attributes)
}

fn patch_root_attribute(
    fragment: &str,
    expected_root: &str,
    name: &str,
    value: &str,
    semantically_equal: impl Fn(&str) -> bool,
) -> Result<String, String> {
    let layout = fragment_layout(fragment)?;
    if layout.root_name != expected_root {
        return Err(format!("XML root is not {expected_root}"));
    }
    let bytes = fragment.as_bytes();
    let mut cursor = layout.root_range.start;
    let mut quote = None;
    let tag_range = loop {
        if cursor >= bytes.len() {
            return Err(format!("{expected_root} start tag is not closed"));
        }
        let character = bytes[cursor] as char;
        if let Some(open) = quote {
            if character == open {
                quote = None;
            }
        } else if character == '\'' || character == '"' {
            quote = Some(character);
        } else if character == '>' {
            break layout.root_range.start..cursor + 1;
        }
        cursor += 1;
    };
    let tag = &fragment[tag_range.clone()];
    if let Some(attribute) = scan_package_attributes(tag)?
        .into_iter()
        .find(|attribute| attribute.name == name)
    {
        if semantically_equal(&attribute.value) {
            return Ok(fragment.to_string());
        }
        let mut output = fragment.to_string();
        output.replace_range(
            tag_range.start + attribute.value_range.start
                ..tag_range.start + attribute.value_range.end,
            &xml_escape(value),
        );
        return Ok(output);
    }
    let insert = if tag.as_bytes().get(tag.len().saturating_sub(2)) == Some(&b'/') {
        tag_range.end - 2
    } else {
        tag_range.end - 1
    };
    let mut output = fragment.to_string();
    output.insert_str(insert, &format!(" {name}=\"{}\"", xml_escape(value)));
    Ok(output)
}

fn patch_string_attribute(
    fragment: &str,
    expected_root: &str,
    name: &str,
    value: &str,
) -> Result<String, String> {
    patch_root_attribute(fragment, expected_root, name, value, |existing| {
        existing == value
    })
}

fn patch_unsigned_attribute(
    fragment: &str,
    expected_root: &str,
    name: &str,
    value: u64,
) -> Result<String, String> {
    patch_root_attribute(
        fragment,
        expected_root,
        name,
        &value.to_string(),
        |existing| existing.parse::<u64>().ok() == Some(value),
    )
}

fn patch_boolean_attribute(
    fragment: &str,
    expected_root: &str,
    name: &str,
    value: bool,
) -> Result<String, String> {
    let open_end = fragment
        .as_bytes()
        .iter()
        .position(|byte| *byte == b'>')
        .ok_or_else(|| format!("{expected_root} start tag is not closed"))?;
    if !value
        && !scan_package_attributes(&fragment[..=open_end])?
            .iter()
            .any(|attribute| attribute.name == name)
    {
        return Ok(fragment.to_string());
    }
    patch_root_attribute(
        fragment,
        expected_root,
        name,
        if value { "1" } else { "0" },
        |existing| {
            matches!(existing, "1" | "true" | "on") == value
                && matches!(existing, "0" | "false" | "off") != value
        },
    )
}

fn package_apply_ranges(
    value: &str,
    mut replacements: Vec<(Range<usize>, String)>,
) -> Result<String, String> {
    replacements.sort_by(|left, right| right.0.start.cmp(&left.0.start));
    let mut previous = value.len();
    let mut output = value.to_string();
    for (range, replacement) in replacements {
        if range.start > range.end || range.end > previous || range.end > output.len() {
            return Err("overlapping or invalid XML replacements".to_string());
        }
        previous = range.start;
        output.replace_range(range, &replacement);
    }
    Ok(output)
}

fn package_qualified_name(fragment: &str) -> Result<String, String> {
    let layout = fragment_layout(fragment)?;
    let rest = &fragment[layout.root_range.start..];
    let end_tag = rest
        .find('>')
        .ok_or_else(|| "XML start tag is not closed".to_string())?;
    let tag = &rest[..=end_tag];
    let end = tag
        .bytes()
        .enumerate()
        .skip(1)
        .find(|(_, byte)| byte.is_ascii_whitespace() || matches!(*byte, b'/' | b'>'))
        .map(|(index, _)| index)
        .unwrap_or(tag.len() - 1);
    Ok(tag[1..end].to_string())
}

fn insert_before_root_close(fragment: &str, inserted: &str) -> Result<String, String> {
    let layout = fragment_layout(fragment)?;
    let close = fragment[layout.root_range.clone()]
        .rfind("</")
        .map(|index| layout.root_range.start + index)
        .ok_or_else(|| "cannot insert a child into a self-closing element".to_string())?;
    let mut output = fragment.to_string();
    output.insert_str(close, inserted);
    Ok(output)
}

fn normalize_package_part(path: &str) -> Result<String, String> {
    let replaced = path.replace('\\', "/");
    let mut components = Vec::new();
    for component in replaced.trim_start_matches('/').split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if components.pop().is_none() {
                    return Err(format!("OPC part escapes package root: {path}"));
                }
            }
            value => components.push(value),
        }
    }
    if components.is_empty() {
        Err(format!("invalid empty OPC part: {path}"))
    } else {
        Ok(components.join("/"))
    }
}

fn package_part_directory(part: &str) -> &str {
    part.rsplit_once('/')
        .map(|(directory, _)| directory)
        .unwrap_or("")
}

fn package_relationship_part(owner: &str) -> String {
    match owner.rsplit_once('/') {
        Some((directory, file)) => format!("{directory}/_rels/{file}.rels"),
        None => format!("_rels/{owner}.rels"),
    }
}

fn resolve_package_target(owner: &str, target: &str) -> Result<String, String> {
    if target.starts_with('/') {
        normalize_package_part(target)
    } else {
        let directory = package_part_directory(owner);
        if directory.is_empty() {
            normalize_package_part(target)
        } else {
            normalize_package_part(&format!("{directory}/{target}"))
        }
    }
}

fn relative_package_target(owner: &str, target: &str) -> String {
    let owner: Vec<_> = package_part_directory(owner)
        .split('/')
        .filter(|component| !component.is_empty())
        .collect();
    let target: Vec<_> = target
        .split('/')
        .filter(|component| !component.is_empty())
        .collect();
    let common = owner
        .iter()
        .zip(&target)
        .take_while(|(left, right)| left == right)
        .count();
    let mut output = vec![".."; owner.len().saturating_sub(common)];
    output.extend(target[common..].iter().copied());
    output.join("/")
}

#[derive(Clone, Debug)]
struct RecordsRelationship {
    id: String,
    part: String,
}

fn find_records_relationship(
    parts: &BTreeMap<String, Vec<u8>>,
    cache_part: &str,
) -> Result<Option<RecordsRelationship>, String> {
    let relationship_part = package_relationship_part(cache_part);
    let Some(bytes) = parts.get(&relationship_part) else {
        return Ok(None);
    };
    let xml = std::str::from_utf8(bytes)
        .map_err(|error| format!("{relationship_part} is not UTF-8 XML: {error}"))?;
    let document = Document::parse(xml)
        .map_err(|error| format!("invalid relationships XML in {relationship_part}: {error}"))?;
    let mut matches = document
        .descendants()
        .filter(|node| node.is_element() && package_local_name(*node) == "Relationship")
        .filter(|node| {
            node.attribute("Type")
                .is_some_and(|value| value.ends_with("/pivotCacheRecords"))
                && !node
                    .attribute("TargetMode")
                    .is_some_and(|value| value.eq_ignore_ascii_case("External"))
        });
    let Some(relationship) = matches.next() else {
        return Ok(None);
    };
    if matches.next().is_some() {
        return Err(format!(
            "{relationship_part} contains more than one pivotCacheRecords relationship"
        ));
    }
    let id = relationship.attribute("Id").ok_or_else(|| {
        format!("pivotCacheRecords relationship in {relationship_part} has no Id")
    })?;
    let target = relationship.attribute("Target").ok_or_else(|| {
        format!("pivotCacheRecords relationship in {relationship_part} has no Target")
    })?;
    Ok(Some(RecordsRelationship {
        id: id.to_string(),
        part: resolve_package_target(cache_part, target)?,
    }))
}

fn infer_related_part(
    parts: &BTreeMap<String, Vec<u8>>,
    owner: &str,
    relationship_suffix: &str,
) -> Result<Option<String>, String> {
    let relationship_part = package_relationship_part(owner);
    let Some(bytes) = parts.get(&relationship_part) else {
        return Ok(None);
    };
    let xml = std::str::from_utf8(bytes)
        .map_err(|error| format!("{relationship_part} is not UTF-8 XML: {error}"))?;
    let document = Document::parse(xml)
        .map_err(|error| format!("invalid relationships XML in {relationship_part}: {error}"))?;
    let mut parts_found = BTreeSet::new();
    for relationship in document.descendants().filter(|node| {
        node.is_element()
            && package_local_name(*node) == "Relationship"
            && node
                .attribute("Type")
                .is_some_and(|value| value.ends_with(relationship_suffix))
            && !node
                .attribute("TargetMode")
                .is_some_and(|value| value.eq_ignore_ascii_case("External"))
    }) {
        if let Some(target) = relationship.attribute("Target") {
            parts_found.insert(resolve_package_target(owner, target)?);
        }
    }
    if parts_found.len() > 1 {
        return Err(format!(
            "{relationship_part} resolves multiple {relationship_suffix} targets"
        ));
    }
    Ok(parts_found.into_iter().next())
}

fn derive_records_part(parts: &BTreeMap<String, Vec<u8>>, cache_part: &str) -> String {
    let directory = package_part_directory(cache_part);
    let stem = cache_part
        .rsplit_once('/')
        .map(|(_, name)| name)
        .unwrap_or(cache_part)
        .trim_end_matches(".xml");
    let suffix: String = stem
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    let starting = suffix.parse::<usize>().unwrap_or(1).max(1);
    for offset in 0..100_000usize {
        let name = format!("pivotCacheRecords{}.xml", starting + offset);
        let candidate = if directory.is_empty() {
            name
        } else {
            format!("{directory}/{name}")
        };
        if !parts.contains_key(&candidate) {
            return candidate;
        }
    }
    format!("{directory}/pivotCacheRecords-local.xml")
}

fn add_records_relationship(
    parts: &mut BTreeMap<String, Vec<u8>>,
    cache_part: &str,
    records_part: &str,
) -> Result<String, String> {
    let relationship_part = package_relationship_part(cache_part);
    let existing = parts.get(&relationship_part).cloned();
    let mut xml = if let Some(bytes) = existing {
        String::from_utf8(bytes)
            .map_err(|error| format!("{relationship_part} is not UTF-8 XML: {error}"))?
    } else {
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"></Relationships>".to_string()
    };
    let document = Document::parse(&xml)
        .map_err(|error| format!("invalid relationships XML in {relationship_part}: {error}"))?;
    if package_local_name(document.root_element()) != "Relationships" {
        return Err(format!("{relationship_part} root is not Relationships"));
    }
    let mut used = BTreeSet::new();
    for relationship in document
        .descendants()
        .filter(|node| node.is_element() && package_local_name(*node) == "Relationship")
    {
        if let Some(id) = relationship.attribute("Id") {
            used.insert(id.to_string());
        }
    }
    let mut number = 1usize;
    let id = loop {
        let candidate = format!("rIdLocalRefresh{number}");
        if !used.contains(&candidate) {
            break candidate;
        }
        number += 1;
    };
    let target = relative_package_target(cache_part, records_part);
    let fragment = format!(
        "<Relationship Id=\"{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotCacheRecords\" Target=\"{}\"/>",
        xml_escape(&id),
        xml_escape(&target)
    );
    xml = insert_before_root_close(&xml, &fragment)?;
    parts.insert(relationship_part, xml.into_bytes());
    Ok(id)
}

fn ensure_records_content_type(
    parts: &mut BTreeMap<String, Vec<u8>>,
    records_part: &str,
) -> Result<(), String> {
    const CONTENT_TYPE: &str =
        "application/vnd.openxmlformats-officedocument.spreadsheetml.pivotCacheRecords+xml";
    let path = "[Content_Types].xml";
    let bytes = parts
        .get(path)
        .ok_or_else(|| "OPC package has no [Content_Types].xml".to_string())?;
    let mut xml = std::str::from_utf8(bytes)
        .map_err(|error| format!("[Content_Types].xml is not UTF-8: {error}"))?
        .to_string();
    let document =
        Document::parse(&xml).map_err(|error| format!("invalid [Content_Types].xml: {error}"))?;
    if package_local_name(document.root_element()) != "Types" {
        return Err("[Content_Types].xml root is not Types".to_string());
    }
    let part_name = format!("/{records_part}");
    if let Some(override_node) = document.descendants().find(|node| {
        node.is_element()
            && package_local_name(*node) == "Override"
            && node.attribute("PartName") == Some(part_name.as_str())
    }) {
        if override_node.attribute("ContentType") == Some(CONTENT_TYPE) {
            return Ok(());
        }
        let range = override_node.range();
        let edited =
            patch_string_attribute(&xml[range.clone()], "Override", "ContentType", CONTENT_TYPE)?;
        xml = package_apply_ranges(&xml, vec![(range, edited)])?;
    } else {
        let fragment = format!(
            "<Override PartName=\"{}\" ContentType=\"{CONTENT_TYPE}\"/>",
            xml_escape(&part_name)
        );
        xml = insert_before_root_close(&xml, &fragment)?;
    }
    parts.insert(path.to_string(), xml.into_bytes());
    Ok(())
}

fn rewrite_known_direct_children(
    fragment: &str,
    expected_root: &str,
    known_children: &[&str],
    desired: &[String],
) -> Result<String, String> {
    let layout = fragment_layout(fragment)?;
    if layout.root_name != expected_root {
        return Err(format!("XML root is not {expected_root}"));
    }
    let existing: Vec<_> = layout
        .children
        .iter()
        .filter(|(name, _)| known_children.contains(&name.as_str()))
        .map(|(_, range)| range.clone())
        .collect();
    if existing.len() == desired.len() {
        let replacements = existing
            .into_iter()
            .zip(desired.iter())
            .filter_map(|(range, desired)| {
                (fragment[range.clone()] != *desired).then(|| (range, desired.clone()))
            })
            .collect();
        return package_apply_ranges(fragment, replacements);
    }
    if existing.is_empty() {
        if desired.is_empty() {
            return Ok(fragment.to_string());
        }
        let end = fragment[layout.root_range.start..]
            .find('>')
            .ok_or_else(|| format!("{expected_root} start tag is not closed"))?;
        let open_range = layout.root_range.start..layout.root_range.start + end + 1;
        let tag = &fragment[open_range.clone()];
        if tag.as_bytes().get(tag.len().saturating_sub(2)) == Some(&b'/') {
            let qualified = package_qualified_name(fragment)?;
            let mut output = fragment.to_string();
            output.replace_range(open_range.end - 2..open_range.end, ">");
            output.push_str(&desired.concat());
            output.push_str(&format!("</{qualified}>"));
            return Ok(output);
        }
        return insert_before_root_close(fragment, &desired.concat());
    }
    let insertion = existing[0].start;
    let removals = existing
        .into_iter()
        .map(|range| (range, String::new()))
        .collect();
    let mut output = package_apply_ranges(fragment, removals)?;
    output.insert_str(insertion, &desired.concat());
    Ok(output)
}

fn shared_item_fragment(item: &Value) -> Result<String, String> {
    let kind = item
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| "cache shared item has no kind".to_string())?;
    let value = item.get("value").cloned().unwrap_or(Value::Null);
    let encoded = xml_escape(&display_text(&value));
    match kind {
        "missing" => Ok("<m/>".to_string()),
        "boolean" => Ok(format!(
            "<b v=\"{}\"/>",
            u8::from(value.as_bool().unwrap_or(false))
        )),
        "number" => Ok(format!("<n v=\"{encoded}\"/>")),
        "date" => Ok(format!("<d v=\"{encoded}\"/>")),
        "string" => Ok(format!("<s v=\"{encoded}\"/>")),
        other => Err(format!("unsupported cache shared item kind {other}")),
    }
}

fn patch_shared_items(fragment: &str, field: &Value) -> Result<String, String> {
    let items = field
        .get("sharedItems")
        .and_then(Value::as_array)
        .ok_or_else(|| "cache field sharedItems must be an array".to_string())?;
    let desired: Vec<String> = items
        .iter()
        .map(shared_item_fragment)
        .collect::<Result<_, _>>()?;
    let mut output = rewrite_known_direct_children(
        fragment,
        "sharedItems",
        &["s", "n", "d", "b", "e", "m"],
        &desired,
    )?;
    output = patch_unsigned_attribute(
        &output,
        "sharedItems",
        "count",
        u64::try_from(items.len()).unwrap_or(u64::MAX),
    )?;
    let flags = [
        ("containsBlank", "containsBlank"),
        ("containsString", "containsString"),
        ("containsNumber", "containsNumber"),
        ("containsDate", "containsDate"),
        ("containsBoolean", "containsBoolean"),
    ];
    let mut type_count = 0usize;
    for (attribute, key) in flags {
        let enabled = field.get(key).and_then(Value::as_bool).unwrap_or(false);
        type_count += usize::from(enabled && key != "containsBlank");
        output = patch_boolean_attribute(&output, "sharedItems", attribute, enabled)?;
    }
    output = patch_boolean_attribute(&output, "sharedItems", "containsMixedTypes", type_count > 1)?;
    Ok(output)
}

fn new_shared_items(field: &Value) -> Result<String, String> {
    patch_shared_items("<sharedItems/>", field)
}

fn date_group_fragment(group: &Value) -> Result<String, String> {
    let base = group
        .get("base")
        .and_then(Value::as_u64)
        .ok_or_else(|| "dateGroup requires base".to_string())?;
    let unit = group
        .get("unit")
        .and_then(Value::as_str)
        .ok_or_else(|| "dateGroup requires unit".to_string())?;
    Ok(format!(
        "<fieldGroup base=\"{base}\"><rangePr groupBy=\"{}\" autoStart=\"1\" autoEnd=\"1\"/></fieldGroup>",
        xml_escape(unit)
    ))
}

fn patch_date_group(fragment: &str, group: &Value) -> Result<String, String> {
    let base = group
        .get("base")
        .and_then(Value::as_u64)
        .ok_or_else(|| "dateGroup requires base".to_string())?;
    let unit = group
        .get("unit")
        .and_then(Value::as_str)
        .ok_or_else(|| "dateGroup requires unit".to_string())?;
    let layout = fragment_layout(fragment)?;
    if layout.root_name != "fieldGroup" {
        return Err("date group fragment root is not fieldGroup".to_string());
    }
    let existing_range_pr = layout
        .children
        .iter()
        .find(|(name, _)| name == "rangePr")
        .map(|(_, range)| range.clone());
    let mut output = if let Some(range) = existing_range_pr {
        let mut range_pr = fragment[range.clone()].to_string();
        range_pr = patch_string_attribute(&range_pr, "rangePr", "groupBy", unit)?;
        range_pr = patch_boolean_attribute(&range_pr, "rangePr", "autoStart", true)?;
        range_pr = patch_boolean_attribute(&range_pr, "rangePr", "autoEnd", true)?;
        rewrite_known_direct_children(
            fragment,
            "fieldGroup",
            &["rangePr", "discretePr", "groupItems"],
            &[range_pr],
        )?
    } else {
        let desired = format!(
            "<rangePr groupBy=\"{}\" autoStart=\"1\" autoEnd=\"1\"/>",
            xml_escape(unit)
        );
        rewrite_known_direct_children(
            fragment,
            "fieldGroup",
            &["rangePr", "discretePr", "groupItems"],
            &[desired],
        )?
    };
    output = patch_unsigned_attribute(&output, "fieldGroup", "base", base)?;
    Ok(output)
}

fn patch_cache_field(fragment: &str, field: &Value) -> Result<String, String> {
    let name = field
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| "cache field requires name".to_string())?;
    let layout = fragment_layout(fragment)?;
    if layout.root_name != "cacheField" {
        return Err("cache field fragment root is not cacheField".to_string());
    }
    let shared_range = layout
        .children
        .iter()
        .find(|(name, _)| name == "sharedItems")
        .map(|(_, range)| range.clone());
    let group_range = layout
        .children
        .iter()
        .find(|(name, _)| name == "fieldGroup")
        .map(|(_, range)| range.clone());
    let shared = if let Some(range) = &shared_range {
        patch_shared_items(&fragment[range.clone()], field)?
    } else {
        new_shared_items(field)?
    };
    let desired_group = field.get("dateGroup").filter(|value| !value.is_null());
    let group = match (group_range.as_ref(), desired_group) {
        (Some(range), Some(group)) => Some(patch_date_group(&fragment[range.clone()], group)?),
        (None, Some(group)) => Some(date_group_fragment(group)?),
        _ => None,
    };
    let mut replacements = Vec::new();
    if let Some(range) = shared_range.clone() {
        replacements.push((range, shared.clone()));
    }
    if let Some(range) = group_range.clone() {
        replacements.push((range, group.clone().unwrap_or_default()));
    }
    let mut output = package_apply_ranges(fragment, replacements)?;
    if !fragment_layout(&output)?
        .children
        .iter()
        .any(|(name, _)| name == "sharedItems")
    {
        output = insert_before_root_close(&output, &shared)?;
    }
    if group_range.is_none() {
        if let Some(group) = group {
            output = insert_before_root_close(&output, &group)?;
        }
    }
    patch_string_attribute(&output, "cacheField", "name", name)
}

fn new_cache_field(field: &Value) -> Result<String, String> {
    let name = field
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| "cache field requires name".to_string())?;
    let shared = new_shared_items(field)?;
    let group = field
        .get("dateGroup")
        .filter(|value| !value.is_null())
        .map(date_group_fragment)
        .transpose()?
        .unwrap_or_default();
    Ok(format!(
        "<cacheField name=\"{}\">{shared}{group}</cacheField>",
        xml_escape(name)
    ))
}

fn rewrite_cache_fields(cache_xml: &str, fields: &[Value]) -> Result<String, String> {
    let document = Document::parse(cache_xml)
        .map_err(|error| format!("invalid PivotCache definition XML: {error}"))?;
    let root = document.root_element();
    if package_local_name(root) != "pivotCacheDefinition" {
        return Err("cache definition root is not pivotCacheDefinition".to_string());
    }
    let Some(container) = package_direct_child(root, "cacheFields") else {
        let children = fields
            .iter()
            .map(new_cache_field)
            .collect::<Result<String, _>>()?;
        let container = format!(
            "<cacheFields count=\"{}\">{children}</cacheFields>",
            fields.len()
        );
        let insertion = root
            .children()
            .find(|child| {
                child.is_element()
                    && matches!(
                        package_local_name(*child),
                        "cacheHierarchies"
                            | "kpis"
                            | "tupleCache"
                            | "calculatedItems"
                            | "calculatedMembers"
                            | "dimensions"
                            | "measureGroups"
                            | "maps"
                            | "extLst"
                    )
            })
            .map(|node| node.range().start)
            .unwrap_or_else(|| {
                let range = root.range();
                cache_xml[range.clone()]
                    .rfind("</")
                    .map(|offset| range.start + offset)
                    .unwrap_or(range.end)
            });
        let mut output = cache_xml.to_string();
        output.insert_str(insertion, &container);
        return Ok(output);
    };
    let container_range = container.range();
    let container_fragment = &cache_xml[container_range.clone()];
    let container_layout = fragment_layout(container_fragment)?;
    let existing: Vec<_> = container_layout
        .children
        .iter()
        .filter(|(name, _)| name == "cacheField")
        .map(|(_, range)| range.clone())
        .collect();
    let desired = fields
        .iter()
        .enumerate()
        .map(|(index, field)| {
            if let Some(range) = existing.get(index) {
                patch_cache_field(&container_fragment[range.clone()], field)
            } else {
                new_cache_field(field)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut edited = rewrite_known_direct_children(
        container_fragment,
        "cacheFields",
        &["cacheField"],
        &desired,
    )?;
    edited = patch_unsigned_attribute(
        &edited,
        "cacheFields",
        "count",
        u64::try_from(fields.len()).unwrap_or(u64::MAX),
    )?;
    package_apply_ranges(cache_xml, vec![(container_range, edited)])
}

fn package_request_part(
    request: &Map<String, Value>,
    names: &[&str],
) -> Result<Option<String>, String> {
    let package = request
        .get("package")
        .and_then(Value::as_object)
        .unwrap_or(request);
    let Some((name, value)) = names
        .iter()
        .find_map(|name| package.get(*name).map(|value| (*name, value)))
    else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let value = value
        .as_str()
        .ok_or_else(|| format!("request {name} must be an OPC part path string"))?;
    normalize_package_part(value).map(Some)
}

fn request_declares_olap(request: &Map<String, Value>) -> bool {
    let source = request
        .get("source")
        .and_then(Value::as_object)
        .unwrap_or(request);
    value_at(source, &["kind", "type", "sourceType"])
        .and_then(Value::as_str)
        .is_some_and(|kind| {
            let kind = kind.to_ascii_lowercase();
            ["olap", "cube", "datamodel", "data model", "vertipaq", "mdx"]
                .iter()
                .any(|needle| kind.contains(needle))
        })
}

fn result_cache_materialization(result: &Value) -> Result<(&[Value], &str, u64), String> {
    let cache = result
        .get("ooxmlPatches")
        .and_then(|value| value.get("cacheRecords"))
        .or_else(|| result.get("cacheRecords"))
        .ok_or_else(|| "local pivot result has no cacheRecords materialization".to_string())?;
    let fields = cache
        .get("cacheFields")
        .or_else(|| cache.get("fields"))
        .and_then(Value::as_array)
        .ok_or_else(|| "cacheRecords materialization has no cache fields".to_string())?;
    let records_xml = cache
        .get("recordsXml")
        .and_then(Value::as_str)
        .ok_or_else(|| "cacheRecords materialization has no recordsXml".to_string())?;
    let count = cache
        .get("recordCount")
        .and_then(Value::as_u64)
        .ok_or_else(|| "cacheRecords materialization has no recordCount".to_string())?;
    let document = Document::parse(records_xml)
        .map_err(|error| format!("generated cacheRecords XML is invalid: {error}"))?;
    let root = document.root_element();
    if package_local_name(root) != "pivotCacheRecords" {
        return Err("generated recordsXml root is not pivotCacheRecords".to_string());
    }
    let xml_count = root
        .attribute("count")
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| "generated pivotCacheRecords has no valid count".to_string())?;
    let actual = root
        .children()
        .filter(|node| node.is_element() && package_local_name(*node) == "r")
        .count() as u64;
    if xml_count != count || actual != count {
        return Err(format!(
            "cacheRecords count mismatch: result={count}, xml={xml_count}, records={actual}"
        ));
    }
    Ok((fields, records_xml, count))
}

fn changed_package_parts(
    before: &BTreeMap<String, Vec<u8>>,
    after: &BTreeMap<String, Vec<u8>>,
) -> Vec<String> {
    let keys: BTreeSet<_> = before.keys().chain(after.keys()).cloned().collect();
    keys.into_iter()
        .filter(|key| before.get(key) != after.get(key))
        .collect()
}

/// Atomically materialise a local worksheet Pivot refresh into an OOXML part map.
///
/// `request` accepts `pivotTablePart`, `pivotCacheDefinitionPart`, and `cacheRecordsPart` either
/// at the root or under `package`.  The cache definition can be inferred from the PivotTable
/// relationship, and the records part is inferred from the cache relationship.  If a worksheet
/// cache has no records relationship yet, a deterministic relationship/part/content-type entry
/// is created.  No caller-visible bytes change unless every XML edit validates successfully.
pub(crate) fn apply_local_refresh_ooxml(
    parts: &mut BTreeMap<String, Vec<u8>>,
    request: &Value,
    result: &Value,
) -> Result<Value, String> {
    let request = request
        .as_object()
        .ok_or_else(|| "local pivot package request must be an object".to_string())?;
    if request_declares_olap(request)
        || result
            .get("boundaries")
            .and_then(|value| value.get("worksheetCache"))
            .and_then(Value::as_bool)
            == Some(false)
    {
        return Err(
            "PIVOT_OLAP_UNSUPPORTED: package materialization supports worksheet caches only"
                .to_string(),
        );
    }
    if result.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err("cannot materialize an unsuccessful local pivot result".to_string());
    }
    let table_part = package_request_part(
        request,
        &["pivotTablePart", "pivot_table_part", "tablePart"],
    )?;
    let explicit_cache = package_request_part(
        request,
        &[
            "pivotCacheDefinitionPart",
            "pivot_cache_definition_part",
            "cacheDefinitionPart",
            "cachePart",
        ],
    )?;
    let cache_part = if let Some(cache) = explicit_cache {
        cache
    } else if let Some(table) = table_part.as_deref() {
        infer_related_part(parts, table, "/pivotCacheDefinition")?.ok_or_else(|| {
            format!("cannot infer PivotCache definition from relationships for {table}")
        })?
    } else {
        return Err(
            "pivotCacheDefinitionPart is required when no pivotTablePart is given".to_string(),
        );
    };
    if !parts.contains_key(&cache_part) {
        return Err(format!("missing PivotCache definition part {cache_part}"));
    }
    if let Some(table) = table_part.as_deref() {
        if !parts.contains_key(table) {
            return Err(format!("missing PivotTable part {table}"));
        }
    }
    let explicit_records = package_request_part(
        request,
        &["cacheRecordsPart", "cache_records_part", "recordsPart"],
    )?;
    let existing_relationship = find_records_relationship(parts, &cache_part)?;
    if let (Some(explicit), Some(existing)) =
        (explicit_records.as_deref(), existing_relationship.as_ref())
    {
        if explicit != existing.part {
            return Err(format!(
                "cacheRecordsPart {explicit} conflicts with relationship target {}",
                existing.part
            ));
        }
    }
    let records_part = explicit_records
        .or_else(|| {
            existing_relationship
                .as_ref()
                .map(|relationship| relationship.part.clone())
        })
        .unwrap_or_else(|| derive_records_part(parts, &cache_part));
    let (cache_fields, records_xml, record_count) = result_cache_materialization(result)?;
    let patches = result
        .get("ooxmlPatches")
        .and_then(Value::as_object)
        .ok_or_else(|| "local pivot result has no ooxmlPatches".to_string())?;
    let table_patch = patches
        .get("nativePivotTable")
        .ok_or_else(|| "local pivot result has no nativePivotTable patch".to_string())?;
    let cache_patch = patches
        .get("nativePivotCache")
        .ok_or_else(|| "local pivot result has no nativePivotCache patch".to_string())?;

    // All writes are staged.  Any relationship, content-type, native patch, cache-field, or
    // records validation failure drops this map without touching the caller's package.
    let before = parts.clone();
    let mut staged = before.clone();
    let relationship_id = if let Some(relationship) = existing_relationship {
        relationship.id
    } else {
        add_records_relationship(&mut staged, &cache_part, &records_part)?
    };
    ensure_records_content_type(&mut staged, &records_part)?;

    if let Some(table_part) = table_part.as_deref() {
        let original = std::str::from_utf8(
            staged
                .get(table_part)
                .ok_or_else(|| format!("missing PivotTable part {table_part}"))?,
        )
        .map_err(|error| format!("{table_part} is not UTF-8 XML: {error}"))?;
        let edited =
            crate::native_pivot_table_edit::apply_pivot_table_patch(original, table_patch)?;
        Document::parse(&edited)
            .map_err(|error| format!("PivotTable XML after local refresh is invalid: {error}"))?;
        staged.insert(table_part.to_string(), edited.into_bytes());
    }

    let original_cache = std::str::from_utf8(
        staged
            .get(&cache_part)
            .ok_or_else(|| format!("missing PivotCache definition part {cache_part}"))?,
    )
    .map_err(|error| format!("{cache_part} is not UTF-8 XML: {error}"))?;
    let mut edited_cache = crate::native_pivot_cache_edit::apply_pivot_cache_refresh_patch(
        original_cache,
        cache_patch,
    )?;
    edited_cache = rewrite_cache_fields(&edited_cache, cache_fields)?;
    edited_cache = patch_unsigned_attribute(
        &edited_cache,
        "pivotCacheDefinition",
        "recordCount",
        record_count,
    )?;
    edited_cache = patch_string_attribute(
        &edited_cache,
        "pivotCacheDefinition",
        "xmlns:r",
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships",
    )?;
    edited_cache = patch_string_attribute(
        &edited_cache,
        "pivotCacheDefinition",
        "r:id",
        &relationship_id,
    )?;
    Document::parse(&edited_cache)
        .map_err(|error| format!("PivotCache XML after local refresh is invalid: {error}"))?;
    staged.insert(cache_part.clone(), edited_cache.into_bytes());
    staged.insert(records_part.clone(), records_xml.as_bytes().to_vec());

    let changed_parts = changed_package_parts(&before, &staged);
    *parts = staged;
    Ok(json!({
        "ok":true,
        "changed":!changed_parts.is_empty(),
        "changedParts":changed_parts,
        "pivotTablePart":table_part,
        "pivotCacheDefinitionPart":cache_part,
        "cacheRecordsPart":records_part,
        "relationshipId":relationship_id,
        "fieldCount":cache_fields.len(),
        "recordCount":record_count
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sales_request() -> Value {
        json!({
            "source":{
                "fields":["Region","Product","Date","Amount","Order"],
                "rows":[
                    ["East","A","2026-01-04",10,"o1"],
                    ["East","A","2026-01-11",20,"o1"],
                    ["East","B","2026-02-03",5,"o2"],
                    ["West","A","2026-01-19",7,"o3"],
                    ["West","A","2026-01-20",null,"o4"],
                    ["West","B","2026-02-08",8,"o3"]
                ]
            },
            "rows":["Region","Product"],
            "columns":[{"field":"Date","group":"months"}],
            "filters":[{"field":"Amount","op":"gt","value":6}],
            "values":[
                {"field":"Amount","aggregate":"sum","caption":"Revenue"},
                {"field":"Order","aggregate":"count","caption":"Orders"},
                {"field":"Order","aggregate":"distinctCount","caption":"Unique Orders"}
            ],
            "subtotals":{"rows":true,"columns":false},
            "grandTotals":{"rows":true,"columns":true}
        })
    }

    #[test]
    fn multi_level_filter_subtotals_grand_totals_and_distinct_count() {
        let result = refresh_pivot_local(&sales_request()).unwrap();
        assert_eq!(result["filteredRecordCount"], 4);
        let row_entries = result["rowEntries"].as_array().unwrap();
        assert_eq!(row_entries.len(), 6);
        assert_eq!(row_entries[0]["key"], json!(["East", "A"]));
        assert_eq!(row_entries[1]["kind"], "subtotal");
        assert_eq!(row_entries[5]["kind"], "grandTotal");
        let column_entries = result["columnEntries"].as_array().unwrap();
        assert_eq!(column_entries.len(), 3);
        assert_eq!(column_entries[0]["key"], json!([1]));
        assert_eq!(column_entries[1]["key"], json!([2]));
        assert_eq!(column_entries[2]["kind"], "grandTotal");
        let rendered = result["result"]["rows"].as_array().unwrap();
        assert_eq!(result["result"]["headerRows"], 2);
        // East/A detail row: two row labels, two empty February cells, then Grand Total values.
        assert_eq!(rendered[2][8], 30);
        assert_eq!(rendered[2][9], 2);
        assert_eq!(rendered[2][10], 1);
        // Bottom-right values are the exact filtered grand totals.
        let grand = rendered.last().unwrap().as_array().unwrap();
        assert_eq!(grand[8], 45);
        assert_eq!(grand[9], 4);
        assert_eq!(grand[10], 2);
        assert!(
            result["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| warning["code"] == "PIVOT_DISTINCT_COUNT_EXTENSION")
        );
    }

    #[test]
    fn every_requested_aggregate_and_empty_policy_is_deterministic() {
        let request = json!({
            "source":{"fields":["Value"],"rows":[[4],[null],[2],[4],["6"]]},
            "values":[
                {"field":"Value","aggregate":"sum"},
                {"field":"Value","aggregate":"count"},
                {"field":"Value","aggregate":"avg"},
                {"field":"Value","aggregate":"min"},
                {"field":"Value","aggregate":"max"},
                {"field":"Value","aggregate":"distinct count"}
            ]
        });
        let first = refresh_pivot_local(&request).unwrap();
        let second = refresh_pivot_local(&request).unwrap();
        assert_eq!(first, second);
        let table = first["result"]["rows"].as_array().unwrap();
        let values = table[1].as_array().unwrap();
        assert_eq!(
            values,
            &vec![
                json!(16),
                json!(4),
                json!(4),
                json!(2),
                json!("6"),
                json!(3)
            ]
        );

        let empty = refresh_pivot_local(&json!({
            "source":{"fields":["Group","Value"],"rows":[[null,null]]},
            "rows":["Group"],
            "values":[{"field":"Value","aggregate":"sum"}],
            "emptyPolicy":{"axis":"zero","values":"zero"},
            "grandTotals":false
        }))
        .unwrap();
        assert_eq!(empty["result"]["rows"][1], json!([0, 0]));
    }

    #[test]
    fn date_grouping_handles_iso_excel_serial_and_iso_weeks() {
        let result = refresh_pivot_local(&json!({
            "source":{"fields":["Date","Amount"],"rows":[
                ["2024-01-01T12:34:56",2],
                [45292,3],
                ["2024-04-08",5]
            ]},
            "dateGroups":[{"field":"Date","by":["years","quarters","months","weeks"]}],
            "rows":[
                {"field":"Date","group":"years"},
                {"field":"Date","group":"quarters"},
                {"field":"Date","group":"months"}
            ],
            "values":[{"field":"Amount","aggregate":"sum"}],
            "grandTotals":false
        }))
        .unwrap();
        assert_eq!(result["fields"].as_array().unwrap().len(), 6);
        assert_eq!(result["rowEntries"][0]["key"], json!([2024, 1, 1]));
        assert!(
            result["rowEntries"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["kind"] == "detail" && entry["key"] == json!([2024, 2, 4]))
        );
        let week_field = result["cacheRecords"]["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|field| field["name"] == "Date (Weeks)")
            .unwrap();
        assert!(
            week_field["sharedItems"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["value"] == "2024-W01")
        );
    }

    #[test]
    fn worksheet_snapshot_adapter_uses_header_and_sparse_blanks() {
        let result = refresh_pivot_local(&json!({
            "source":{
                "cells":[
                    {"r":1,"c":1,"v":"Region"},{"r":1,"c":2,"v":"Amount"},
                    {"r":2,"c":1,"v":"East"},{"r":2,"c":2,"v":2},
                    {"r":3,"c":1,"v":"West"}
                ],
                "range":[1,1,3,2]
            },
            "rows":["Region"],
            "values":[{"field":"Amount","aggregate":"count"}],
            "grandTotals":false
        }))
        .unwrap();
        assert_eq!(result["source"]["recordCount"], 2);
        assert_eq!(result["result"]["rows"][1], json!(["East", 1]));
        assert_eq!(result["result"]["rows"][2], json!(["West", 0]));
    }

    #[test]
    fn cache_records_have_shared_indexes_xml_and_full_unfiltered_source() {
        let result = refresh_pivot_local(&sales_request()).unwrap();
        let cache = &result["cacheRecords"];
        assert_eq!(cache["recordCount"], 6);
        assert_eq!(cache["fieldCount"], 6);
        let xml = cache["recordsXml"].as_str().unwrap();
        assert!(xml.contains("<pivotCacheRecords"));
        assert!(xml.contains("count=\"6\""));
        assert_eq!(xml.matches("<r>").count(), 6);
        assert_eq!(xml.matches("<x v=").count(), 35); // one source null remains <m/>.
        assert_eq!(xml.matches("<m/>").count(), 1);
        assert_eq!(result["ooxmlPatches"]["cacheRecords"]["recordCount"], 6);
        assert_eq!(
            result["ooxmlPatches"]["cacheRecords"]["requiresCacheFieldRewrite"],
            true
        );
    }

    #[test]
    fn differential_native_patches_are_accepted_by_existing_editors() {
        let result = refresh_pivot_local(&sales_request()).unwrap();
        let table_xml = r#"<pivotTableDefinition xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" name="P" cacheId="1"><location ref="A1:J20" firstHeaderRow="1" firstDataRow="1" firstDataCol="2"/><pivotFields count="6"><pivotField/><pivotField/><pivotField/><pivotField/><pivotField/><pivotField/></pivotFields></pivotTableDefinition>"#;
        let edited = crate::native_pivot_table_edit::apply_pivot_table_patch(
            table_xml,
            &result["ooxmlPatches"]["nativePivotTable"],
        )
        .unwrap();
        assert!(edited.contains("<rowFields count=\"2\">"));
        assert!(edited.contains("<colFields count=\"2\">"));
        assert!(edited.contains("<dataFields count=\"3\">"));
        let cache_xml = r#"<pivotCacheDefinition xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" refreshOnLoad="1" saveData="0"><cacheSource type="worksheet"><worksheetSource sheet="Data" ref="A1:E7"/></cacheSource><cacheFields count="5"/></pivotCacheDefinition>"#;
        let cache = crate::native_pivot_cache_edit::apply_pivot_cache_refresh_patch(
            cache_xml,
            &result["ooxmlPatches"]["nativePivotCache"],
        )
        .unwrap();
        assert!(cache.contains("refreshOnLoad=\"0\""));
        assert!(cache.contains("saveData=\"1\""));
        assert!(cache.contains("enableRefresh=\"1\""));
    }

    #[test]
    fn structured_diagnostics_reject_olap_and_bad_filters() {
        let olap = diagnose_pivot_local(&json!({
            "source":{"kind":"OLAP cube","fields":["A"],"rows":[]},
            "values":[{"field":"A","aggregate":"count"}]
        }));
        assert_eq!(olap["ok"], false);
        assert_eq!(olap["error"]["code"], "PIVOT_OLAP_UNSUPPORTED");
        assert!(
            olap["error"]["message"]
                .as_str()
                .unwrap()
                .contains("MDX/DAX")
        );
        let bad = diagnose_pivot_local(&json!({
            "source":{"fields":["A"],"rows":[[1]]},
            "filters":[{"field":"A","op":"teleport"}],
            "values":[{"field":"A","aggregate":"sum"}]
        }));
        assert_eq!(bad["error"]["code"], "PIVOT_FILTER");
        assert_eq!(bad["error"]["path"], "/filters/0");
    }

    fn package_fixture(with_records_relationship: bool) -> BTreeMap<String, Vec<u8>> {
        let mut parts = BTreeMap::new();
        let records_override = if with_records_relationship {
            r#"<Override PartName="/xl/pivotCache/records/cacheRecords7.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.pivotCacheRecords+xml"/>"#
        } else {
            ""
        };
        parts.insert(
            "[Content_Types].xml".to_string(),
            format!(r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types" vendor="KEEP"><Default Extension="xml" ContentType="application/xml"/><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/pivotTables/pivotTable7.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.pivotTable+xml"/><Override PartName="/xl/pivotCache/pivotCacheDefinition7.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.pivotCacheDefinition+xml"/>{records_override}<x:future xmlns:x="urn:vendor" keep="yes"/></Types>"#).into_bytes(),
        );
        parts.insert(
            "_rels/.rels".to_string(),
            br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="office" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#.to_vec(),
        );
        parts.insert(
            "xl/workbook.xml".to_string(),
            br#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Data" sheetId="1" r:id="sheetRel"/></sheets><pivotCaches><pivotCache cacheId="7" r:id="cacheRel"/></pivotCaches></workbook>"#.to_vec(),
        );
        parts.insert(
            "xl/_rels/workbook.xml.rels".to_string(),
            br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="sheetRel" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="cacheRel" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotCacheDefinition" Target="pivotCache/pivotCacheDefinition7.xml"/></Relationships>"#.to_vec(),
        );
        parts.insert(
            "xl/worksheets/sheet1.xml".to_string(),
            br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheetData/><pivotTableParts count="1"><pivotTablePart r:id="pivotRel"/></pivotTableParts></worksheet>"#.to_vec(),
        );
        parts.insert(
            "xl/worksheets/_rels/sheet1.xml.rels".to_string(),
            br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="pivotRel" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotTable" Target="../pivotTables/pivotTable7.xml"/></Relationships>"#.to_vec(),
        );
        parts.insert(
            "xl/pivotTables/pivotTable7.xml".to_string(),
            br#"<pivotTableDefinition xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:x="urn:vendor" name="Sales" cacheId="7" preserveFormatting="1" vendor="KEEP"><location ref="A1:M40" firstHeaderRow="1" firstDataRow="2" firstDataCol="2"/><pivotFields count="6"><pivotField/><pivotField/><pivotField/><pivotField/><pivotField/><pivotField><extLst><x:field keep="yes"/></extLst></pivotField></pivotFields><extLst><x:table keep="yes"/></extLst></pivotTableDefinition>"#.to_vec(),
        );
        parts.insert(
            "xl/pivotTables/_rels/pivotTable7.xml.rels".to_string(),
            br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="tableCache" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotCacheDefinition" Target="../pivotCache/pivotCacheDefinition7.xml"/></Relationships>"#.to_vec(),
        );
        let relationship_attribute = if with_records_relationship {
            r#" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" r:id="recordsRel""#
        } else {
            ""
        };
        parts.insert(
            "xl/pivotCache/pivotCacheDefinition7.xml".to_string(),
            format!(r#"<pivotCacheDefinition xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:x="urn:vendor"{relationship_attribute} saveData="0" refreshOnLoad="1" enableRefresh="true" recordCount="1" vendorRoot="KEEP"><cacheSource type="worksheet"><worksheetSource sheet="Data" ref="A1:E7"/></cacheSource><cacheFields count="5" vendorFields="KEEP"><cacheField name="Region" vendorField="KEEP"><sharedItems count="2" containsString="1" vendorShared="KEEP"><s v="East"/><s v="West"/><extLst><x:shared keep="yes"/></extLst></sharedItems><extLst><x:field keep="yes"/></extLst></cacheField><cacheField name="Product"><sharedItems count="2"><s v="A"/><s v="B"/></sharedItems></cacheField><cacheField name="Date"><sharedItems count="1"><d v="2020-01-01"/></sharedItems></cacheField><cacheField name="Amount"><sharedItems count="1"><n v="1"/></sharedItems></cacheField><cacheField name="Order"><sharedItems count="1"><s v="old"/></sharedItems></cacheField><x:cacheFieldsExtension keep="yes"/></cacheFields><extLst><x:root keep="yes"/></extLst></pivotCacheDefinition>"#).into_bytes(),
        );
        if with_records_relationship {
            parts.insert(
                "xl/pivotCache/_rels/pivotCacheDefinition7.xml.rels".to_string(),
                br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships" vendor="KEEP"><Relationship Id="recordsRel" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotCacheRecords" Target="records/cacheRecords7.xml"/><x:future xmlns:x="urn:vendor" keep="yes"/></Relationships>"#.to_vec(),
            );
            parts.insert(
                "xl/pivotCache/records/cacheRecords7.xml".to_string(),
                br#"<pivotCacheRecords xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="1"><r><x v="0"/></r></pivotCacheRecords>"#.to_vec(),
            );
        }
        parts
    }

    fn package_request() -> Value {
        json!({
            "pivotTablePart":"xl/pivotTables/pivotTable7.xml",
            "pivotCacheDefinitionPart":"xl/pivotCache/pivotCacheDefinition7.xml"
        })
    }

    #[test]
    fn package_refresh_preserves_extensions_reopens_and_reapply_is_byte_exact() {
        let mut parts = package_fixture(true);
        let relationship_before =
            parts["xl/pivotCache/_rels/pivotCacheDefinition7.xml.rels"].clone();
        let result = refresh_pivot_local(&sales_request()).unwrap();
        let applied = apply_local_refresh_ooxml(&mut parts, &package_request(), &result).unwrap();
        assert_eq!(applied["changed"], true);
        assert_eq!(
            applied["cacheRecordsPart"],
            "xl/pivotCache/records/cacheRecords7.xml"
        );
        assert_eq!(
            parts["xl/pivotCache/_rels/pivotCacheDefinition7.xml.rels"],
            relationship_before
        );
        let cache = std::str::from_utf8(&parts["xl/pivotCache/pivotCacheDefinition7.xml"]).unwrap();
        assert!(cache.contains("vendorRoot=\"KEEP\""));
        assert!(cache.contains("vendorFields=\"KEEP\""));
        assert!(cache.contains("vendorField=\"KEEP\""));
        assert!(cache.contains("vendorShared=\"KEEP\""));
        assert!(cache.contains("<x:shared keep=\"yes\"/>"));
        assert!(cache.contains("<x:cacheFieldsExtension keep=\"yes\"/>"));
        assert!(cache.contains("<x:root keep=\"yes\"/>"));
        assert!(cache.contains("recordCount=\"6\""));
        let document = Document::parse(cache).unwrap();
        let cache_fields = package_direct_child(document.root_element(), "cacheFields").unwrap();
        assert_eq!(cache_fields.attribute("count"), Some("6"));
        assert_eq!(
            cache_fields
                .children()
                .filter(|node| node.is_element() && package_local_name(*node) == "cacheField")
                .count(),
            6
        );
        let records =
            std::str::from_utf8(&parts["xl/pivotCache/records/cacheRecords7.xml"]).unwrap();
        assert_eq!(
            Document::parse(records)
                .unwrap()
                .root_element()
                .attribute("count"),
            Some("6")
        );
        let cache_model = crate::native_pivot_cache_edit::parse_pivot_cache_model(&parts).unwrap();
        assert_eq!(cache_model["caches"][0]["fieldCount"], 6);
        let table_model = crate::native_pivot_table_edit::parse_pivot_table_model(&parts).unwrap();
        assert_eq!(
            table_model["tables"][0]["axes"]["data"]
                .as_array()
                .unwrap()
                .len(),
            3
        );

        let first_materialization = parts.clone();
        let second = apply_local_refresh_ooxml(&mut parts, &package_request(), &result).unwrap();
        assert_eq!(second["changed"], false);
        assert_eq!(parts, first_materialization);
    }

    #[test]
    fn package_refresh_creates_missing_records_relationship_and_content_type() {
        let mut parts = package_fixture(false);
        let result = refresh_pivot_local(&sales_request()).unwrap();
        let applied = apply_local_refresh_ooxml(&mut parts, &package_request(), &result).unwrap();
        assert_eq!(applied["changed"], true);
        let records_part = applied["cacheRecordsPart"].as_str().unwrap();
        assert!(parts.contains_key(records_part));
        let relationships =
            std::str::from_utf8(&parts["xl/pivotCache/_rels/pivotCacheDefinition7.xml.rels"])
                .unwrap();
        assert!(relationships.contains("pivotCacheRecords"));
        assert!(relationships.contains("rIdLocalRefresh1"));
        let types = std::str::from_utf8(&parts["[Content_Types].xml"]).unwrap();
        assert!(types.contains(&format!("PartName=\"/{records_part}\"")));
        assert!(types.contains("<x:future xmlns:x=\"urn:vendor\" keep=\"yes\"/>"));
        let cache = std::str::from_utf8(&parts["xl/pivotCache/pivotCacheDefinition7.xml"]).unwrap();
        assert!(cache.contains("r:id=\"rIdLocalRefresh1\""));
        assert!(Document::parse(cache).is_ok());
    }

    #[test]
    fn package_refresh_failure_and_olap_rejection_are_atomic() {
        let mut parts = package_fixture(true);
        let original = parts.clone();
        let result = refresh_pivot_local(&sales_request()).unwrap();
        let conflict = json!({
            "pivotTablePart":"xl/pivotTables/pivotTable7.xml",
            "pivotCacheDefinitionPart":"xl/pivotCache/pivotCacheDefinition7.xml",
            "cacheRecordsPart":"xl/pivotCache/different.xml"
        });
        assert!(apply_local_refresh_ooxml(&mut parts, &conflict, &result).is_err());
        assert_eq!(parts, original);

        let mut malformed = result.clone();
        malformed["ooxmlPatches"]["nativePivotTable"]["axes"]["rows"] = json!([999]);
        assert!(apply_local_refresh_ooxml(&mut parts, &package_request(), &malformed).is_err());
        assert_eq!(parts, original);

        let olap = json!({
            "source":{"kind":"OLAP"},
            "pivotCacheDefinitionPart":"xl/pivotCache/pivotCacheDefinition7.xml"
        });
        assert!(
            apply_local_refresh_ooxml(&mut parts, &olap, &result)
                .unwrap_err()
                .contains("PIVOT_OLAP_UNSUPPORTED")
        );
        assert_eq!(parts, original);
    }
}
