use std::{cmp::Ordering, collections::HashMap};

use crate::{
    calc_result::CalcResult,
    expressions::{
        parser::{ArrayNode, Node},
        token::Error,
        types::CellReferenceIndex,
    },
    functions::{math_and_trigonometry::array_size::check_array_size, Function},
    model::Model,
};

#[derive(Clone)]
enum SummaryFunction {
    Sum,
    Average,
    Count,
    Max,
    Min,
    Lambda(CalcResult),
}

#[derive(Clone, Eq, Hash, PartialEq)]
enum KeyAtom {
    Empty,
    Number(u64),
    Text(String),
    Boolean(bool),
}

#[derive(Clone)]
struct Group {
    labels: Vec<ArrayNode>,
    rows: Vec<usize>,
}

#[derive(Clone)]
struct AxisEntry {
    labels: Vec<ArrayNode>,
    rows: Vec<usize>,
}

#[derive(Clone, Copy)]
struct HeaderConfig {
    consume: bool,
    show: bool,
}

fn key_atom(value: &ArrayNode) -> Option<KeyAtom> {
    match value {
        ArrayNode::Empty => Some(KeyAtom::Empty),
        ArrayNode::Number(value) => {
            // Excel groups -0 and +0 together.  Canonicalising NaNs also keeps
            // hashing deterministic for malformed imported workbooks.
            let bits = if *value == 0.0 {
                0
            } else if value.is_nan() {
                f64::NAN.to_bits()
            } else {
                value.to_bits()
            };
            Some(KeyAtom::Number(bits))
        }
        ArrayNode::String(value) => Some(KeyAtom::Text(value.to_uppercase())),
        ArrayNode::Boolean(value) => Some(KeyAtom::Boolean(*value)),
        ArrayNode::Error(_) => None,
    }
}

fn node_rank(value: &ArrayNode) -> u8 {
    match value {
        ArrayNode::Number(_) => 0,
        ArrayNode::String(_) => 1,
        ArrayNode::Boolean(_) => 2,
        ArrayNode::Empty => 3,
        ArrayNode::Error(_) => 4,
    }
}

fn compare_nodes(left: &ArrayNode, right: &ArrayNode) -> Ordering {
    let rank = node_rank(left).cmp(&node_rank(right));
    if rank != Ordering::Equal {
        return rank;
    }
    match (left, right) {
        (ArrayNode::Number(a), ArrayNode::Number(b)) => a.partial_cmp(b).unwrap_or(Ordering::Equal),
        (ArrayNode::String(a), ArrayNode::String(b)) => a.to_uppercase().cmp(&b.to_uppercase()),
        (ArrayNode::Boolean(a), ArrayNode::Boolean(b)) => a.cmp(b),
        (ArrayNode::Error(a), ArrayNode::Error(b)) => format!("{a:?}").cmp(&format!("{b:?}")),
        _ => Ordering::Equal,
    }
}

fn compare_label_vectors(left: &[ArrayNode], right: &[ArrayNode]) -> Ordering {
    for (a, b) in left.iter().zip(right) {
        let order = compare_nodes(a, b);
        if order != Ordering::Equal {
            return order;
        }
    }
    left.len().cmp(&right.len())
}

fn rectangular_width(data: &[Vec<ArrayNode>]) -> Option<usize> {
    let width = data.first()?.len();
    if width == 0 || data.iter().any(|row| row.len() != width) {
        None
    } else {
        Some(width)
    }
}

fn flatten_vector(data: Vec<Vec<ArrayNode>>) -> Option<Vec<ArrayNode>> {
    if data.is_empty() || data[0].is_empty() {
        return None;
    }
    if data.len() == 1 {
        return data.into_iter().next();
    }
    if data.iter().all(|row| row.len() == 1) {
        return Some(data.into_iter().map(|row| row[0].clone()).collect());
    }
    None
}

