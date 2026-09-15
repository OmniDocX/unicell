use std::{cmp::Ordering, collections::HashMap};

use crate::{
    calc_result::CalcResult,
    constants::{LAST_COLUMN, LAST_ROW},
    expressions::{
        parser::{ArrayNode, Node},
        token::Error,
        types::CellReferenceIndex,
    },
    functions::{subtotal::CellTableStatus, Function},
    model::Model,
};

#[derive(Clone, Copy)]
struct AggregateOptions {
    ignore_hidden: bool,
    ignore_errors: bool,
    ignore_nested: bool,
}

impl AggregateOptions {
    fn from_code(code: i32) -> Option<Self> {
        Some(match code {
            0 => Self::new(false, false, true),
            1 => Self::new(true, false, true),
            2 => Self::new(false, true, true),
            3 => Self::new(true, true, true),
            4 => Self::new(false, false, false),
            5 => Self::new(true, false, false),
            6 => Self::new(false, true, false),
            7 => Self::new(true, true, false),
            _ => return None,
        })
    }

    const fn new(ignore_hidden: bool, ignore_errors: bool, ignore_nested: bool) -> Self {
        Self {
            ignore_hidden,
            ignore_errors,
            ignore_nested,
        }
    }
}

#[derive(Default)]
struct AggregateData {
    numbers: Vec<f64>,
    non_empty: usize,
    first_error: Option<CalcResult>,
}

impl AggregateData {
    fn push_result(
        &mut self,
        result: CalcResult,
        options: AggregateOptions,
        origin: CellReferenceIndex,
    ) -> Result<(), CalcResult> {
        match result {
            CalcResult::Number(value) => {
                self.numbers.push(value);
                self.non_empty += 1;
            }
            CalcResult::String(_) | CalcResult::Boolean(_) => self.non_empty += 1,
            CalcResult::EmptyCell | CalcResult::EmptyArg => {}
            error @ CalcResult::Error { .. } => {
                if !options.ignore_errors {
                    self.non_empty += 1;
                    if self.first_error.is_none() {
                        self.first_error = Some(error);
                    }
                }
            }
            CalcResult::Range { .. } | CalcResult::Array(_) | CalcResult::Lambda(_) => {
                return Err(CalcResult::new_error(
                    Error::VALUE,
                    origin,
                    "Unexpected nested value in AGGREGATE".to_string(),
                ));
            }
        }
        Ok(())
    }

    fn push_array_node(
        &mut self,
        value: ArrayNode,
        options: AggregateOptions,
        origin: CellReferenceIndex,
    ) {
        match value {
            ArrayNode::Number(value) => {
                self.numbers.push(value);
                self.non_empty += 1;
            }
            ArrayNode::String(_) | ArrayNode::Boolean(_) => self.non_empty += 1,
            ArrayNode::Empty => {}
            ArrayNode::Error(error) if !options.ignore_errors => {
                self.non_empty += 1;
                if self.first_error.is_none() {
                    self.first_error = Some(CalcResult::new_error(
                        error,
                        origin,
                        "Error in AGGREGATE array".to_string(),
                    ));
                }
            }
            ArrayNode::Error(_) => {}
        }
    }
}

fn is_nested_aggregate(node: &Node) -> bool {
    matches!(
        node,
        Node::FunctionKind {
            kind: Function::Subtotal | Function::Aggregate,
            ..
        }
    )
}

fn div_zero(cell: CellReferenceIndex, name: &str) -> CalcResult {
    CalcResult::new_error(Error::DIV, cell, format!("{name}: division by zero"))
}

fn num_error(cell: CellReferenceIndex, message: &str) -> CalcResult {
    CalcResult::new_error(Error::NUM, cell, message.to_string())
}

fn percentile_inc(sorted: &[f64], k: f64) -> f64 {
    if sorted.len() == 1 {
        return sorted[0];
    }
    let rank = k * (sorted.len() - 1) as f64;
    let low = rank.floor() as usize;
    let high = (low + 1).min(sorted.len() - 1);
    sorted[low] + (rank - rank.floor()) * (sorted[high] - sorted[low])
}

fn percentile_exc(sorted: &[f64], k: f64) -> Option<f64> {
    let n = sorted.len() as f64;
    if sorted.is_empty() || k <= 0.0 || k >= 1.0 || k < 1.0 / (n + 1.0) || k > n / (n + 1.0) {
        return None;
    }
    let rank = k * (n + 1.0) - 1.0;
    let low = rank.floor() as usize;
    let high = (low + 1).min(sorted.len() - 1);
    Some(sorted[low] + (rank - rank.floor()) * (sorted[high] - sorted[low]))
}

