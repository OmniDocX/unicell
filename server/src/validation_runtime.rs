//! Excel-compatible runtime evaluation for standard worksheet data validation.
//!
//! OOXML parsing and lossless persistence deliberately live elsewhere.  This module accepts a
//! normalized rule plus a candidate value and evaluates it against a private in-memory clone of
//! the workbook.  Custom formulas therefore see the proposed value and all of its dependent
//! formulas, without changing the live model or its undo history.

use ironcalc::base::{UserModel, cell::CellValue, expressions::types::CellReferenceIndex};
use serde::{Deserialize, Serialize};

/// A one-based cell coordinate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CellAddress {
    /// Zero-based worksheet index.
    pub sheet: u32,
    /// One-based row.
    pub row: i32,
    /// One-based column.
    pub column: i32,
}

/// Standard OOXML data-validation types supported by the runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ValidationType {
    /// No input restriction.
    Any,
    /// Integer input.
    Whole,
    /// Numeric input.
    Decimal,
    /// Excel date serial or an ISO date input.
    Date,
    /// Excel day fraction or an ISO time input.
    Time,
    /// UTF-16 text length, matching legacy Excel LEN semantics.
    TextLength,
    /// An inline or range-backed list.
    List,
    /// A formula whose scalar result must be TRUE or non-zero.
    Custom,
}

impl ValidationType {
    /// Converts an OOXML `dataValidation/@type` value into its runtime form.
    /// Missing/empty/`none` values are the unrestricted `Any` type.
    pub fn from_ooxml(value: &str) -> Option<Self> {
        match value.trim() {
            "" | "any" | "none" => Some(Self::Any),
            "whole" => Some(Self::Whole),
            "decimal" => Some(Self::Decimal),
            "date" => Some(Self::Date),
            "time" => Some(Self::Time),
            "textLength" => Some(Self::TextLength),
            "list" => Some(Self::List),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }
}

/// Comparison operators used by numeric, date, time and text-length validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ValidationOperator {
    /// Lower bound <= value <= upper bound.
    Between,
    /// Value is outside the inclusive bounds.
    NotBetween,
    /// Value equals the operand.
    Equal,
    /// Value does not equal the operand.
    NotEqual,
    /// Value is greater than the operand.
    GreaterThan,
    /// Value is less than the operand.
    LessThan,
    /// Value is greater than or equal to the operand.
    GreaterThanOrEqual,
    /// Value is less than or equal to the operand.
    LessThanOrEqual,
}

impl ValidationOperator {
    /// Converts an OOXML `dataValidation/@operator` value into its runtime form.
    pub fn from_ooxml(value: &str) -> Option<Self> {
        match value.trim() {
            "between" => Some(Self::Between),
            "notBetween" => Some(Self::NotBetween),
            "equal" => Some(Self::Equal),
            "notEqual" => Some(Self::NotEqual),
            "greaterThan" => Some(Self::GreaterThan),
            "lessThan" => Some(Self::LessThan),
            "greaterThanOrEqual" => Some(Self::GreaterThanOrEqual),
            "lessThanOrEqual" => Some(Self::LessThanOrEqual),
            _ => None,
        }
    }
}

/// Normalized semantic subset of an OOXML `dataValidation` element.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationRule {
    /// Validation kind.
    pub validation_type: ValidationType,
    /// Comparison operator; Excel defaults to `between` when omitted.
    #[serde(default)]
    pub operator: Option<ValidationOperator>,
    /// Whether an empty candidate bypasses validation.
    #[serde(default)]
    pub allow_blank: bool,
    /// First operand, inline list, range or custom formula.
    #[serde(default)]
    pub formula1: Option<String>,
    /// Second operand for between/not-between rules.
    #[serde(default)]
    pub formula2: Option<String>,
    /// Top-left cell of the rule's first `sqref` area. Relative references in formulas are stored
    /// relative to this coordinate and are translated to the target before evaluation.
    pub anchor: CellAddress,
}