fn display_label(value: &ArrayNode) -> String {
    match value {
        ArrayNode::Number(value) => value.to_string(),
        ArrayNode::String(value) => value.clone(),
        ArrayNode::Boolean(true) => "TRUE".to_string(),
        ArrayNode::Boolean(false) => "FALSE".to_string(),
        ArrayNode::Empty => String::new(),
        ArrayNode::Error(error) => format!("{error:?}"),
    }
}

fn build_groups(fields: &[Vec<ArrayNode>], included_rows: &[usize]) -> Result<Vec<Group>, Error> {
    let mut positions: HashMap<Vec<KeyAtom>, usize> = HashMap::new();
    let mut groups: Vec<Group> = Vec::new();

    for &row_index in included_rows {
        let labels = fields[row_index].clone();
        let mut key = Vec::with_capacity(labels.len());
        for label in &labels {
            let Some(atom) = key_atom(label) else {
                if let ArrayNode::Error(error) = label {
                    return Err(error.clone());
                }
                return Err(Error::VALUE);
            };
            key.push(atom);
        }

        if let Some(&position) = positions.get(&key) {
            groups[position].rows.push(row_index);
        } else {
            let position = groups.len();
            positions.insert(key, position);
            groups.push(Group {
                labels,
                rows: vec![row_index],
            });
        }
    }
    Ok(groups)
}

fn subtotal_entry(groups: &[Group], level: usize) -> AxisEntry {
    let mut labels = groups[0].labels.clone();
    labels.truncate(level + 1);
    labels.resize(groups[0].labels.len(), ArrayNode::Empty);
    labels[level] = ArrayNode::String(format!("{} Total", display_label(&groups[0].labels[level])));
    AxisEntry {
        labels,
        rows: groups
            .iter()
            .flat_map(|group| group.rows.iter().copied())
            .collect(),
    }
}

fn render_group_level(
    groups: &[Group],
    level: usize,
    subtotal_levels: usize,
    totals_at_top: bool,
    output: &mut Vec<AxisEntry>,
) {
    if groups.is_empty() {
        return;
    }
    let field_count = groups[0].labels.len();
    if level >= field_count {
        output.extend(groups.iter().map(|group| AxisEntry {
            labels: group.labels.clone(),
            rows: group.rows.clone(),
        }));
        return;
    }

    let mut start = 0;
    while start < groups.len() {
        let mut end = start + 1;
        while end < groups.len()
            && compare_nodes(&groups[start].labels[level], &groups[end].labels[level])
                == Ordering::Equal
        {
            end += 1;
        }
        let section = &groups[start..end];
        let show_subtotal = level < subtotal_levels && level + 1 < field_count;
        if show_subtotal && totals_at_top {
            output.push(subtotal_entry(section, level));
        }
        if level + 1 == field_count {
            output.extend(section.iter().map(|group| AxisEntry {
                labels: group.labels.clone(),
                rows: group.rows.clone(),
            }));
        } else {
            render_group_level(section, level + 1, subtotal_levels, totals_at_top, output);
        }
        if show_subtotal && !totals_at_top {
            output.push(subtotal_entry(section, level));
        }
        start = end;
    }
}

fn build_axis_entries(groups: &[Group], total_depth: i32) -> Vec<AxisEntry> {
    if groups.is_empty() {
        return Vec::new();
    }
    let totals_at_top = total_depth < 0;
    let subtotal_levels = total_depth
        .unsigned_abs()
        .saturating_sub(1)
        .min(groups[0].labels.len().saturating_sub(1) as u32) as usize;
    let mut result = Vec::new();
    let mut grand_labels = vec![ArrayNode::Empty; groups[0].labels.len()];
    grand_labels[0] = ArrayNode::String("Grand Total".to_string());
    let grand = AxisEntry {
        labels: grand_labels,
        rows: groups
            .iter()
            .flat_map(|group| group.rows.iter().copied())
            .collect(),
    };

    if total_depth != 0 && totals_at_top {
        result.push(grand.clone());
    }
    render_group_level(groups, 0, subtotal_levels, totals_at_top, &mut result);
    if total_depth != 0 && !totals_at_top {
        result.push(grand);
    }
    result
}