impl<'a> Model<'a> {
    fn aggregate_collect(
        &mut self,
        args: &[Node],
        cell: CellReferenceIndex,
        options: AggregateOptions,
    ) -> Result<AggregateData, CalcResult> {
        let mut data = AggregateData::default();

        for arg in args {
            if options.ignore_nested && is_nested_aggregate(arg) {
                continue;
            }

            match self.evaluate_node_with_reference(arg, cell) {
                CalcResult::Range { left, right } => {
                    if left.sheet != right.sheet {
                        return Err(CalcResult::new_error(
                            Error::VALUE,
                            cell,
                            "AGGREGATE does not accept 3-D references".to_string(),
                        ));
                    }

                    let row1 = left.row.min(right.row);
                    let mut row2 = left.row.max(right.row);
                    let column1 = left.column.min(right.column);
                    let mut column2 = left.column.max(right.column);
                    if row1 == 1 && row2 == LAST_ROW {
                        row2 = self
                            .workbook
                            .worksheet(left.sheet)
                            .map_err(|message| CalcResult::new_error(Error::REF, cell, message))?
                            .dimension()
                            .max_row;
                    }
                    if column1 == 1 && column2 == LAST_COLUMN {
                        column2 = self
                            .workbook
                            .worksheet(left.sheet)
                            .map_err(|message| CalcResult::new_error(Error::REF, cell, message))?
                            .dimension()
                            .max_column;
                    }

                    for row in row1..=row2 {
                        let status = self.cell_hidden_status(left.sheet, row, column1).map_err(
                            |message| CalcResult::new_error(Error::ERROR, cell, message),
                        )?;
                        if status == CellTableStatus::Filtered
                            || (options.ignore_hidden && status == CellTableStatus::Hidden)
                        {
                            continue;
                        }
                        for column in column1..=column2 {
                            if options.ignore_nested
                                && self.cell_is_subtotal_or_aggregate(left.sheet, row, column)
                            {
                                continue;
                            }
                            let value = self.evaluate_cell(CellReferenceIndex {
                                sheet: left.sheet,
                                row,
                                column,
                            });
                            data.push_result(value, options, cell)?;
                        }
                    }
                }
                CalcResult::Array(array) => {
                    for row in array {
                        for value in row {
                            data.push_array_node(value, options, cell);
                        }
                    }
                }
                value => data.push_result(value, options, cell)?,
            }
        }

        Ok(data)
    }