/// Candidate input as entered by a user.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum CandidateValue {
    /// A cleared cell.
    Blank,
    /// A numeric value or Excel serial.
    Number(f64),
    /// Literal text. It is never coerced to a number, date, Boolean or formula.
    Text(String),
    /// Raw text as typed into Excel. Normal number/date/Boolean/formula coercion is applied.
    UserInput(String),
    /// A Boolean value.
    Boolean(bool),
    /// A formula input, including or excluding its leading equals sign.
    Formula(String),
}

impl CandidateValue {
    fn is_blank(&self) -> bool {
        matches!(self, Self::Blank)
            || matches!(self, Self::Text(value) | Self::UserInput(value) if value.is_empty())
    }

    fn user_input(&self) -> String {
        match self {
            Self::Blank => String::new(),
            Self::Number(value) => value.to_string(),
            Self::Text(value) => format!("'{value}"),
            Self::UserInput(value) => value.clone(),
            Self::Boolean(value) => if *value { "TRUE" } else { "FALSE" }.to_string(),
            Self::Formula(value) => {
                if value.starts_with('=') {
                    value.clone()
                } else {
                    format!("={value}")
                }
            }
        }
    }

    fn display_text(&self) -> String {
        match self {
            Self::Blank => String::new(),
            Self::Number(value) => value.to_string(),
            Self::Text(value) | Self::UserInput(value) | Self::Formula(value) => value.clone(),
            Self::Boolean(value) => value.to_string().to_uppercase(),
        }
    }
}

/// A complete validation request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationRequest {
    /// Rule to evaluate.
    pub rule: ValidationRule,
    /// Cell receiving the candidate input.
    pub target: CellAddress,
    /// Proposed input.
    pub candidate: CandidateValue,
}

/// Semantic reason for accepting or rejecting an input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ValidationReason {
    /// The rule imposed no restriction.
    AnyValue,
    /// `allowBlank` accepted an empty input.
    BlankAllowed,
    /// Numeric/date/time/text-length comparison passed.
    ComparisonMatched,
    /// Candidate appeared in the configured list.
    ListMatched,
    /// Custom formula returned TRUE/non-zero.
    CustomFormulaTrue,
    /// Blank was not permitted.
    BlankRejected,
    /// Candidate could not be converted to the rule's required type.
    TypeMismatch,
    /// Comparison failed.
    ComparisonFailed,
    /// Candidate did not appear in the configured list.
    ListMismatch,
    /// Custom formula returned FALSE/zero/empty.
    CustomFormulaFalse,
}

/// Result of evaluating a candidate input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationOutcome {
    /// Whether Excel should accept the candidate.
    pub valid: bool,
    /// Stable machine-readable reason.
    pub reason: ValidationReason,
}

impl ValidationOutcome {
    fn accept(reason: ValidationReason) -> Self {
        Self {
            valid: true,
            reason,
        }
    }

    fn reject(reason: ValidationReason) -> Self {
        Self {
            valid: false,
            reason,
        }
    }
}

/// Validates a candidate against a workbook without mutating the supplied model.
///
/// The clone is serialized from the current workbook, so dependent formulas, defined names,
/// structured references and cross-sheet references are available to the validation formula.
/// OOXML formulas are canonical English and are therefore loaded with the English language.
pub fn validate_candidate(
    model: &UserModel<'_>,
    request: &ValidationRequest,
) -> Result<ValidationOutcome, String> {
    validate_address(request.rule.anchor)?;
    validate_address(request.target)?;
    if request.rule.validation_type == ValidationType::Any {
        return Ok(ValidationOutcome::accept(ValidationReason::AnyValue));
    }
    if request.candidate.is_blank() {
        return Ok(if request.rule.allow_blank {
            ValidationOutcome::accept(ValidationReason::BlankAllowed)
        } else {
            ValidationOutcome::reject(ValidationReason::BlankRejected)
        });
    }

    let bytes = model.to_bytes();
    let mut sandbox = UserModel::from_bytes(&bytes, "en")?;
    sandbox.set_user_input(
        request.target.sheet,
        request.target.row,
        request.target.column,
        &request.candidate.user_input(),
    )?;

    match request.rule.validation_type {
        ValidationType::Any => Ok(ValidationOutcome::accept(ValidationReason::AnyValue)),
        ValidationType::Custom => validate_custom(&mut sandbox, request),
        ValidationType::List => validate_list(&mut sandbox, request),
        kind => validate_comparison(&mut sandbox, request, kind),
    }
}