fn generated_headers(prefix: &str, count: usize) -> Vec<ArrayNode> {
    (0..count)
        .map(|index| {
            let name = if count == 1 {
                prefix.to_string()
            } else {
                format!("{prefix} {}", index + 1)
            };
            ArrayNode::String(name)
        })
        .collect()
}

fn result_to_array_node(result: CalcResult) -> ArrayNode {
    match result {
        CalcResult::Number(value) => ArrayNode::Number(value),
        CalcResult::String(value) => ArrayNode::String(value),
        CalcResult::Boolean(value) => ArrayNode::Boolean(value),
        CalcResult::EmptyCell | CalcResult::EmptyArg => ArrayNode::Empty,
        CalcResult::Error { error, .. } => ArrayNode::Error(error),
        CalcResult::Range { .. } | CalcResult::Array(_) | CalcResult::Lambda(_) => {
            ArrayNode::Error(Error::VALUE)
        }
    }
}

impl<'a> Model<'a> {
    fn optional_integer(
        &mut self,
        args: &[Node],
        index: usize,
        cell: CellReferenceIndex,
    ) -> Result<Option<i32>, CalcResult> {
        let Some(argument) = args.get(index) else {
            return Ok(None);
        };
        if matches!(argument, Node::EmptyArgKind) {
            return Ok(None);
        }
        let value = self.get_number(argument, cell)?;
        if !value.is_finite()
            || value.fract() != 0.0
            || value < i32::MIN as f64
            || value > i32::MAX as f64
        {
            return Err(CalcResult::new_error(
                Error::VALUE,
                cell,
                "Expected an integer option".to_string(),
            ));
        }
        Ok(Some(value as i32))
    }

    fn header_config(
        &mut self,
        args: &[Node],
        index: usize,
        values: &[Vec<ArrayNode>],
        group_levels: usize,
        cell: CellReferenceIndex,
    ) -> Result<HeaderConfig, CalcResult> {
        let inferred = values.len() >= 2
            && matches!(values[0].first(), Some(ArrayNode::String(_)))
            && matches!(values[1].first(), Some(ArrayNode::Number(_)));
        match self.optional_integer(args, index, cell)? {
            None => Ok(HeaderConfig {
                consume: inferred,
                show: group_levels > 1,
            }),
            Some(0) => Ok(HeaderConfig {
                consume: false,
                show: false,
            }),
            Some(1) => Ok(HeaderConfig {
                consume: true,
                show: false,
            }),
            Some(2) => Ok(HeaderConfig {
                consume: false,
                show: true,
            }),
            Some(3) => Ok(HeaderConfig {
                consume: true,
                show: true,
            }),
            Some(_) => Err(CalcResult::new_error(
                Error::VALUE,
                cell,
                "field_headers must be 0, 1, 2, or 3".to_string(),
            )),
        }
    }

    fn parse_summary_function(
        &mut self,
        node: &Node,
        cell: CellReferenceIndex,
    ) -> Result<SummaryFunction, CalcResult> {
        fn from_name(name: &str) -> Option<SummaryFunction> {
            match name
                .trim_start_matches("_xlfn.")
                .to_ascii_uppercase()
                .as_str()
            {
                "SUM" => Some(SummaryFunction::Sum),
                "AVERAGE" => Some(SummaryFunction::Average),
                "COUNT" => Some(SummaryFunction::Count),
                "MAX" => Some(SummaryFunction::Max),
                "MIN" => Some(SummaryFunction::Min),
                _ => None,
            }
        }

        let mut current = node;
        while let Node::ImplicitIntersection { child, .. } = current {
            current = child;
        }
        if let Node::NamedVariableKind { name, .. } = current {
            if let Some(function) = from_name(name) {
                return Ok(function);
            }
        }
        if let Node::FunctionKind { kind, args } = current {
            if args.is_empty() {
                let builtin = match kind {
                    Function::Sum => Some(SummaryFunction::Sum),
                    Function::Average => Some(SummaryFunction::Average),
                    Function::Count => Some(SummaryFunction::Count),
                    Function::Max => Some(SummaryFunction::Max),
                    Function::Min => Some(SummaryFunction::Min),
                    _ => None,
                };
                if let Some(function) = builtin {
                    return Ok(function);
                }
            }
        }

        match self.evaluate_node_in_context(current, cell) {
            lambda @ CalcResult::Lambda(_) => Ok(SummaryFunction::Lambda(lambda)),
            error @ CalcResult::Error { .. } => Err(error),
            _ => Err(CalcResult::new_error(
                Error::VALUE,
                cell,
                "function must be SUM, AVERAGE, COUNT, MAX, MIN, or a LAMBDA".to_string(),
            )),
        }
    }

