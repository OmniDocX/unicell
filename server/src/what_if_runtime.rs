//! Runtime support for Excel-style What-If Analysis.
//!
//! Calculations always run against an in-memory IronCalc clone.  Callers can inspect a preview
//! without touching the live workbook and, after accepting it, apply the returned cell writes as
//! one application transaction.  This module intentionally does not claim to be an optimization
//! solver: it implements Goal Seek, one/two-input data tables and Scenario Manager semantics.

use ironcalc::base::{UserModel, cell::CellValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

const MAX_ITERATIONS: usize = 1_000;
const MAX_DATA_TABLE_CELLS: usize = 100_000;
const MAX_SCENARIOS: usize = 1_024;
const MAX_SCENARIO_CHANGES: usize = 32;

/// One-based worksheet address with a zero-based worksheet index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CellAddress {
    pub sheet: u32,
    pub row: i32,
    pub column: i32,
}

impl CellAddress {
    fn validate(self) -> Result<(), String> {
        if !(1..=1_048_576).contains(&self.row) {
            return Err(format!("row {} is outside Excel's worksheet", self.row));
        }
        if !(1..=16_384).contains(&self.column) {
            return Err(format!(
                "column {} is outside Excel's worksheet",
                self.column
            ));
        }
        Ok(())
    }
}