/// Returns the top-left anchor of the first area in an OOXML `sqref` value.
///
/// Standard worksheet validation `sqref` values are local to their worksheet; `sheet` supplies
/// that zero-based worksheet index. Absolute markers and rectangular ranges are accepted.
pub fn anchor_from_sqref(sheet: u32, sqref: &str) -> Option<CellAddress> {
    let first_area = sqref.split_ascii_whitespace().next()?;
    let first_cell = first_area.split(':').next()?;
    let (row, column) = parse_a1_cell(first_cell)?;
    Some(CellAddress { sheet, row, column })
}

fn validate_address(address: CellAddress) -> Result<(), String> {
    if address.row < 1 || address.row > 1_048_576 {
        return Err(format!("Invalid validation row: {}", address.row));
    }
    if address.column < 1 || address.column > 16_384 {
        return Err(format!("Invalid validation column: {}", address.column));
    }
    Ok(())
}

fn translated_formula(
    sandbox: &mut UserModel<'_>,
    formula: &str,
    anchor: CellAddress,
    target: CellAddress,
) -> Result<String, String> {
    let formula = formula.trim();
    if formula.is_empty() {
        return Err("Data validation formula is empty".into());
    }
    let normalized = if formula.starts_with('=') {
        formula.to_string()
    } else {
        format!("={formula}")
    };
    sandbox.extend_copied_value(
        &normalized,
        &CellReferenceIndex {
            sheet: anchor.sheet,
            row: anchor.row,
            column: anchor.column,
        },
        &CellReferenceIndex {
            sheet: target.sheet,
            row: target.row,
            column: target.column,
        },
    )
}

fn evaluate_formula_value(
    sandbox: &mut UserModel<'_>,
    request: &ValidationRequest,
    formula: &str,
) -> Result<CellValue, String> {
    let formula = translated_formula(sandbox, formula, request.rule.anchor, request.target)?;
    sandbox.evaluate_formula_value_at(
        &formula,
        request.target.sheet,
        request.target.row,
        request.target.column,
    )
}

fn validate_custom(
    sandbox: &mut UserModel<'_>,
    request: &ValidationRequest,
) -> Result<ValidationOutcome, String> {
    let formula = request
        .rule
        .formula1
        .as_deref()
        .ok_or("Custom validation requires formula1")?;
    let valid = match evaluate_formula_value(sandbox, request, formula)? {
        CellValue::Boolean(value) => value,
        CellValue::Number(value) => value != 0.0,
        CellValue::None | CellValue::String(_) => false,
    };
    Ok(if valid {
        ValidationOutcome::accept(ValidationReason::CustomFormulaTrue)
    } else {
        ValidationOutcome::reject(ValidationReason::CustomFormulaFalse)
    })
}

fn validate_comparison(
    sandbox: &mut UserModel<'_>,
    request: &ValidationRequest,
    kind: ValidationType,
) -> Result<ValidationOutcome, String> {
    let candidate = match candidate_metric(sandbox, request, kind)? {
        Some(value) => value,
        None => return Ok(ValidationOutcome::reject(ValidationReason::TypeMismatch)),
    };
    let formula1 = request
        .rule
        .formula1
        .as_deref()
        .ok_or("Data validation comparison requires formula1")?;
    let first = operand_metric(sandbox, request, formula1, kind)?;
    let operator = request.rule.operator.unwrap_or(ValidationOperator::Between);
    let second = if matches!(
        operator,
        ValidationOperator::Between | ValidationOperator::NotBetween
    ) {
        Some(operand_metric(
            sandbox,
            request,
            request
                .rule
                .formula2
                .as_deref()
                .ok_or("Between validation requires formula2")?,
            kind,
        )?)
    } else {
        None
    };
    let matched = compare(candidate, first, second, operator);
    Ok(if matched {
        ValidationOutcome::accept(ValidationReason::ComparisonMatched)
    } else {
        ValidationOutcome::reject(ValidationReason::ComparisonFailed)
    })
}