    fn aggregate_nodes(
        &mut self,
        function: &SummaryFunction,
        nodes: Vec<ArrayNode>,
        cell: CellReferenceIndex,
    ) -> ArrayNode {
        // COUNT ignores errors in referenced data, and an explicit LAMBDA must
        // receive the original vector so its body can decide how to handle an
        // error.  SUM/AVERAGE/MAX/MIN propagate errors like their Excel range
        // counterparts.
        if !matches!(
            function,
            SummaryFunction::Count | SummaryFunction::Lambda(_)
        ) {
            if let Some(error) = nodes.iter().find_map(|node| match node {
                ArrayNode::Error(error) => Some(error.clone()),
                _ => None,
            }) {
                return ArrayNode::Error(error);
            }
        }

        match function {
            SummaryFunction::Sum => ArrayNode::Number(
                nodes
                    .iter()
                    .filter_map(|node| match node {
                        ArrayNode::Number(value) => Some(*value),
                        _ => None,
                    })
                    .sum(),
            ),
            SummaryFunction::Average => {
                let numbers: Vec<f64> = nodes
                    .iter()
                    .filter_map(|node| match node {
                        ArrayNode::Number(value) => Some(*value),
                        _ => None,
                    })
                    .collect();
                if numbers.is_empty() {
                    ArrayNode::Error(Error::DIV)
                } else {
                    ArrayNode::Number(numbers.iter().sum::<f64>() / numbers.len() as f64)
                }
            }
            SummaryFunction::Count => ArrayNode::Number(
                nodes
                    .iter()
                    .filter(|node| matches!(node, ArrayNode::Number(_)))
                    .count() as f64,
            ),
            SummaryFunction::Max => ArrayNode::Number(
                nodes
                    .iter()
                    .filter_map(|node| match node {
                        ArrayNode::Number(value) => Some(*value),
                        _ => None,
                    })
                    .reduce(f64::max)
                    .unwrap_or(0.0),
            ),
            SummaryFunction::Min => ArrayNode::Number(
                nodes
                    .iter()
                    .filter_map(|node| match node {
                        ArrayNode::Number(value) => Some(*value),
                        _ => None,
                    })
                    .reduce(f64::min)
                    .unwrap_or(0.0),
            ),
            SummaryFunction::Lambda(lambda) => {
                let vector = nodes.into_iter().map(|value| vec![value]).collect();
                result_to_array_node(self.call_lambda(
                    lambda.clone(),
                    &[Node::ArrayKind(vector)],
                    cell,
                ))
            }
        }
    }

    fn aggregate_rows(
        &mut self,
        function: &SummaryFunction,
        values: &[Vec<ArrayNode>],
        rows: &[usize],
        value_column: usize,
        cell: CellReferenceIndex,
    ) -> ArrayNode {
        self.aggregate_nodes(
            function,
            rows.iter()
                .map(|&row| values[row][value_column].clone())
                .collect(),
            cell,
        )
    }