    pub(crate) fn fn_aggregate(&mut self, args: &[Node], cell: CellReferenceIndex) -> CalcResult {
        if args.len() < 3 {
            return CalcResult::new_args_number_error(cell);
        }
        let function_num = match self.get_number(&args[0], cell) {
            Ok(value) if value.is_finite() => value.trunc() as i32,
            Ok(_) => {
                return CalcResult::new_error(
                    Error::VALUE,
                    cell,
                    "Invalid AGGREGATE function number".to_string(),
                )
            }
            Err(error) => return error,
        };
        if !(1..=19).contains(&function_num) {
            return CalcResult::new_error(
                Error::VALUE,
                cell,
                format!("Invalid AGGREGATE function number: {function_num}"),
            );
        }
        if function_num >= 14 && args.len() != 4 {
            return CalcResult::new_args_number_error(cell);
        }

        let option_code = match self.get_number(&args[1], cell) {
            Ok(value) if value.is_finite() => value.trunc() as i32,
            Ok(_) => {
                return CalcResult::new_error(
                    Error::VALUE,
                    cell,
                    "Invalid AGGREGATE option".to_string(),
                )
            }
            Err(error) => return error,
        };
        let Some(options) = AggregateOptions::from_code(option_code) else {
            return CalcResult::new_error(
                Error::VALUE,
                cell,
                format!("Invalid AGGREGATE option: {option_code}"),
            );
        };

        let value_args = if function_num >= 14 {
            &args[2..3]
        } else {
            &args[2..]
        };
        let mut data = match self.aggregate_collect(value_args, cell, options) {
            Ok(data) => data,
            Err(error) => return error,
        };

        if function_num == 2 {
            return CalcResult::Number(data.numbers.len() as f64);
        }
        if function_num == 3 {
            return CalcResult::Number(data.non_empty as f64);
        }
        if let Some(error) = data.first_error {
            return error;
        }

        match function_num {
            1 => {
                if data.numbers.is_empty() {
                    div_zero(cell, "AGGREGATE/AVERAGE")
                } else {
                    CalcResult::Number(data.numbers.iter().sum::<f64>() / data.numbers.len() as f64)
                }
            }
            4 => CalcResult::Number(data.numbers.into_iter().reduce(f64::max).unwrap_or(0.0)),
            5 => CalcResult::Number(data.numbers.into_iter().reduce(f64::min).unwrap_or(0.0)),
            6 => {
                if data.numbers.is_empty() {
                    CalcResult::Number(0.0)
                } else {
                    CalcResult::Number(data.numbers.into_iter().product())
                }
            }
            7 | 8 | 10 | 11 => {
                let sample = matches!(function_num, 7 | 10);
                let len = data.numbers.len();
                if (sample && len < 2) || (!sample && len == 0) {
                    return div_zero(cell, "AGGREGATE variance");
                }
                let mean = data.numbers.iter().sum::<f64>() / len as f64;
                let divisor = if sample { len - 1 } else { len } as f64;
                let variance = data
                    .numbers
                    .iter()
                    .map(|value| (value - mean).powi(2))
                    .sum::<f64>()
                    / divisor;
                if matches!(function_num, 7 | 8) {
                    CalcResult::Number(variance.sqrt())
                } else {
                    CalcResult::Number(variance)
                }
            }
            9 => CalcResult::Number(data.numbers.into_iter().sum()),
            12 => {
                if data.numbers.is_empty() {
                    return num_error(cell, "AGGREGATE/MEDIAN has no numeric values");
                }
                data.numbers
                    .sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
                let len = data.numbers.len();
                if len % 2 == 1 {
                    CalcResult::Number(data.numbers[len / 2])
                } else {
                    CalcResult::Number((data.numbers[len / 2 - 1] + data.numbers[len / 2]) / 2.0)
                }
            }
            13 => {
                let mut counts: HashMap<u64, (f64, usize, usize)> = HashMap::new();
                for (index, value) in data.numbers.into_iter().enumerate() {
                    counts.entry(value.to_bits()).or_insert((value, 0, index)).1 += 1;
                }
                let max_count = counts.values().map(|entry| entry.1).max().unwrap_or(0);
                if max_count < 2 {
                    return CalcResult::new_error(
                        Error::NA,
                        cell,
                        "AGGREGATE/MODE.SNGL found no repeated value".to_string(),
                    );
                }
                let value = counts
                    .values()
                    .filter(|entry| entry.1 == max_count)
                    .min_by_key(|entry| entry.2)
                    .map(|entry| entry.0)
                    .unwrap_or(0.0);
                CalcResult::Number(value)
            }
            14 | 15 => {
                let k = match self.get_number_no_bools(&args[3], cell) {
                    Ok(value) if value.is_finite() => value.trunc(),
                    Ok(_) => return num_error(cell, "AGGREGATE k is invalid"),
                    Err(error) => return error,
                };
                if k < 1.0 || k > data.numbers.len() as f64 {
                    return num_error(cell, "AGGREGATE k is outside the data range");
                }
                data.numbers
                    .sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
                let index = k as usize - 1;
                if function_num == 14 {
                    CalcResult::Number(data.numbers[data.numbers.len() - 1 - index])
                } else {
                    CalcResult::Number(data.numbers[index])
                }
            }
            16 | 18 => {
                if data.numbers.is_empty() {
                    return num_error(cell, "AGGREGATE percentile has no numeric values");
                }
                let k = match self.get_number_no_bools(&args[3], cell) {
                    Ok(value) if value.is_finite() => value,
                    Ok(_) => return num_error(cell, "AGGREGATE percentile is invalid"),
                    Err(error) => return error,
                };
                data.numbers
                    .sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
                if function_num == 16 {
                    if !(0.0..=1.0).contains(&k) {
                        return num_error(cell, "AGGREGATE/PERCENTILE.INC k is outside 0..1");
                    }
                    CalcResult::Number(percentile_inc(&data.numbers, k))
                } else {
                    match percentile_exc(&data.numbers, k) {
                        Some(value) => CalcResult::Number(value),
                        None => num_error(
                            cell,
                            "AGGREGATE/PERCENTILE.EXC k is outside its valid range",
                        ),
                    }
                }
            }
            17 | 19 => {
                if data.numbers.is_empty() {
                    return num_error(cell, "AGGREGATE quartile has no numeric values");
                }
                let quart = match self.get_number_no_bools(&args[3], cell) {
                    Ok(value) if value.is_finite() => value.floor() as i32,
                    Ok(_) => return num_error(cell, "AGGREGATE quartile is invalid"),
                    Err(error) => return error,
                };
                data.numbers
                    .sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
                if function_num == 17 {
                    if !(0..=4).contains(&quart) {
                        return num_error(cell, "AGGREGATE/QUARTILE.INC requires 0..4");
                    }
                    CalcResult::Number(percentile_inc(&data.numbers, quart as f64 / 4.0))
                } else {
                    if !(1..=3).contains(&quart) {
                        return num_error(cell, "AGGREGATE/QUARTILE.EXC requires 1..3");
                    }
                    match percentile_exc(&data.numbers, quart as f64 / 4.0) {
                        Some(value) => CalcResult::Number(value),
                        None => num_error(cell, "AGGREGATE/QUARTILE.EXC does not have enough data"),
                    }
                }
            }
            _ => unreachable!(),
        }
    }
}