fn compare(value: f64, first: f64, second: Option<f64>, operator: ValidationOperator) -> bool {
    let equal = |left: f64, right: f64| (left - right).abs() <= f64::EPSILON;
    match operator {
        ValidationOperator::Between => {
            second.is_some_and(|second| value >= first && value <= second)
        }
        ValidationOperator::NotBetween => {
            second.is_some_and(|second| value < first || value > second)
        }
        ValidationOperator::Equal => equal(value, first),
        ValidationOperator::NotEqual => !equal(value, first),
        ValidationOperator::GreaterThan => value > first,
        ValidationOperator::LessThan => value < first,
        ValidationOperator::GreaterThanOrEqual => value >= first,
        ValidationOperator::LessThanOrEqual => value <= first,
    }
}

fn candidate_metric(
    sandbox: &UserModel<'_>,
    request: &ValidationRequest,
    kind: ValidationType,
) -> Result<Option<f64>, String> {
    let evaluated = sandbox.get_model().get_cell_value_by_index(
        request.target.sheet,
        request.target.row,
        request.target.column,
    )?;
    let raw = request.candidate.display_text();
    let value = match kind {
        ValidationType::Whole | ValidationType::Decimal => {
            cell_number(&evaluated).or_else(|| raw.trim().parse::<f64>().ok())
        }
        ValidationType::Date => cell_number(&evaluated)
            .or_else(|| raw.trim().parse::<f64>().ok())
            .or_else(|| parse_excel_date(&raw)),
        ValidationType::Time => cell_number(&evaluated)
            .or_else(|| raw.trim().parse::<f64>().ok())
            .or_else(|| parse_excel_time(&raw)),
        ValidationType::TextLength => {
            Some(cell_text(&evaluated, &raw).encode_utf16().count() as f64)
        }
        _ => None,
    };
    Ok(match (kind, value) {
        (ValidationType::Whole, Some(value)) if value.fract().abs() > f64::EPSILON => None,
        (_, value) => value,
    })
}

fn operand_metric(
    sandbox: &mut UserModel<'_>,
    request: &ValidationRequest,
    formula: &str,
    kind: ValidationType,
) -> Result<f64, String> {
    let trimmed = formula.trim();
    if let Ok(value) = trimmed.parse::<f64>() {
        return Ok(value);
    }
    if kind == ValidationType::Date {
        if let Some(value) = parse_excel_date(trimmed.trim_matches('"')) {
            return Ok(value);
        }
    } else if kind == ValidationType::Time {
        if let Some(value) = parse_excel_time(trimmed.trim_matches('"')) {
            return Ok(value);
        }
    }
    let value = evaluate_formula_value(sandbox, request, formula)?;
    cell_number(&value).ok_or_else(|| format!("Validation operand is not numeric: {formula}"))
}

fn cell_number(value: &CellValue) -> Option<f64> {
    match value {
        CellValue::Number(value) => Some(*value),
        CellValue::Boolean(value) => Some(if *value { 1.0 } else { 0.0 }),
        CellValue::String(value) => value.trim().parse().ok(),
        CellValue::None => None,
    }
}

fn validate_list(
    sandbox: &mut UserModel<'_>,
    request: &ValidationRequest,
) -> Result<ValidationOutcome, String> {
    let formula = request
        .rule
        .formula1
        .as_deref()
        .ok_or("List validation requires formula1")?;
    let items = list_items(sandbox, request, formula)?;
    let candidate = sandbox.get_model().get_cell_value_by_index(
        request.target.sheet,
        request.target.row,
        request.target.column,
    )?;
    let candidate = cell_text(&candidate, &request.candidate.display_text());
    let matched = items
        .iter()
        .any(|item| item.eq_ignore_ascii_case(candidate.trim()));
    Ok(if matched {
        ValidationOutcome::accept(ValidationReason::ListMatched)
    } else {
        ValidationOutcome::reject(ValidationReason::ListMismatch)
    })
}

fn cell_text(value: &CellValue, fallback: &str) -> String {
    match value {
        CellValue::String(value) => value.clone(),
        CellValue::Number(value) => value.to_string(),
        CellValue::Boolean(value) => value.to_string().to_uppercase(),
        CellValue::None => fallback.to_string(),
    }
}