    fn parse_filter(
        &mut self,
        argument: Option<&Node>,
        original_rows: usize,
        data_rows: usize,
        consumed_header: bool,
        cell: CellReferenceIndex,
    ) -> Result<Vec<bool>, CalcResult> {
        let Some(argument) = argument else {
            return Ok(vec![true; data_rows]);
        };
        if matches!(argument, Node::EmptyArgKind) {
            return Ok(vec![true; data_rows]);
        }
        let data = self.eval_to_array(argument, cell)?;
        let Some(mut filter) = flatten_vector(data) else {
            return Err(CalcResult::new_error(
                Error::VALUE,
                cell,
                "filter_array must be one-dimensional".to_string(),
            ));
        };
        if consumed_header && filter.len() == original_rows {
            filter.remove(0);
        }
        if filter.len() != data_rows {
            return Err(CalcResult::new_error(
                Error::VALUE,
                cell,
                "filter_array length must match the source rows".to_string(),
            ));
        }
        filter
            .into_iter()
            .map(|value| match value {
                ArrayNode::Boolean(value) => Ok(value),
                ArrayNode::Number(value) => Ok(value != 0.0),
                ArrayNode::Empty => Ok(false),
                ArrayNode::Error(error) => Err(CalcResult::new_error(
                    error,
                    cell,
                    "filter_array contains an error".to_string(),
                )),
                ArrayNode::String(_) => Err(CalcResult::new_error(
                    Error::VALUE,
                    cell,
                    "filter_array must contain booleans or numbers".to_string(),
                )),
            })
            .collect()
    }

    fn parse_sort_orders(
        &mut self,
        argument: Option<&Node>,
        maximum_index: usize,
        cell: CellReferenceIndex,
    ) -> Result<Option<Vec<i32>>, CalcResult> {
        let Some(argument) = argument else {
            return Ok(None);
        };
        if matches!(argument, Node::EmptyArgKind) {
            return Ok(None);
        }
        let data = self.eval_to_array(argument, cell)?;
        let Some(values) = flatten_vector(data) else {
            return Err(CalcResult::new_error(
                Error::VALUE,
                cell,
                "sort_order must be a number or vector".to_string(),
            ));
        };
        let mut result = Vec::with_capacity(values.len());
        for value in values {
            let ArrayNode::Number(value) = value else {
                return Err(CalcResult::new_error(
                    Error::VALUE,
                    cell,
                    "sort_order must contain numbers".to_string(),
                ));
            };
            if !value.is_finite() || value.fract() != 0.0 {
                return Err(CalcResult::new_error(
                    Error::VALUE,
                    cell,
                    "sort_order must contain integers".to_string(),
                ));
            }
            let order = value as i32;
            if order == 0 || order.unsigned_abs() as usize > maximum_index {
                return Err(CalcResult::new_error(
                    Error::VALUE,
                    cell,
                    "sort_order index is outside the result fields".to_string(),
                ));
            }
            result.push(order);
        }
        Ok(Some(result))
    }

    fn sort_groups(
        &mut self,
        groups: &mut Vec<Group>,
        sort_orders: Option<Vec<i32>>,
        field_count: usize,
        values: &[Vec<ArrayNode>],
        function: &SummaryFunction,
        cell: CellReferenceIndex,
    ) {
        let specs = sort_orders.unwrap_or_else(|| (1..=field_count as i32).collect());
        let mut decorated: Vec<(Group, Vec<ArrayNode>)> = groups
            .drain(..)
            .map(|group| {
                let keys = specs
                    .iter()
                    .map(|order| {
                        let index = order.unsigned_abs() as usize - 1;
                        if index < field_count {
                            group.labels[index].clone()
                        } else {
                            self.aggregate_rows(
                                function,
                                values,
                                &group.rows,
                                index - field_count,
                                cell,
                            )
                        }
                    })
                    .collect();
                (group, keys)
            })
            .collect();
        decorated.sort_by(|left, right| {
            for (position, order) in specs.iter().enumerate() {
                let comparison = compare_nodes(&left.1[position], &right.1[position]);
                if comparison != Ordering::Equal {
                    return if *order > 0 {
                        comparison
                    } else {
                        comparison.reverse()
                    };
                }
            }
            compare_label_vectors(&left.0.labels, &right.0.labels)
        });
        groups.extend(decorated.into_iter().map(|item| item.0));
    }