/// A raw input that can be committed with `UserModel::set_user_input`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CellWrite {
    pub cell: CellAddress,
    pub input: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GoalSeekStatus {
    Converged,
    NoConvergence,
    EvaluationError,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalSeekTracePoint {
    pub input: f64,
    pub output: Option<f64>,
    pub residual: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn default_goal_iterations() -> usize {
    100
}

fn default_goal_tolerance() -> f64 {
    1e-8
}

/// Request for Goal Seek. Bounds are optional; when present they are never crossed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalSeekRequest {
    pub target: CellAddress,
    pub changing: CellAddress,
    pub target_value: f64,
    #[serde(default)]
    pub initial_value: Option<f64>,
    #[serde(default)]
    pub lower_bound: Option<f64>,
    #[serde(default)]
    pub upper_bound: Option<f64>,
    #[serde(default)]
    pub initial_step: Option<f64>,
    #[serde(default = "default_goal_iterations")]
    pub max_iterations: usize,
    #[serde(default = "default_goal_tolerance")]
    pub tolerance: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalSeekOutcome {
    pub status: GoalSeekStatus,
    pub converged: bool,
    pub iterations: usize,
    pub evaluations: usize,
    pub initial_value: f64,
    pub result_value: f64,
    pub achieved_value: Option<f64>,
    pub residual: Option<f64>,
    pub tolerance: f64,
    pub message: String,
    pub trace: Vec<GoalSeekTracePoint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub write: Option<CellWrite>,
}

#[derive(Debug, Clone, Copy)]
struct Point {
    x: f64,
    y: f64,
    f: f64,
}

struct GoalEvaluator {
    sandbox: UserModel<'static>,
    request: GoalSeekRequest,
    evaluations: usize,
    trace: Vec<GoalSeekTracePoint>,
}

impl GoalEvaluator {
    fn evaluate(&mut self, x: f64) -> Result<Point, String> {
        if !x.is_finite() {
            return Err("candidate input is not finite".into());
        }
        self.evaluations += 1;
        self.sandbox.set_user_input(
            self.request.changing.sheet,
            self.request.changing.row,
            self.request.changing.column,
            &number_input(x),
        )?;
        let result = self.sandbox.get_model().get_cell_value_by_index(
            self.request.target.sheet,
            self.request.target.row,
            self.request.target.column,
        )?;
        let y = match result {
            CellValue::Number(value) if value.is_finite() => value,
            CellValue::Number(_) => return self.fail(x, "target result is not finite"),
            CellValue::String(value) => {
                return self.fail(x, &format!("target did not produce a number: {value}"));
            }
            CellValue::Boolean(value) => {
                return self.fail(x, &format!("target produced Boolean {value}, not a number"));
            }
            CellValue::None => return self.fail(x, "target result is blank"),
        };
        let f = y - self.request.target_value;
        if !f.is_finite() {
            return self.fail(x, "target residual is not finite");
        }
        if self.trace.len() < 160 {
            self.trace.push(GoalSeekTracePoint {
                input: x,
                output: Some(y),
                residual: Some(f),
                error: None,
            });
        }
        Ok(Point { x, y, f })
    }

    fn fail<T>(&mut self, x: f64, message: &str) -> Result<T, String> {
        if self.trace.len() < 160 {
            self.trace.push(GoalSeekTracePoint {
                input: x,
                output: None,
                residual: None,
                error: Some(message.to_string()),
            });
        }
        Err(message.to_string())
    }
}

fn number_input(value: f64) -> String {
    if value == 0.0 {
        "0".to_string()
    } else {
        value.to_string()
    }
}

fn numeric_cell(model: &UserModel<'_>, cell: CellAddress) -> Result<f64, String> {
    match model
        .get_model()
        .get_cell_value_by_index(cell.sheet, cell.row, cell.column)?
    {
        CellValue::Number(value) if value.is_finite() => Ok(value),
        CellValue::None => Ok(0.0),
        CellValue::String(value) => value
            .trim()
            .parse::<f64>()
            .map_err(|_| "changing cell must contain a number or be blank".into()),
        CellValue::Boolean(value) => Ok(if value { 1.0 } else { 0.0 }),
        CellValue::Number(_) => Err("changing cell is not finite".into()),
    }
}

fn within_bounds(value: f64, lower: Option<f64>, upper: Option<f64>) -> bool {
    lower.is_none_or(|bound| value >= bound) && upper.is_none_or(|bound| value <= bound)
}

fn clamp_bounds(value: f64, lower: Option<f64>, upper: Option<f64>) -> f64 {
    let value = lower.map_or(value, |bound| value.max(bound));
    upper.map_or(value, |bound| value.min(bound))
}

fn opposite_sign(left: f64, right: f64) -> bool {
    left == 0.0 || right == 0.0 || left.is_sign_positive() != right.is_sign_positive()
}

fn converged_outcome(
    evaluator: GoalEvaluator,
    point: Point,
    initial_value: f64,
    iterations: usize,
) -> GoalSeekOutcome {
    GoalSeekOutcome {
        status: GoalSeekStatus::Converged,
        converged: true,
        iterations,
        evaluations: evaluator.evaluations,
        initial_value,
        result_value: point.x,
        achieved_value: Some(point.y),
        residual: Some(point.f),
        tolerance: evaluator.request.tolerance,
        message: "Goal Seek converged".into(),
        trace: evaluator.trace,
        write: Some(CellWrite {
            cell: evaluator.request.changing,
            input: number_input(point.x),
        }),
    }
}

fn failure_outcome(
    evaluator: GoalEvaluator,
    status: GoalSeekStatus,
    best: Option<Point>,
    initial_value: f64,
    iterations: usize,
    message: String,
) -> GoalSeekOutcome {
    GoalSeekOutcome {
        status,
        converged: false,
        iterations,
        evaluations: evaluator.evaluations,
        initial_value,
        result_value: best.map_or(initial_value, |point| point.x),
        achieved_value: best.map(|point| point.y),
        residual: best.map(|point| point.f),
        tolerance: evaluator.request.tolerance,
        message,
        trace: evaluator.trace,
        write: None,
    }
}

/// Runs Goal Seek against an isolated model clone.
///
/// The algorithm uses a safeguarded secant step and switches to bisection whenever a bracket is
/// known but the secant step is unsafe. Without a bracket it expands deterministically until it
/// either finds one or exhausts the configured iteration limit.
pub fn goal_seek(
    model: &UserModel<'_>,
    request: &GoalSeekRequest,
) -> Result<GoalSeekOutcome, String> {
    request.target.validate()?;
    request.changing.validate()?;
    if request.target == request.changing {
        return Err("target and changing cell must be different".into());
    }
    if !request.target_value.is_finite() {
        return Err("targetValue must be finite".into());
    }
    if request.max_iterations == 0 || request.max_iterations > MAX_ITERATIONS {
        return Err(format!(
            "maxIterations must be between 1 and {MAX_ITERATIONS}"
        ));
    }
    if !request.tolerance.is_finite() || request.tolerance <= 0.0 {
        return Err("tolerance must be a positive finite number".into());
    }
    if request.lower_bound.is_some_and(|value| !value.is_finite())
        || request.upper_bound.is_some_and(|value| !value.is_finite())
    {
        return Err("Goal Seek bounds must be finite".into());
    }
    if let (Some(lower), Some(upper)) = (request.lower_bound, request.upper_bound) {
        if lower >= upper {
            return Err("lowerBound must be less than upperBound".into());
        }
    }
    let target_content = model.get_cell_content(
        request.target.sheet,
        request.target.row,
        request.target.column,
    )?;
    if !target_content.trim_start().starts_with('=') {
        return Err("Goal Seek target must be a formula cell".into());
    }
    let changing_content = model.get_cell_content(
        request.changing.sheet,
        request.changing.row,
        request.changing.column,
    )?;
    if changing_content.trim_start().starts_with('=') {
        return Err("Goal Seek changing cell cannot contain a formula".into());
    }

    let initial_value = request
        .initial_value
        .unwrap_or(numeric_cell(model, request.changing)?);
    if !initial_value.is_finite()
        || !within_bounds(initial_value, request.lower_bound, request.upper_bound)
    {
        return Err("initialValue must be finite and inside the configured bounds".into());
    }
    let mut evaluator = GoalEvaluator {
        sandbox: UserModel::from_bytes(&model.to_bytes(), "en")?,
        request: request.clone(),
        evaluations: 0,
        trace: Vec::new(),
    };

    let first = match evaluator.evaluate(initial_value) {
        Ok(point) => point,
        Err(message) => {
            return Ok(failure_outcome(
                evaluator,
                GoalSeekStatus::EvaluationError,
                None,
                initial_value,
                0,
                message,
            ));
        }
    };
    if first.f.abs() <= request.tolerance {
        return Ok(converged_outcome(evaluator, first, initial_value, 0));
    }

    let mut best = first;
    let mut bracket: Option<(Point, Point)> = None;
    let mut step = request
        .initial_step
        .unwrap_or_else(|| (initial_value.abs() * 0.05).max(1.0));
    if !step.is_finite() || step == 0.0 {
        return Err("initialStep must be a non-zero finite number".into());
    }
    step = step.abs();

    // Prefer explicit bounds as a reliable initial bracket.
    if let (Some(lower), Some(upper)) = (request.lower_bound, request.upper_bound) {
        let lower_point = if lower == first.x {
            first
        } else {
            match evaluator.evaluate(lower) {
                Ok(point) => point,
                Err(message) => {
                    return Ok(failure_outcome(
                        evaluator,
                        GoalSeekStatus::EvaluationError,
                        Some(best),
                        initial_value,
                        0,
                        message,
                    ));
                }
            }
        };
        if lower_point.f.abs() < best.f.abs() {
            best = lower_point;
        }
        if lower_point.f.abs() <= request.tolerance {
            return Ok(converged_outcome(evaluator, lower_point, initial_value, 0));
        }
        let upper_point = if upper == first.x {
            first
        } else {
            match evaluator.evaluate(upper) {
                Ok(point) => point,
                Err(message) => {
                    return Ok(failure_outcome(
                        evaluator,
                        GoalSeekStatus::EvaluationError,
                        Some(best),
                        initial_value,
                        0,
                        message,
                    ));
                }
            }
        };
        if upper_point.f.abs() < best.f.abs() {
            best = upper_point;
        }
        if upper_point.f.abs() <= request.tolerance {
            return Ok(converged_outcome(evaluator, upper_point, initial_value, 0));
        }
        if opposite_sign(lower_point.f, upper_point.f) {
            bracket = Some((lower_point, upper_point));
        }
    }

    let second_x = if bracket.is_some() {
        best.x
    } else {
        let plus = clamp_bounds(
            initial_value + step,
            request.lower_bound,
            request.upper_bound,
        );
        if (plus - initial_value).abs() > f64::EPSILON {
            plus
        } else {
            clamp_bounds(
                initial_value - step,
                request.lower_bound,
                request.upper_bound,
            )
        }
    };
    let mut previous = first;
    let mut current = if bracket.is_some() || second_x == first.x {
        best
    } else {
        match evaluator.evaluate(second_x) {
            Ok(point) => point,
            Err(message) => {
                return Ok(failure_outcome(
                    evaluator,
                    GoalSeekStatus::EvaluationError,
                    Some(best),
                    initial_value,
                    0,
                    message,
                ));
            }
        }
    };
    if current.f.abs() < best.f.abs() {
        best = current;
    }
    if current.f.abs() <= request.tolerance {
        return Ok(converged_outcome(evaluator, current, initial_value, 0));
    }
    if bracket.is_none() && current.x != previous.x && opposite_sign(previous.f, current.f) {
        bracket = Some(if previous.x < current.x {
            (previous, current)
        } else {
            (current, previous)
        });
    }

    for iteration in 1..=request.max_iterations {
        let candidate = if let Some((left, right)) = bracket {
            let denominator = right.f - left.f;
            let secant = if denominator.abs() > f64::EPSILON {
                right.x - right.f * (right.x - left.x) / denominator
            } else {
                f64::NAN
            };
            let width = right.x - left.x;
            if secant.is_finite()
                && secant > left.x + width.abs() * 0.05
                && secant < right.x - width.abs() * 0.05
            {
                secant
            } else {
                (left.x + right.x) * 0.5
            }
        } else {
            let denominator = current.f - previous.f;
            let secant = if denominator.abs() > f64::EPSILON {
                current.x - current.f * (current.x - previous.x) / denominator
            } else {
                f64::NAN
            };
            if secant.is_finite()
                && within_bounds(secant, request.lower_bound, request.upper_bound)
                && (secant - current.x).abs() > f64::EPSILON
                && (secant - current.x).abs() <= step * 32.0
            {
                secant
            } else {
                let direction = if iteration % 2 == 0 { -1.0 } else { 1.0 };
                clamp_bounds(
                    initial_value + direction * step,
                    request.lower_bound,
                    request.upper_bound,
                )
            }
        };

        let x_tolerance = f64::EPSILON.sqrt() * (1.0 + candidate.abs());
        if !candidate.is_finite()
            || (candidate - current.x).abs() <= x_tolerance
                && bracket.is_none()
                && current.f.abs() > request.tolerance
        {
            step *= 2.0;
            continue;
        }
        let next = match evaluator.evaluate(candidate) {
            Ok(point) => point,
            Err(message) => {
                return Ok(failure_outcome(
                    evaluator,
                    GoalSeekStatus::EvaluationError,
                    Some(best),
                    initial_value,
                    iteration,
                    message,
                ));
            }
        };
        if next.f.abs() < best.f.abs() {
            best = next;
        }
        if next.f.abs() <= request.tolerance {
            return Ok(converged_outcome(evaluator, next, initial_value, iteration));
        }

        if let Some((left, right)) = bracket {
            bracket = Some(if opposite_sign(left.f, next.f) {
                if left.x < next.x {
                    (left, next)
                } else {
                    (next, left)
                }
            } else if right.x < next.x {
                (right, next)
            } else {
                (next, right)
            });
        } else if opposite_sign(current.f, next.f) {
            bracket = Some(if current.x < next.x {
                (current, next)
            } else {
                (next, current)
            });
        } else if opposite_sign(previous.f, next.f) {
            bracket = Some(if previous.x < next.x {
                (previous, next)
            } else {
                (next, previous)
            });
        }
        previous = current;
        current = next;
        if bracket.is_none() {
            step *= 1.6;
        }
    }

    Ok(failure_outcome(
        evaluator,
        GoalSeekStatus::NoConvergence,
        Some(best),
        initial_value,
        request.max_iterations,
        format!(
            "Goal Seek did not converge within {} iterations; best residual was {:.6e}",
            request.max_iterations, best.f
        ),
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TableOrientation {
    Row,
    Column,
}

/// One- and two-variable data tables. The output is a snapshot table, not a hidden Solver model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum DataTableRequest {
    OneVariable {
        formula_cell: CellAddress,
        input_cell: CellAddress,
        values: Vec<Value>,
        orientation: TableOrientation,
        output: CellAddress,
    },
    TwoVariable {
        formula_cell: CellAddress,
        row_input_cell: CellAddress,
        column_input_cell: CellAddress,
        row_values: Vec<Value>,
        column_values: Vec<Value>,
        output: CellAddress,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DataTableOutcome {
    pub rows: usize,
    pub columns: usize,
    pub evaluations: usize,
    pub matrix: Vec<Vec<Value>>,
    pub writes: Vec<CellWrite>,
    pub dynamic: bool,
    pub message: String,
}

fn scalar_input(value: &Value) -> Result<String, String> {
    match value {
        Value::Null => Ok(String::new()),
        Value::Bool(value) => Ok(if *value { "TRUE" } else { "FALSE" }.into()),
        Value::Number(value) => Ok(value.to_string()),
        Value::String(value) => Ok(value.clone()),
        _ => Err("data table assumptions must be scalar values".into()),
    }
}

fn cell_value_json(value: CellValue) -> Value {
    match value {
        CellValue::None => Value::Null,
        CellValue::String(value) => Value::String(value),
        CellValue::Number(value) => serde_json::Number::from_f64(value)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        CellValue::Boolean(value) => Value::Bool(value),
    }
}

fn column_name(mut column: i32) -> String {
    let mut result = String::new();
    while column > 0 {
        let digit = ((column - 1) % 26) as u8;
        result.insert(0, (b'A' + digit) as char);
        column = (column - 1) / 26;
    }
    result
}

fn formula_link(model: &UserModel<'_>, cell: CellAddress) -> Result<String, String> {
    let sheet = model
        .get_worksheets_properties()
        .get(cell.sheet as usize)
        .map(|sheet| sheet.name.clone())
        .ok_or_else(|| format!("invalid sheet index {}", cell.sheet))?;
    let sheet = sheet.replace('\'', "''");
    Ok(format!(
        "='{}'!${}${}",
        sheet,
        column_name(cell.column),
        cell.row
    ))
}

fn evaluate_target(model: &UserModel<'_>, cell: CellAddress) -> Result<Value, String> {
    Ok(cell_value_json(model.get_model().get_cell_value_by_index(
        cell.sheet,
        cell.row,
        cell.column,
    )?))
}

fn ensure_formula_cell(model: &UserModel<'_>, cell: CellAddress) -> Result<(), String> {
    cell.validate()?;
    if !model
        .get_cell_content(cell.sheet, cell.row, cell.column)?
        .trim_start()
        .starts_with('=')
    {
        return Err("data table formulaCell must contain a formula".into());
    }
    Ok(())
}

fn output_contains(output: CellAddress, rows: usize, columns: usize, cell: CellAddress) -> bool {
    cell.sheet == output.sheet
        && cell.row >= output.row
        && cell.column >= output.column
        && cell.row < output.row.saturating_add(rows as i32)
        && cell.column < output.column.saturating_add(columns as i32)
}

fn push_write(
    writes: &mut Vec<CellWrite>,
    matrix: &mut [Vec<Value>],
    output: CellAddress,
    row_offset: usize,
    column_offset: usize,
    input: String,
    displayed: Value,
) -> Result<(), String> {
    let cell = CellAddress {
        sheet: output.sheet,
        row: output
            .row
            .checked_add(row_offset as i32)
            .ok_or("data table output row overflow")?,
        column: output
            .column
            .checked_add(column_offset as i32)
            .ok_or("data table output column overflow")?,
    };
    cell.validate()?;
    matrix[row_offset][column_offset] = displayed;
    writes.push(CellWrite { cell, input });
    Ok(())
}

/// Calculates an Excel-shaped one/two-variable data-table snapshot in a sandbox.
pub fn data_table(
    model: &UserModel<'_>,
    request: &DataTableRequest,
) -> Result<DataTableOutcome, String> {
    let mut sandbox = UserModel::from_bytes(&model.to_bytes(), "en")?;
    let mut writes = Vec::new();
    let (rows, columns, formula_cell, output) = match request {
        DataTableRequest::OneVariable {
            formula_cell,
            input_cell,
            values,
            orientation,
            output,
        } => {
            formula_cell.validate()?;
            input_cell.validate()?;
            output.validate()?;
            if values.is_empty() {
                return Err("one-variable data table requires at least one value".into());
            }
            let shape = match orientation {
                TableOrientation::Column => (values.len() + 1, 2),
                TableOrientation::Row => (2, values.len() + 1),
            };
            (shape.0, shape.1, *formula_cell, *output)
        }
        DataTableRequest::TwoVariable {
            formula_cell,
            row_input_cell,
            column_input_cell,
            row_values,
            column_values,
            output,
        } => {
            formula_cell.validate()?;
            row_input_cell.validate()?;
            column_input_cell.validate()?;
            output.validate()?;
            if row_input_cell == column_input_cell {
                return Err("two-variable data table needs two different input cells".into());
            }
            if row_values.is_empty() || column_values.is_empty() {
                return Err("two-variable data table requires rowValues and columnValues".into());
            }
            (
                column_values.len() + 1,
                row_values.len() + 1,
                *formula_cell,
                *output,
            )
        }
    };
    ensure_formula_cell(model, formula_cell)?;
    if rows.saturating_mul(columns) > MAX_DATA_TABLE_CELLS {
        return Err(format!(
            "data table exceeds the {MAX_DATA_TABLE_CELLS}-cell safety limit"
        ));
    }
    let mut matrix = vec![vec![Value::Null; columns]; rows];
    match request {
        DataTableRequest::OneVariable { input_cell, .. } => {
            if output_contains(output, rows, columns, *input_cell) {
                return Err("data table output cannot overwrite its input cell".into());
            }
        }
        DataTableRequest::TwoVariable {
            row_input_cell,
            column_input_cell,
            ..
        } => {
            if output_contains(output, rows, columns, *row_input_cell)
                || output_contains(output, rows, columns, *column_input_cell)
            {
                return Err("data table output cannot overwrite either input cell".into());
            }
        }
    }
    if output_contains(output, rows, columns, formula_cell) && output != formula_cell {
        return Err(
            "formulaCell may be outside the output or exactly at its top-left corner".into(),
        );
    }
    let header_input = if output == formula_cell {
        model.get_cell_content(formula_cell.sheet, formula_cell.row, formula_cell.column)?
    } else {
        formula_link(model, formula_cell)?
    };
    push_write(
        &mut writes,
        &mut matrix,
        output,
        0,
        0,
        header_input,
        evaluate_target(model, formula_cell)?,
    )?;
    let mut evaluations = 0usize;

    match request {
        DataTableRequest::OneVariable {
            formula_cell,
            input_cell,
            values,
            orientation,
            ..
        } => {
            for (index, value) in values.iter().enumerate() {
                let input = scalar_input(value)?;
                sandbox.set_user_input(
                    input_cell.sheet,
                    input_cell.row,
                    input_cell.column,
                    &input,
                )?;
                evaluations += 1;
                let result = evaluate_target(&sandbox, *formula_cell)?;
                match orientation {
                    TableOrientation::Column => {
                        push_write(
                            &mut writes,
                            &mut matrix,
                            output,
                            index + 1,
                            0,
                            input,
                            value.clone(),
                        )?;
                        push_write(
                            &mut writes,
                            &mut matrix,
                            output,
                            index + 1,
                            1,
                            scalar_input(&result)?,
                            result,
                        )?;
                    }
                    TableOrientation::Row => {
                        push_write(
                            &mut writes,
                            &mut matrix,
                            output,
                            0,
                            index + 1,
                            input,
                            value.clone(),
                        )?;
                        push_write(
                            &mut writes,
                            &mut matrix,
                            output,
                            1,
                            index + 1,
                            scalar_input(&result)?,
                            result,
                        )?;
                    }
                }
            }
        }
        DataTableRequest::TwoVariable {
            formula_cell,
            row_input_cell,
            column_input_cell,
            row_values,
            column_values,
            ..
        } => {
            for (column, value) in row_values.iter().enumerate() {
                push_write(
                    &mut writes,
                    &mut matrix,
                    output,
                    0,
                    column + 1,
                    scalar_input(value)?,
                    value.clone(),
                )?;
            }
            for (row, value) in column_values.iter().enumerate() {
                push_write(
                    &mut writes,
                    &mut matrix,
                    output,
                    row + 1,
                    0,
                    scalar_input(value)?,
                    value.clone(),
                )?;
                for (column, row_value) in row_values.iter().enumerate() {
                    sandbox.set_user_input(
                        row_input_cell.sheet,
                        row_input_cell.row,
                        row_input_cell.column,
                        &scalar_input(row_value)?,
                    )?;
                    sandbox.set_user_input(
                        column_input_cell.sheet,
                        column_input_cell.row,
                        column_input_cell.column,
                        &scalar_input(value)?,
                    )?;
                    evaluations += 1;
                    let result = evaluate_target(&sandbox, *formula_cell)?;
                    push_write(
                        &mut writes,
                        &mut matrix,
                        output,
                        row + 1,
                        column + 1,
                        scalar_input(&result)?,
                        result,
                    )?;
                }
            }
        }
    }

    Ok(DataTableOutcome {
        rows,
        columns,
        evaluations,
        matrix,
        writes,
        // IronCalc presently commits the calculated snapshot. `false` is explicit so callers do
        // not confuse it with OOXML's special `t="dataTable"` recalculation formula.
        dynamic: false,
        message: "Calculated in an isolated workbook and ready for atomic snapshot write".into(),
    })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioChange {
    pub cell: CellAddress,
    pub input: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Scenario {
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub comment: String,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub locked: bool,
    pub changes: Vec<ScenarioChange>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioStore {
    #[serde(default)]
    next_id: u64,
    #[serde(default)]
    scenarios: Vec<Scenario>,
    #[serde(default)]
    dirty: bool,
}

impl ScenarioStore {
    pub fn list(&self) -> &[Scenario] {
        &self.scenarios
    }

    pub fn get(&self, id: &str) -> Option<&Scenario> {
        self.scenarios.iter().find(|scenario| scenario.id == id)
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    fn validate(&self, scenario: &Scenario, replacing: Option<&str>) -> Result<(), String> {
        let name = scenario.name.trim();
        if name.is_empty() || name.chars().count() > 255 {
            return Err("scenario name must contain 1 to 255 characters".into());
        }
        if scenario.changes.is_empty() || scenario.changes.len() > MAX_SCENARIO_CHANGES {
            return Err(format!(
                "a scenario must contain 1 to {MAX_SCENARIO_CHANGES} changing cells"
            ));
        }
        let mut seen = std::collections::HashSet::new();
        let mut scenario_sheet = None;
        for change in &scenario.changes {
            change.cell.validate()?;
            match scenario_sheet {
                None => scenario_sheet = Some(change.cell.sheet),
                Some(sheet) if sheet != change.cell.sheet => {
                    return Err(
                        "Excel scenarios cannot contain changing cells from different worksheets"
                            .into(),
                    );
                }
                _ => {}
            }
            if !seen.insert(change.cell) {
                return Err("a scenario cannot list the same changing cell twice".into());
            }
        }
        if self.scenarios.iter().any(|existing| {
            Some(existing.id.as_str()) != replacing && existing.name.eq_ignore_ascii_case(name)
        }) {
            return Err(format!("scenario name already exists: {name}"));
        }
        Ok(())
    }

    pub fn create(&mut self, mut scenario: Scenario) -> Result<Scenario, String> {
        if self.scenarios.len() >= MAX_SCENARIOS {
            return Err(format!(
                "workbook exceeds the {MAX_SCENARIOS}-scenario safety limit"
            ));
        }
        self.validate(&scenario, None)?;
        self.next_id = self.next_id.saturating_add(1).max(1);
        scenario.id = format!("scenario-{}", self.next_id);
        scenario.name = scenario.name.trim().to_string();
        self.scenarios.push(scenario.clone());
        self.dirty = true;
        Ok(scenario)
    }

    pub fn update(&mut self, id: &str, mut scenario: Scenario) -> Result<Scenario, String> {
        let index = self
            .scenarios
            .iter()
            .position(|scenario| scenario.id == id)
            .ok_or_else(|| format!("scenario not found: {id}"))?;
        self.validate(&scenario, Some(id))?;
        scenario.id = id.to_string();
        scenario.name = scenario.name.trim().to_string();
        self.scenarios[index] = scenario.clone();
        self.dirty = true;
        Ok(scenario)
    }

    pub fn delete(&mut self, id: &str) -> Result<Scenario, String> {
        let index = self
            .scenarios
            .iter()
            .position(|scenario| scenario.id == id)
            .ok_or_else(|| format!("scenario not found: {id}"))?;
        let removed = self.scenarios.remove(index);
        self.dirty = true;
        Ok(removed)
    }
}

fn xml_bool(value: Option<&str>) -> bool {
    matches!(value, Some("1" | "true" | "TRUE" | "True"))
}

fn parse_a1(value: &str) -> Option<(i32, i32)> {
    let value = value.replace('$', "");
    let mut column = 0i32;
    let mut split = 0usize;
    for (index, ch) in value.char_indices() {
        if ch.is_ascii_alphabetic() {
            column = column
                .checked_mul(26)?
                .checked_add((ch.to_ascii_uppercase() as u8 - b'A' + 1) as i32)?;
            split = index + ch.len_utf8();
        } else {
            break;
        }
    }
    if column == 0 || split == 0 {
        return None;
    }
    let row = value[split..].parse::<i32>().ok()?;
    (row >= 1 && row <= 1_048_576 && column <= 16_384).then_some((row, column))
}

/// Reads native worksheet scenarios. Imported definitions remain clean, so an untouched export
/// continues using the original byte-preserved worksheet XML.
pub fn import_scenarios(
    parts: &BTreeMap<String, Vec<u8>>,
    sheet_parts: &[String],
) -> ScenarioStore {
    let mut store = ScenarioStore::default();
    for (sheet, part) in sheet_parts.iter().enumerate() {
        let Some(xml) = parts
            .get(part)
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
        else {
            continue;
        };
        let Ok(document) = roxmltree::Document::parse(xml) else {
            continue;
        };
        let Some(container) = document
            .descendants()
            .find(|node| node.is_element() && node.tag_name().name() == "scenarios")
        else {
            continue;
        };
        for node in container
            .children()
            .filter(|node| node.is_element() && node.tag_name().name() == "scenario")
        {
            let Some(name) = node.attribute("name") else {
                continue;
            };
            let changes = node
                .children()
                .filter(|child| child.is_element() && child.tag_name().name() == "inputCells")
                .filter_map(|child| {
                    let (row, column) = parse_a1(child.attribute("r")?)?;
                    Some(ScenarioChange {
                        cell: CellAddress {
                            sheet: sheet as u32,
                            row,
                            column,
                        },
                        input: child.attribute("val").unwrap_or("").to_string(),
                    })
                })
                .collect::<Vec<_>>();
            if changes.is_empty() || changes.len() > MAX_SCENARIO_CHANGES {
                continue;
            }
            store.next_id = store.next_id.saturating_add(1);
            store.scenarios.push(Scenario {
                id: format!("scenario-{}", store.next_id),
                name: name.to_string(),
                comment: node.attribute("comment").unwrap_or("").to_string(),
                hidden: xml_bool(node.attribute("hidden")),
                locked: xml_bool(node.attribute("locked")),
                changes,
            });
        }
    }
    store.dirty = false;
    store
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn local_a1(cell: CellAddress) -> String {
    format!("{}{}", column_name(cell.column), cell.row)
}

fn scenario_xml(scenarios: &[&Scenario]) -> String {
    if scenarios.is_empty() {
        return String::new();
    }
    let mut refs = Vec::new();
    for scenario in scenarios {
        for change in &scenario.changes {
            let reference = local_a1(change.cell);
            if !refs.contains(&reference) {
                refs.push(reference);
            }
        }
    }
    let mut xml = format!(
        "<scenarios current=\"0\" show=\"0\" sqref=\"{}\">",
        refs.join(" ")
    );
    for scenario in scenarios {
        xml.push_str(&format!(
            "<scenario name=\"{}\" locked=\"{}\" hidden=\"{}\"",
            xml_escape(&scenario.name),
            u8::from(scenario.locked),
            u8::from(scenario.hidden)
        ));
        if !scenario.comment.is_empty() {
            xml.push_str(&format!(" comment=\"{}\"", xml_escape(&scenario.comment)));
        }
        xml.push('>');
        for change in &scenario.changes {
            xml.push_str(&format!(
                "<inputCells r=\"{}\" val=\"{}\"/>",
                local_a1(change.cell),
                xml_escape(&change.input)
            ));
        }
        xml.push_str("</scenario>");
    }
    xml.push_str("</scenarios>");
    xml
}

fn patch_scenarios_element(xml: &str, replacement: &str) -> Result<String, String> {
    let document = roxmltree::Document::parse(xml)
        .map_err(|error| format!("worksheet XML is invalid: {error}"))?;
    if let Some(existing) = document
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "scenarios")
    {
        let range = existing.range();
        return Ok(format!(
            "{}{}{}",
            &xml[..range.start],
            replacement,
            &xml[range.end..]
        ));
    }
    if replacement.is_empty() {
        return Ok(xml.to_string());
    }
    let insertion = [
        "<autoFilter",
        "<sortState",
        "<dataConsolidate",
        "<customSheetViews",
        "<mergeCells",
        "<phoneticPr",
        "<conditionalFormatting",
        "<dataValidations",
        "<hyperlinks",
        "<printOptions",
        "<pageMargins",
        "<pageSetup",
        "<headerFooter",
        "<rowBreaks",
        "<colBreaks",
        "<customProperties",
        "<cellWatches",
        "<ignoredErrors",
        "<smartTags",
        "<drawing",
        "<legacyDrawing",
        "<picture",
        "<oleObjects",
        "<controls",
        "<webPublishItems",
        "<tableParts",
        "<extLst",
        "</worksheet>",
    ]
    .iter()
    .filter_map(|marker| xml.find(marker))
    .min()
    .ok_or("worksheet has no valid scenario insertion point")?;
    Ok(format!(
        "{}{}{}",
        &xml[..insertion],
        replacement,
        &xml[insertion..]
    ))
}

/// Writes dirty Scenario Manager state into native worksheet OOXML.
pub fn apply_scenarios_to_parts(
    parts: &mut BTreeMap<String, Vec<u8>>,
    sheet_parts: &[String],
    store: &ScenarioStore,
) -> Result<(), String> {
    if !store.is_dirty() {
        return Ok(());
    }
    for (sheet, part) in sheet_parts.iter().enumerate() {
        let scenarios = store
            .scenarios
            .iter()
            .filter(|scenario| {
                scenario
                    .changes
                    .first()
                    .is_some_and(|change| change.cell.sheet == sheet as u32)
            })
            .collect::<Vec<_>>();
        let replacement = scenario_xml(&scenarios);
        let source = parts
            .get(part)
            .ok_or_else(|| format!("missing worksheet part {part}"))?;
        let source = std::str::from_utf8(source)
            .map_err(|_| format!("worksheet part is not UTF-8 XML: {part}"))?;
        let patched = patch_scenarios_element(source, &replacement)?;
        parts.insert(part.clone(), patched.into_bytes());
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioPreview {
    pub scenario: Scenario,
    pub before: Vec<Value>,
    pub after: Vec<Value>,
    pub writes: Vec<CellWrite>,
}

/// Applies a scenario to a private clone and reports exactly what would be written.
pub fn preview_scenario(
    model: &UserModel<'_>,
    scenario: &Scenario,
) -> Result<ScenarioPreview, String> {
    let mut sandbox = UserModel::from_bytes(&model.to_bytes(), "en")?;
    let mut before = Vec::with_capacity(scenario.changes.len());
    let mut after = Vec::with_capacity(scenario.changes.len());
    let mut writes = Vec::with_capacity(scenario.changes.len());
    for change in &scenario.changes {
        change.cell.validate()?;
        before.push(cell_value_json(model.get_model().get_cell_value_by_index(
            change.cell.sheet,
            change.cell.row,
            change.cell.column,
        )?));
        sandbox.set_user_input(
            change.cell.sheet,
            change.cell.row,
            change.cell.column,
            &change.input,
        )?;
        after.push(cell_value_json(
            sandbox.get_model().get_cell_value_by_index(
                change.cell.sheet,
                change.cell.row,
                change.cell.column,
            )?,
        ));
        writes.push(CellWrite {
            cell: change.cell,
            input: change.input.clone(),
        });
    }
    Ok(ScenarioPreview {
        scenario: scenario.clone(),
        before,
        after,
        writes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> UserModel<'static> {
        let mut model = UserModel::new_empty("Book1", "en", "UTC", "en").unwrap();
        model.set_user_input(0, 1, 1, "1").unwrap();
        model.set_user_input(0, 1, 2, "=A1*A1").unwrap();
        model
    }

    #[test]
    fn goal_seek_converges_without_mutating_the_live_workbook() {
        let model = model();
        let outcome = goal_seek(
            &model,
            &GoalSeekRequest {
                target: CellAddress {
                    sheet: 0,
                    row: 1,
                    column: 2,
                },
                changing: CellAddress {
                    sheet: 0,
                    row: 1,
                    column: 1,
                },
                target_value: 9.0,
                initial_value: None,
                lower_bound: Some(0.0),
                upper_bound: Some(10.0),
                initial_step: None,
                max_iterations: 100,
                tolerance: 1e-10,
            },
        )
        .unwrap();
        assert!(outcome.converged, "{}", outcome.message);
        assert!((outcome.result_value - 3.0).abs() < 1e-7);
        assert!((outcome.achieved_value.unwrap() - 9.0).abs() < 1e-8);
        assert_eq!(model.get_cell_content(0, 1, 1).unwrap(), "1");
    }

    #[test]
    fn one_and_two_variable_tables_have_excel_shaped_headers() {
        let mut model = model();
        model.set_user_input(0, 2, 1, "2").unwrap();
        model.set_user_input(0, 2, 2, "3").unwrap();
        model.set_user_input(0, 2, 3, "=A2*B2").unwrap();
        let one = data_table(
            &model,
            &DataTableRequest::OneVariable {
                formula_cell: CellAddress {
                    sheet: 0,
                    row: 1,
                    column: 2,
                },
                input_cell: CellAddress {
                    sheet: 0,
                    row: 1,
                    column: 1,
                },
                values: vec![Value::from(2), Value::from(3)],
                orientation: TableOrientation::Column,
                output: CellAddress {
                    sheet: 0,
                    row: 4,
                    column: 1,
                },
            },
        )
        .unwrap();
        assert_eq!((one.rows, one.columns, one.evaluations), (3, 2, 2));
        assert_eq!(one.matrix[2][1].as_f64(), Some(9.0));
        assert!(!one.dynamic);

        let two = data_table(
            &model,
            &DataTableRequest::TwoVariable {
                formula_cell: CellAddress {
                    sheet: 0,
                    row: 2,
                    column: 3,
                },
                row_input_cell: CellAddress {
                    sheet: 0,
                    row: 2,
                    column: 1,
                },
                column_input_cell: CellAddress {
                    sheet: 0,
                    row: 2,
                    column: 2,
                },
                row_values: vec![Value::from(4), Value::from(5)],
                column_values: vec![Value::from(6), Value::from(7)],
                output: CellAddress {
                    sheet: 0,
                    row: 8,
                    column: 1,
                },
            },
        )
        .unwrap();
        assert_eq!((two.rows, two.columns, two.evaluations), (3, 3, 4));
        assert_eq!(two.matrix[1][1].as_f64(), Some(24.0));
        assert_eq!(two.matrix[2][2].as_f64(), Some(35.0));
    }

    #[test]
    fn scenario_store_validates_names_and_previews_in_isolation() {
        let model = model();
        let mut store = ScenarioStore::default();
        let scenario = store
            .create(Scenario {
                id: String::new(),
                name: "High growth".into(),
                comment: "fixture".into(),
                hidden: false,
                locked: false,
                changes: vec![ScenarioChange {
                    cell: CellAddress {
                        sheet: 0,
                        row: 1,
                        column: 1,
                    },
                    input: "4".into(),
                }],
            })
            .unwrap();
        let preview = preview_scenario(&model, &scenario).unwrap();
        assert_eq!(preview.before[0].as_f64(), Some(1.0));
        assert_eq!(preview.after[0].as_f64(), Some(4.0));
        assert_eq!(model.get_cell_content(0, 1, 1).unwrap(), "1");
        assert!(
            store
                .create(Scenario {
                    id: String::new(),
                    ..scenario.clone()
                })
                .is_err()
        );
    }

    #[test]
    fn native_scenarios_import_clean_and_dirty_edits_write_excel_xml() {
        let sheet = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetData/><scenarios current=\"0\" show=\"0\" sqref=\"A1\"><scenario name=\"Base &amp; safe\" locked=\"1\" comment=\"old\"><inputCells r=\"$A$1\" val=\"2\"/></scenario></scenarios><pageMargins left=\"0.7\"/></worksheet>";
        let mut parts =
            BTreeMap::from([("xl/worksheets/sheet1.xml".into(), sheet.as_bytes().to_vec())]);
        let paths = vec!["xl/worksheets/sheet1.xml".to_string()];
        let mut store = import_scenarios(&parts, &paths);
        assert!(!store.is_dirty());
        assert_eq!(store.list()[0].name, "Base & safe");
        let id = store.list()[0].id.clone();
        let mut edited = store.list()[0].clone();
        edited.comment = "new <comment>".into();
        edited.changes[0].input = "5".into();
        store.update(&id, edited).unwrap();
        assert!(store.is_dirty());
        apply_scenarios_to_parts(&mut parts, &paths, &store).unwrap();
        let xml = std::str::from_utf8(&parts["xl/worksheets/sheet1.xml"]).unwrap();
        assert!(xml.contains("name=\"Base &amp; safe\""));
        assert!(xml.contains("comment=\"new &lt;comment&gt;\""));
        assert!(xml.contains("r=\"A1\" val=\"5\""));
        assert!(xml.contains("<pageMargins left=\"0.7\"/>"));
        assert_eq!(xml.matches("<scenarios").count(), 1);
    }
}