fn list_items(
    sandbox: &mut UserModel<'_>,
    request: &ValidationRequest,
    formula: &str,
) -> Result<Vec<String>, String> {
    let trimmed = formula.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        let body = &trimmed[1..trimmed.len() - 1];
        return Ok(body
            .split([',', ';'])
            .map(|item| item.trim().replace("\"\"", "\""))
            .collect());
    }

    let translated = translated_formula(sandbox, trimmed, request.rule.anchor, request.target)?;
    let values = sandbox.evaluate_formula_values_at(
        &translated,
        request.target.sheet,
        request.target.row,
        request.target.column,
    )?;
    Ok(values
        .iter()
        .flatten()
        .map(|value| cell_text(value, ""))
        .collect())
}

fn parse_a1_cell(value: &str) -> Option<(i32, i32)> {
    let value = value.trim().replace('$', "");
    let split = value.find(|character: char| character.is_ascii_digit())?;
    let (column, row) = value.split_at(split);
    if column.is_empty() || row.is_empty() || !column.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let mut column_number = 0i32;
    for character in column.bytes() {
        column_number = column_number
            .checked_mul(26)?
            .checked_add((character.to_ascii_uppercase() - b'A' + 1) as i32)?;
    }
    let row: i32 = row.parse().ok()?;
    if row < 1 || row > 1_048_576 || column_number < 1 || column_number > 16_384 {
        return None;
    }
    Some((row, column_number))
}

fn parse_excel_date(value: &str) -> Option<f64> {
    let date = value.trim().split(['T', ' ']).next()?;
    let mut parts = date.split('-');
    let year: i32 = parts.next()?.parse().ok()?;
    let month: u32 = parts.next()?.parse().ok()?;
    let day: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !valid_date(year, month, day) {
        return None;
    }
    let epoch = days_from_civil(1899, 12, 31);
    let mut serial = days_from_civil(year, month, day) - epoch;
    if (year, month, day) >= (1900, 3, 1) {
        serial += 1; // Excel's intentional 1900-02-29 compatibility day.
    }
    Some(serial as f64)
}

fn parse_excel_time(value: &str) -> Option<f64> {
    let value = value.trim();
    let time = value
        .split_once('T')
        .map(|(_, time)| time)
        .or_else(|| value.split_once(' ').map(|(_, time)| time))
        .unwrap_or(value);
    let mut parts = time.split(':');
    let hour: u32 = parts.next()?.parse().ok()?;
    let minute: u32 = parts.next()?.parse().ok()?;
    let seconds: f64 = parts.next().unwrap_or("0").parse().ok()?;
    if parts.next().is_some() || hour > 23 || minute > 59 || !(0.0..60.0).contains(&seconds) {
        return None;
    }
    Some((hour as f64 * 3600.0 + minute as f64 * 60.0 + seconds) / 86_400.0)
}