    fn validate_total_depth(
        &mut self,
        args: &[Node],
        index: usize,
        field_count: usize,
        cell: CellReferenceIndex,
    ) -> Result<i32, CalcResult> {
        let default = if field_count > 1 { 2 } else { 1 };
        let value = self.optional_integer(args, index, cell)?.unwrap_or(default);
        if value.unsigned_abs() as usize > field_count {
            return Err(CalcResult::new_error(
                Error::VALUE,
                cell,
                "total depth exceeds the number of grouping fields".to_string(),
            ));
        }
        Ok(value)
    }

    /// `GROUPBY(row_fields, values, function, [field_headers], [total_depth],
    /// [sort_order], [filter_array], [field_relationship])`.
    pub(crate) fn fn_groupby(&mut self, args: &[Node], cell: CellReferenceIndex) -> CalcResult {
        if !(3..=8).contains(&args.len()) {
            return CalcResult::new_args_number_error(cell);
        }

        let mut fields = match self.eval_to_array(&args[0], cell) {
            Ok(data) => data,
            Err(error) => return error,
        };
        let mut values = match self.eval_to_array(&args[1], cell) {
            Ok(data) => data,
            Err(error) => return error,
        };
        let Some(field_count) = rectangular_width(&fields) else {
            return CalcResult::new_error(
                Error::VALUE,
                cell,
                "row_fields must be a non-empty rectangular array".to_string(),
            );
        };
        let Some(value_count) = rectangular_width(&values) else {
            return CalcResult::new_error(
                Error::VALUE,
                cell,
                "values must be a non-empty rectangular array".to_string(),
            );
        };
        if fields.len() != values.len() {
            return CalcResult::new_error(
                Error::VALUE,
                cell,
                "row_fields and values must have the same height".to_string(),
            );
        }

        let original_rows = fields.len();
        let header_config = match self.header_config(args, 3, &values, field_count, cell) {
            Ok(config) => config,
            Err(error) => return error,
        };
        let mut field_headers = generated_headers("Row Field", field_count);
        let mut value_headers = generated_headers("Value", value_count);
        if header_config.consume {
            if fields.len() < 2 {
                return CalcResult::new_error(
                    Error::VALUE,
                    cell,
                    "header mode requires at least one data row".to_string(),
                );
            }
            field_headers = fields.remove(0);
            value_headers = values.remove(0);
        }

        let function = match self.parse_summary_function(&args[2], cell) {
            Ok(function) => function,
            Err(error) => return error,
        };
        let filter = match self.parse_filter(
            args.get(6),
            original_rows,
            fields.len(),
            header_config.consume,
            cell,
        ) {
            Ok(filter) => filter,
            Err(error) => return error,
        };
        let included_rows: Vec<usize> = filter
            .iter()
            .enumerate()
            .filter_map(|(index, include)| include.then_some(index))
            .collect();
        if included_rows.is_empty() {
            return CalcResult::new_error(
                Error::CALC,
                cell,
                "GROUPBY has no rows to aggregate".to_string(),
            );
        }

        let field_relationship = match self.optional_integer(args, 7, cell) {
            Ok(Some(value @ (0 | 1))) => value,
            Ok(None) => 0,
            Ok(Some(_)) => {
                return CalcResult::new_error(
                    Error::VALUE,
                    cell,
                    "field_relationship must be 0 or 1".to_string(),
                )
            }
            Err(error) => return error,
        };
        let mut total_depth = match self.validate_total_depth(args, 4, field_count, cell) {
            Ok(value) => value,
            Err(error) => return error,
        };
        // Excel's table relationship does not support hierarchy subtotals.
        if field_relationship == 1 && total_depth.unsigned_abs() > 1 {
            total_depth = total_depth.signum();
        }

        let sort_orders = match self.parse_sort_orders(args.get(5), field_count + value_count, cell)
        {
            Ok(value) => value,
            Err(error) => return error,
        };
        let mut groups = match build_groups(&fields, &included_rows) {
            Ok(groups) => groups,
            Err(error) => {
                return CalcResult::new_error(
                    error,
                    cell,
                    "row_fields contains an error".to_string(),
                )
            }
        };
        self.sort_groups(
            &mut groups,
            sort_orders,
            field_count,
            &values,
            &function,
            cell,
        );
        let entries = build_axis_entries(&groups, total_depth);

        let mut output = Vec::with_capacity(entries.len() + usize::from(header_config.show));
        if header_config.show {
            let mut header = field_headers;
            header.extend(value_headers);
            output.push(header);
        }
        for entry in entries {
            let mut row = entry.labels;
            for value_column in 0..value_count {
                row.push(self.aggregate_rows(&function, &values, &entry.rows, value_column, cell));
            }
            output.push(row);
        }

        if let Some((error, message)) = check_array_size(output.len(), field_count + value_count) {
            return CalcResult::new_error(error, cell, message);
        }
        CalcResult::Array(output)
    }

    /// `PIVOTBY(row_fields, col_fields, values, function, [field_headers],
    /// [row_total_depth], [row_sort_order], [col_total_depth], [col_sort_order],
    /// [filter_array], [relative_to])`.
    pub(crate) fn fn_pivotby(&mut self, args: &[Node], cell: CellReferenceIndex) -> CalcResult {
        if !(4..=11).contains(&args.len()) {
            return CalcResult::new_args_number_error(cell);
        }

        let mut row_fields = match self.eval_to_array(&args[0], cell) {
            Ok(data) => data,
            Err(error) => return error,
        };
        let mut col_fields = match self.eval_to_array(&args[1], cell) {
            Ok(data) => data,
            Err(error) => return error,
        };
        let mut values = match self.eval_to_array(&args[2], cell) {
            Ok(data) => data,
            Err(error) => return error,
        };
        let Some(row_field_count) = rectangular_width(&row_fields) else {
            return CalcResult::new_error(
                Error::VALUE,
                cell,
                "row_fields must be a non-empty rectangular array".to_string(),
            );
        };
        let Some(col_field_count) = rectangular_width(&col_fields) else {
            return CalcResult::new_error(
                Error::VALUE,
                cell,
                "col_fields must be a non-empty rectangular array".to_string(),
            );
        };
        let Some(value_count) = rectangular_width(&values) else {
            return CalcResult::new_error(
                Error::VALUE,
                cell,
                "values must be a non-empty rectangular array".to_string(),
            );
        };
        if row_fields.len() != col_fields.len() || row_fields.len() != values.len() {
            return CalcResult::new_error(
                Error::VALUE,
                cell,
                "row_fields, col_fields, and values must have the same height".to_string(),
            );
        }

        let original_rows = row_fields.len();
        let header_config =
            match self.header_config(args, 4, &values, row_field_count + col_field_count, cell) {
                Ok(config) => config,
                Err(error) => return error,
            };
        let mut row_headers = generated_headers("Row Field", row_field_count);
        let mut col_headers = generated_headers("Column Field", col_field_count);
        let mut value_headers = generated_headers("Value", value_count);
        if header_config.consume {
            if row_fields.len() < 2 {
                return CalcResult::new_error(
                    Error::VALUE,
                    cell,
                    "header mode requires at least one data row".to_string(),
                );
            }
            row_headers = row_fields.remove(0);
            col_headers = col_fields.remove(0);
            value_headers = values.remove(0);
        }

        let function = match self.parse_summary_function(&args[3], cell) {
            Ok(function) => function,
            Err(error) => return error,
        };
        let filter = match self.parse_filter(
            args.get(9),
            original_rows,
            row_fields.len(),
            header_config.consume,
            cell,
        ) {
            Ok(filter) => filter,
            Err(error) => return error,
        };
        let included_rows: Vec<usize> = filter
            .iter()
            .enumerate()
            .filter_map(|(index, include)| include.then_some(index))
            .collect();
        if included_rows.is_empty() {
            return CalcResult::new_error(
                Error::CALC,
                cell,
                "PIVOTBY has no rows to aggregate".to_string(),
            );
        }

        if let Some(relative_to) = match self.optional_integer(args, 10, cell) {
            Ok(value) => value,
            Err(error) => return error,
        } {
            if !(0..=4).contains(&relative_to) {
                return CalcResult::new_error(
                    Error::VALUE,
                    cell,
                    "relative_to must be between 0 and 4".to_string(),
                );
            }
            // relative_to affects percentage aggregators.  It is intentionally
            // neutral for SUM/AVERAGE/COUNT/MAX/MIN and ordinary lambdas.
        }

        let row_total_depth = match self.validate_total_depth(args, 5, row_field_count, cell) {
            Ok(value) => value,
            Err(error) => return error,
        };
        let col_total_depth = match self.validate_total_depth(args, 7, col_field_count, cell) {
            Ok(value) => value,
            Err(error) => return error,
        };
        let row_sort_orders =
            match self.parse_sort_orders(args.get(6), row_field_count + value_count, cell) {
                Ok(value) => value,
                Err(error) => return error,
            };
        let col_sort_orders =
            match self.parse_sort_orders(args.get(8), col_field_count + value_count, cell) {
                Ok(value) => value,
                Err(error) => return error,
            };

        let mut row_groups = match build_groups(&row_fields, &included_rows) {
            Ok(groups) => groups,
            Err(error) => {
                return CalcResult::new_error(
                    error,
                    cell,
                    "row_fields contains an error".to_string(),
                )
            }
        };
        let mut col_groups = match build_groups(&col_fields, &included_rows) {
            Ok(groups) => groups,
            Err(error) => {
                return CalcResult::new_error(
                    error,
                    cell,
                    "col_fields contains an error".to_string(),
                )
            }
        };
        self.sort_groups(
            &mut row_groups,
            row_sort_orders,
            row_field_count,
            &values,
            &function,
            cell,
        );
        self.sort_groups(
            &mut col_groups,
            col_sort_orders,
            col_field_count,
            &values,
            &function,
            cell,
        );
        let row_entries = build_axis_entries(&row_groups, row_total_depth);
        let col_entries = build_axis_entries(&col_groups, col_total_depth);

        let result_columns = row_field_count + col_entries.len() * value_count;
        let value_header_row = value_count > 1;
        let result_rows = col_field_count + usize::from(value_header_row) + row_entries.len();
        if let Some((error, message)) = check_array_size(result_rows, result_columns) {
            return CalcResult::new_error(error, cell, message);
        }

        let mut output = Vec::with_capacity(result_rows);
        for (level, col_header) in col_headers.iter().enumerate() {
            let mut header = vec![ArrayNode::Empty; row_field_count];
            if header_config.show && level + 1 == col_field_count && !value_header_row {
                header.clone_from_slice(&row_headers);
            } else if header_config.show && !header.is_empty() {
                header[0] = col_header.clone();
            }
            for entry in &col_entries {
                for value_column in 0..value_count {
                    if value_column == 0 {
                        header.push(entry.labels[level].clone());
                    } else {
                        header.push(ArrayNode::Empty);
                    }
                }
            }
            output.push(header);
        }
        if value_header_row {
            let mut header = if header_config.show {
                row_headers
            } else {
                vec![ArrayNode::Empty; row_field_count]
            };
            for _ in &col_entries {
                header.extend(value_headers.iter().cloned());
            }
            output.push(header);
        }

        let mut col_membership = Vec::with_capacity(col_entries.len());
        for entry in &col_entries {
            let mut membership = vec![false; values.len()];
            for &row in &entry.rows {
                membership[row] = true;
            }
            col_membership.push(membership);
        }
        for row_entry in row_entries {
            let mut result_row = row_entry.labels;
            for (col_index, _) in col_entries.iter().enumerate() {
                let intersection: Vec<usize> = row_entry
                    .rows
                    .iter()
                    .copied()
                    .filter(|row| col_membership[col_index][*row])
                    .collect();
                for value_column in 0..value_count {
                    result_row.push(self.aggregate_rows(
                        &function,
                        &values,
                        &intersection,
                        value_column,
                        cell,
                    ));
                }
            }
            output.push(result_row);
        }

        CalcResult::Array(output)
    }
}