fn valid_date(year: i32, month: u32, day: u32) -> bool {
    if year < 1 || !(1..=12).contains(&month) || day == 0 {
        return false;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let max = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    day <= max
}

// Howard Hinnant's civil calendar conversion, returning days from an arbitrary epoch.
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let year = year - i32::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_prime = month as i32 + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month_prime + 2) / 5 + day as i32 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era as i64 * 146_097 + day_of_era as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use ironcalc::base::types::{Table, TableColumn, TableStyleInfo};
    use std::collections::HashMap;

    fn model() -> UserModel<'static> {
        UserModel::new_empty("validation", "en", "UTC", "en").unwrap()
    }

    fn request(validation_type: ValidationType, candidate: CandidateValue) -> ValidationRequest {
        ValidationRequest {
            rule: ValidationRule {
                validation_type,
                operator: None,
                allow_blank: false,
                formula1: None,
                formula2: None,
                anchor: CellAddress {
                    sheet: 0,
                    row: 1,
                    column: 1,
                },
            },
            target: CellAddress {
                sheet: 0,
                row: 1,
                column: 1,
            },
            candidate,
        }
    }

    #[test]
    fn whole_decimal_and_all_comparison_shapes() {
        let model = model();
        let mut whole = request(ValidationType::Whole, CandidateValue::Number(5.0));
        whole.rule.formula1 = Some("1".into());
        whole.rule.formula2 = Some("5".into());
        assert!(validate_candidate(&model, &whole).unwrap().valid);
        whole.candidate = CandidateValue::Number(5.5);
        assert_eq!(
            validate_candidate(&model, &whole).unwrap().reason,
            ValidationReason::TypeMismatch
        );

        let mut decimal = request(ValidationType::Decimal, CandidateValue::Number(5.5));
        decimal.rule.formula1 = Some("5".into());
        decimal.rule.operator = Some(ValidationOperator::GreaterThan);
        assert!(validate_candidate(&model, &decimal).unwrap().valid);
        decimal.rule.operator = Some(ValidationOperator::NotEqual);
        assert!(validate_candidate(&model, &decimal).unwrap().valid);
    }

    #[test]
    fn blank_and_text_length_follow_excel_semantics() {
        let model = model();
        let mut blank = request(ValidationType::Whole, CandidateValue::Blank);
        blank.rule.allow_blank = true;
        assert_eq!(
            validate_candidate(&model, &blank).unwrap().reason,
            ValidationReason::BlankAllowed
        );
        blank.rule.allow_blank = false;
        assert!(!validate_candidate(&model, &blank).unwrap().valid);

        let mut length = request(
            ValidationType::TextLength,
            CandidateValue::Text("A😀".into()),
        );
        length.rule.formula1 = Some("3".into());
        length.rule.operator = Some(ValidationOperator::Equal);
        assert!(validate_candidate(&model, &length).unwrap().valid);
    }

    #[test]
    fn date_time_accept_iso_and_excel_serials() {
        let model = model();
        let mut date = request(
            ValidationType::Date,
            CandidateValue::UserInput("2024-02-29".into()),
        );
        date.rule.formula1 = Some("2024-01-01".into());
        date.rule.operator = Some(ValidationOperator::GreaterThan);
        assert!(validate_candidate(&model, &date).unwrap().valid);

        let mut time = request(
            ValidationType::Time,
            CandidateValue::UserInput("12:30:00".into()),
        );
        time.rule.formula1 = Some("0.5".into());
        time.rule.operator = Some(ValidationOperator::GreaterThan);
        assert!(validate_candidate(&model, &time).unwrap().valid);
        assert_eq!(parse_excel_date("1900-03-01"), Some(61.0));
    }

    #[test]
    fn inline_and_cross_sheet_range_lists_work() {
        let mut model = model();
        let mut inline = request(ValidationType::List, CandidateValue::Text("Green".into()));
        inline.rule.formula1 = Some("\"Red,Green,Blue\"".into());
        assert!(validate_candidate(&model, &inline).unwrap().valid);

        model.new_sheet().unwrap();
        model.rename_sheet(1, "Lookup Data").unwrap();
        model.set_user_input(1, 1, 1, "North").unwrap();
        model.set_user_input(1, 2, 1, "South").unwrap();
        let mut ranged = request(ValidationType::List, CandidateValue::Text("south".into()));
        ranged.rule.formula1 = Some("='Lookup Data'!$A$1:$A$2".into());
        assert!(validate_candidate(&model, &ranged).unwrap().valid);
    }

    #[test]
    fn named_spill_and_structured_reference_lists_expand_through_ironcalc() {
        let mut named_model = model();
        named_model.new_sheet().unwrap();
        named_model.rename_sheet(1, "Lookup").unwrap();
        named_model.set_user_input(1, 1, 1, "East").unwrap();
        named_model.set_user_input(1, 2, 1, "West").unwrap();
        named_model
            .new_defined_name("AllowedRegions", None, "Lookup!$A$1:$A$2")
            .unwrap();
        let mut named = request(ValidationType::List, CandidateValue::Text("West".into()));
        named.target = CellAddress {
            sheet: 0,
            row: 1,
            column: 3,
        };
        named.rule.anchor = named.target;
        named.rule.formula1 = Some("=AllowedRegions".into());
        let before_named = named_model.to_bytes();
        assert!(validate_candidate(&named_model, &named).unwrap().valid);
        assert_eq!(named_model.to_bytes(), before_named);

        let mut spill_model = model();
        spill_model.set_user_input(0, 5, 1, "=SEQUENCE(3)").unwrap();
        let mut spill = request(ValidationType::List, CandidateValue::Number(2.0));
        spill.target = CellAddress {
            sheet: 0,
            row: 1,
            column: 3,
        };
        spill.rule.anchor = spill.target;
        spill.rule.formula1 = Some("=$A$5#".into());
        let before_spill = spill_model.to_bytes();
        assert!(validate_candidate(&spill_model, &spill).unwrap().valid);
        assert_eq!(spill_model.to_bytes(), before_spill);

        let mut table_model = model();
        table_model.set_user_input(0, 1, 1, "City").unwrap();
        table_model.set_user_input(0, 2, 1, "Singapore").unwrap();
        table_model.set_user_input(0, 3, 1, "Shanghai").unwrap();
        table_model.replace_tables(HashMap::from([(
            "CitiesTable".to_string(),
            Table {
                name: "CitiesTable".into(),
                display_name: "CitiesTable".into(),
                sheet_name: "Sheet1".into(),
                reference: "A1:A3".into(),
                totals_row_count: 0,
                header_row_count: 1,
                header_row_dxf_id: None,
                data_dxf_id: None,
                totals_row_dxf_id: None,
                columns: vec![TableColumn {
                    id: 1,
                    name: "City".into(),
                    ..TableColumn::default()
                }],
                style_info: TableStyleInfo::default(),
                has_filters: true,
            },
        )]));
        let mut structured = request(
            ValidationType::List,
            CandidateValue::Text("Shanghai".into()),
        );
        structured.target = CellAddress {
            sheet: 0,
            row: 1,
            column: 3,
        };
        structured.rule.anchor = structured.target;
        structured.rule.formula1 = Some("=CitiesTable[City]".into());
        let before_table = table_model.to_bytes();
        assert!(validate_candidate(&table_model, &structured).unwrap().valid);
        assert_eq!(table_model.to_bytes(), before_table);
    }

    #[test]
    fn custom_formula_sees_candidate_without_mutating_original() {
        let mut model = model();
        model.set_user_input(0, 1, 1, "old").unwrap();
        let before = model.to_bytes();
        let mut custom = request(ValidationType::Custom, CandidateValue::Text("abc".into()));
        custom.rule.formula1 = Some("LEN(A1)=3".into());
        assert!(validate_candidate(&model, &custom).unwrap().valid);
        custom.candidate = CandidateValue::Text("abcd".into());
        assert!(!validate_candidate(&model, &custom).unwrap().valid);
        assert_eq!(model.to_bytes(), before);
        assert_eq!(model.get_cell_content(0, 1, 1).unwrap(), "old");
    }

    #[test]
    fn relative_absolute_and_cross_sheet_custom_references_shift_like_excel() {
        let mut model = model();
        model.new_sheet().unwrap();
        model.rename_sheet(1, "Rules").unwrap();
        model.set_user_input(1, 1, 1, "10").unwrap();
        model.set_user_input(0, 2, 1, "7").unwrap();
        let mut custom = request(ValidationType::Custom, CandidateValue::Number(8.0));
        custom.rule.anchor = CellAddress {
            sheet: 0,
            row: 1,
            column: 1,
        };
        custom.target = CellAddress {
            sheet: 0,
            row: 2,
            column: 2,
        };
        // A1 shifts to B2 (the candidate), $A2 keeps its absolute column and shifts row,
        // and the absolute cross-sheet reference remains fixed.
        custom.rule.formula1 = Some("AND(A1>$A1,$A1<'Rules'!$A$1)".into());
        assert!(validate_candidate(&model, &custom).unwrap().valid);
    }

    #[test]
    fn custom_formula_recalculates_candidate_dependents() {
        let mut model = model();
        model.set_user_input(0, 1, 2, "=A1*2").unwrap();
        let mut custom = request(ValidationType::Custom, CandidateValue::Number(6.0));
        custom.rule.formula1 = Some("B1=12".into());
        assert!(validate_candidate(&model, &custom).unwrap().valid);
        custom.candidate = CandidateValue::Number(5.0);
        assert!(!validate_candidate(&model, &custom).unwrap().valid);
    }
}
