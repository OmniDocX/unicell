use crate::{
    cf_types::{CfRule, Cfvo},
    expressions::{
        parser::{
            move_formula::{move_formula, ref_is_in_area, MoveContext},
            stringify::to_localized_string,
            Node,
        },
        types::{Area, CellReferenceRC},
        utils,
    },
    language::get_default_language,
    locale::get_default_locale,
    model::Model,
    utils as common,
};

// ---------------------------------------------------------------------------
// sqref helpers
// ---------------------------------------------------------------------------

/// Updates a single sqref range part if both corners are fully inside the cut area.
fn cf_range_part_update_for_cut(part: &str, area: &Area, row_delta: i32, col_delta: i32) -> String {
    let upper = part.to_uppercase();
    let segs: Vec<&str> = upper.splitn(2, ':').collect();
    match segs.len() {
        1 => {
            if let Some(r) = utils::parse_reference_a1(segs[0]) {
                if ref_is_in_area(area.sheet, r.row, r.column, area) {
                    if let Some(c) = utils::number_to_column(r.column + col_delta) {
                        return format!("{}{}", c, r.row + row_delta);
                    }
                }
            }
            part.to_string()
        }
        2 => {
            if let (Some(r1), Some(r2)) = (
                utils::parse_reference_a1(segs[0]),
                utils::parse_reference_a1(segs[1]),
            ) {
                if ref_is_in_area(area.sheet, r1.row, r1.column, area)
                    && ref_is_in_area(area.sheet, r2.row, r2.column, area)
                {
                    if let (Some(c1), Some(c2)) = (
                        utils::number_to_column(r1.column + col_delta),
                        utils::number_to_column(r2.column + col_delta),
                    ) {
                        return format!(
                            "{}{}:{}{}",
                            c1,
                            r1.row + row_delta,
                            c2,
                            r2.row + row_delta
                        );
                    }
                }
            }
            part.to_string()
        }
        _ => part.to_string(),
    }
}

/// Maps a single CF sqref range part to the target location, intersecting with the copied area.
/// Returns `None` if the CF range part does not overlap the copy source.
fn map_cf_range_part_to_target(
    part: &str,
    src_r1: i32,
    src_c1: i32,
    src_r2: i32,
    src_c2: i32,
    tgt_row: i32,
    tgt_col: i32,
) -> Option<String> {
    let upper = part.to_uppercase();
    let segs: Vec<&str> = upper.splitn(2, ':').collect();
    let (rule_r1, rule_c1, rule_r2, rule_c2) = match segs.len() {
        1 => {
            let r = utils::parse_reference_a1(segs[0])?;
            (r.row, r.column, r.row, r.column)
        }
        2 => {
            let r1 = utils::parse_reference_a1(segs[0])?;
            let r2 = utils::parse_reference_a1(segs[1])?;
            (r1.row, r1.column, r2.row, r2.column)
        }
        _ => return None,
    };

    // Intersection with copy source
    let int_r1 = rule_r1.max(src_r1);
    let int_c1 = rule_c1.max(src_c1);
    let int_r2 = rule_r2.min(src_r2);
    let int_c2 = rule_c2.min(src_c2);
    if int_r1 > int_r2 || int_c1 > int_c2 {
        return None;
    }

    // Map intersection to target coordinates
    let new_r1 = tgt_row + (int_r1 - src_r1);
    let new_c1 = tgt_col + (int_c1 - src_c1);
    let new_r2 = tgt_row + (int_r2 - src_r1);
    let new_c2 = tgt_col + (int_c2 - src_c1);

    let c1 = utils::number_to_column(new_c1)?;
    let c2 = utils::number_to_column(new_c2)?;
    if new_r1 == new_r2 && new_c1 == new_c2 {
        Some(format!("{c1}{new_r1}"))
    } else {
        Some(format!("{c1}{new_r1}:{c2}{new_r2}"))
    }
}

/// Maps every part of a space-separated sqref string to the target location,
/// dropping parts that fall entirely outside the copy source.
fn map_cf_sqref_to_target(
    sqref: &str,
    src_r1: i32,
    src_c1: i32,
    src_r2: i32,
    src_c2: i32,
    tgt_row: i32,
    tgt_col: i32,
) -> String {
    sqref
        .split_whitespace()
        .filter_map(|p| {
            map_cf_range_part_to_target(p, src_r1, src_c1, src_r2, src_c2, tgt_row, tgt_col)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Updates every part of a space-separated sqref string for a cut operation.
fn cf_sqref_update_for_cut(sqref: &str, area: &Area, row_delta: i32, col_delta: i32) -> String {
    sqref
        .split_whitespace()
        .map(|p| cf_range_part_update_for_cut(p, area, row_delta, col_delta))
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CutRangeRelation {
    Disjoint,
    Contained,
    Partial,
}

fn cf_range_part_relation(part: &str, area: &Area) -> Option<CutRangeRelation> {
    let upper = part.to_uppercase();
    let (first, second) = upper.split_once(':').unwrap_or((&upper, &upper));
    let first = utils::parse_reference_a1(first)?;
    let second = utils::parse_reference_a1(second)?;
    let (r0, c0, r1, c1) = (
        first.row.min(second.row),
        first.column.min(second.column),
        first.row.max(second.row),
        first.column.max(second.column),
    );
    let area_r1 = area.row + area.height - 1;
    let area_c1 = area.column + area.width - 1;
    if r1 < area.row || r0 > area_r1 || c1 < area.column || c0 > area_c1 {
        return Some(CutRangeRelation::Disjoint);
    }
    if r0 >= area.row && r1 <= area_r1 && c0 >= area.column && c1 <= area_c1 {
        Some(CutRangeRelation::Contained)
    } else {
        Some(CutRangeRelation::Partial)
    }
}

fn reference_rectangle_relation(
    sheet: u32,
    row1: i32,
    column1: i32,
    row2: i32,
    column2: i32,
    area: &Area,
) -> CutRangeRelation {
    if sheet != area.sheet {
        return CutRangeRelation::Disjoint;
    }
    let (row0, row1) = (row1.min(row2), row1.max(row2));
    let (column0, column1) = (column1.min(column2), column1.max(column2));
    let area_last_row = area.row + area.height - 1;
    let area_last_column = area.column + area.width - 1;
    if row1 < area.row
        || row0 > area_last_row
        || column1 < area.column
        || column0 > area_last_column
    {
        CutRangeRelation::Disjoint
    } else if row0 >= area.row
        && row1 <= area_last_row
        && column0 >= area.column
        && column1 <= area_last_column
    {
        CutRangeRelation::Contained
    } else {
        CutRangeRelation::Partial
    }
}

/// Returns the (row, column) of the top-left cell in a sqref string.
pub(crate) fn cf_sqref_anchor(sqref: &str) -> Option<(i32, i32)> {
    let part = sqref.split_whitespace().next()?;
    let upper = part.to_uppercase();
    let first = upper.split(':').next()?;
    let r = utils::parse_reference_a1(first)?;
    Some((r.row, r.column))
}

fn move_cfvo_formula<F>(cfvo: Cfvo, move_formula: &mut F) -> Result<Cfvo, String>
where
    F: FnMut(&str) -> Result<String, String>,
{
    match cfvo {
        Cfvo::Formula(formula) => Ok(Cfvo::Formula(move_formula(&formula)?)),
        other => Ok(other),
    }
}

// ---------------------------------------------------------------------------
// Model methods
// ---------------------------------------------------------------------------

impl<'a> Model<'a> {
    /// Builds the conditional-formatting part of a cross-sheet cut before any
    /// worksheet cell is changed. Excel can split partially selected rules, but
    /// doing that without also splitting their relative formula anchor is unsafe;
    /// reject those cases atomically instead of leaving a rule on the wrong sheet.
    pub(crate) fn get_cross_sheet_conditional_formatting_moves_for_cut(
        &mut self,
        area: &Area,
        target_sheet: u32,
        target_row: i32,
        target_column: i32,
    ) -> Result<Vec<(usize, String, CfRule)>, String> {
        let row_delta = target_row - area.row;
        let column_delta = target_column - area.column;
        let source_sheet_name = self
            .workbook
            .worksheets
            .get(area.sheet as usize)
            .ok_or_else(|| format!("Sheet {} not found", area.sheet))?
            .get_name();
        let target_sheet_name = self
            .workbook
            .worksheets
            .get(target_sheet as usize)
            .ok_or_else(|| format!("Sheet {target_sheet} not found"))?
            .get_name();
        let entries: Vec<(String, CfRule)> = self.workbook.worksheets[area.sheet as usize]
            .conditional_formatting
            .iter()
            .map(|entry| (entry.range.clone(), entry.cf_rule.clone()))
            .collect();
        let mut moves = Vec::new();
        for (index, (range, rule)) in entries.into_iter().enumerate() {
            let mut contained = 0usize;
            let mut disjoint = 0usize;
            for part in range.split_whitespace() {
                match cf_range_part_relation(part, area).ok_or_else(|| {
                    format!("Cannot cut conditional formatting with invalid range '{part}'")
                })? {
                    CutRangeRelation::Disjoint => disjoint += 1,
                    CutRangeRelation::Contained => contained += 1,
                    CutRangeRelation::Partial => {
                        return Err(format!(
                            "Cannot cut part of conditional-formatting range '{part}'"
                        ));
                    }
                }
            }
            if contained == 0 {
                continue;
            }
            if disjoint != 0 {
                return Err(
                    "Cannot cross-sheet cut only part of a multi-area conditional-formatting rule"
                        .to_string(),
                );
            }
            let new_range = map_cf_sqref_to_target(
                &range,
                area.row,
                area.column,
                area.row + area.height - 1,
                area.column + area.width - 1,
                target_row,
                target_column,
            );
            let (anchor_row, anchor_col) = cf_sqref_anchor(&range)
                .ok_or_else(|| format!("Conditional-formatting range '{range}' has no anchor"))?;
            let new_rule = self.cf_rule_move_formulas(
                rule,
                &source_sheet_name,
                &target_sheet_name,
                anchor_row,
                anchor_col,
                area,
                row_delta,
                column_delta,
            )?;
            moves.push((index, new_range, new_rule));
        }
        Ok(moves)
    }

    #[allow(clippy::too_many_arguments)]
    fn retarget_cut_references_in_node(
        node: &mut Node,
        formula_row: i32,
        formula_column: i32,
        area: &Area,
        target_sheet: u32,
        target_sheet_name: &str,
        row_delta: i32,
        column_delta: i32,
    ) -> Result<bool, String> {
        match node {
            Node::ReferenceKind {
                sheet_name,
                sheet_index,
                absolute_row,
                absolute_column,
                row,
                column,
            } => {
                let actual_row = if *absolute_row {
                    *row
                } else {
                    *row + formula_row
                };
                let actual_column = if *absolute_column {
                    *column
                } else {
                    *column + formula_column
                };
                if ref_is_in_area(*sheet_index, actual_row, actual_column, area) {
                    *row += row_delta;
                    *column += column_delta;
                    *sheet_index = target_sheet;
                    // Keep an explicit target sheet. This is required when the observing formula
                    // stays on another worksheet and is harmless (and Excel-compatible) locally.
                    *sheet_name = Some(target_sheet_name.to_string());
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
            Node::RangeKind {
                sheet_name,
                sheet_index,
                absolute_row1,
                absolute_column1,
                row1,
                column1,
                absolute_row2,
                absolute_column2,
                row2,
                column2,
            } => {
                let actual_row1 = if *absolute_row1 {
                    *row1
                } else {
                    *row1 + formula_row
                };
                let actual_column1 = if *absolute_column1 {
                    *column1
                } else {
                    *column1 + formula_column
                };
                let actual_row2 = if *absolute_row2 {
                    *row2
                } else {
                    *row2 + formula_row
                };
                let actual_column2 = if *absolute_column2 {
                    *column2
                } else {
                    *column2 + formula_column
                };
                match reference_rectangle_relation(
                    *sheet_index,
                    actual_row1,
                    actual_column1,
                    actual_row2,
                    actual_column2,
                    area,
                ) {
                    CutRangeRelation::Disjoint => Ok(false),
                    CutRangeRelation::Contained => {
                        *row1 += row_delta;
                        *column1 += column_delta;
                        *row2 += row_delta;
                        *column2 += column_delta;
                        *sheet_index = target_sheet;
                        *sheet_name = Some(target_sheet_name.to_string());
                        Ok(true)
                    }
                    CutRangeRelation::Partial => Err(
                        "Cannot cut because a formula range only partially intersects the source"
                            .to_string(),
                    ),
                }
            }
            Node::OpRangeKind { left, right } => {
                let left_changed = Self::retarget_cut_references_in_node(
                    left,
                    formula_row,
                    formula_column,
                    area,
                    target_sheet,
                    target_sheet_name,
                    row_delta,
                    column_delta,
                )?;
                let right_changed = Self::retarget_cut_references_in_node(
                    right,
                    formula_row,
                    formula_column,
                    area,
                    target_sheet,
                    target_sheet_name,
                    row_delta,
                    column_delta,
                )?;
                if left_changed || right_changed {
                    Err("Cannot safely rewrite a dynamic range operator during cut".to_string())
                } else {
                    Ok(false)
                }
            }
            Node::OpConcatenateKind { left, right }
            | Node::OpSumKind { left, right, .. }
            | Node::OpProductKind { left, right, .. }
            | Node::OpPowerKind { left, right }
            | Node::CompareKind { left, right, .. } => {
                let left_changed = Self::retarget_cut_references_in_node(
                    left,
                    formula_row,
                    formula_column,
                    area,
                    target_sheet,
                    target_sheet_name,
                    row_delta,
                    column_delta,
                )?;
                let right_changed = Self::retarget_cut_references_in_node(
                    right,
                    formula_row,
                    formula_column,
                    area,
                    target_sheet,
                    target_sheet_name,
                    row_delta,
                    column_delta,
                )?;
                Ok(left_changed || right_changed)
            }
            Node::FunctionKind { args, .. } | Node::NamedFunctionKind { args, .. } => {
                let mut changed = false;
                for argument in args {
                    changed |= Self::retarget_cut_references_in_node(
                        argument,
                        formula_row,
                        formula_column,
                        area,
                        target_sheet,
                        target_sheet_name,
                        row_delta,
                        column_delta,
                    )?;
                }
                Ok(changed)
            }
            Node::LambdaDefKind { body, .. }
            | Node::ImplicitIntersection { child: body, .. }
            | Node::SpillRangeOperator { child: body }
            | Node::UnaryKind { right: body, .. } => Self::retarget_cut_references_in_node(
                body,
                formula_row,
                formula_column,
                area,
                target_sheet,
                target_sheet_name,
                row_delta,
                column_delta,
            ),
            Node::LambdaCallKind { lambda, args } => {
                let mut changed = Self::retarget_cut_references_in_node(
                    lambda,
                    formula_row,
                    formula_column,
                    area,
                    target_sheet,
                    target_sheet_name,
                    row_delta,
                    column_delta,
                )?;
                for argument in args {
                    changed |= Self::retarget_cut_references_in_node(
                        argument,
                        formula_row,
                        formula_column,
                        area,
                        target_sheet,
                        target_sheet_name,
                        row_delta,
                        column_delta,
                    )?;
                }
                Ok(changed)
            }
            Node::WrongReferenceKind { .. }
            | Node::WrongRangeKind { .. }
            | Node::ParseErrorKind { .. } => Err(
                "Cannot safely rewrite an invalid or unsupported formula during cut".to_string(),
            ),
            Node::BooleanKind(_)
            | Node::NumberKind(_)
            | Node::StringKind(_)
            | Node::ArrayKind(_)
            | Node::DefinedNameKind(_)
            | Node::TableNameKind(_)
            | Node::NamedVariableKind { .. }
            | Node::ErrorKind(_)
            | Node::EmptyArgKind => Ok(false),
        }
    }

    /// Returns updated formula strings for all cells whose formulas reference
    /// cells inside `area`, excluding cells that are themselves inside `area`.
    /// Used during cut-paste to propagate the move to external observers.
    ///
    /// Returns `(sheet_index, row, column, new_formula_string)` for each cell
    /// whose formula changed.
    pub(crate) fn get_external_formula_updates_for_cut(
        &mut self,
        area: &Area,
        target_sheet: u32,
        target_row: i32,
        target_column: i32,
    ) -> Result<Vec<(u32, i32, i32, String)>, String> {
        let row_delta = target_row - area.row;
        let column_delta = target_column - area.column;
        if target_sheet == area.sheet && row_delta == 0 && column_delta == 0 {
            return Ok(vec![]);
        }

        let target_sheet_name = self
            .workbook
            .worksheets
            .get(target_sheet as usize)
            .ok_or_else(|| format!("Sheet {target_sheet} not found"))?
            .get_name();

        let num_sheets = self.workbook.worksheets.len();

        // Phase 1 – collect formula cells outside the cut area (immutable reads only)
        let mut candidates: Vec<(u32, i32, i32, String)> = Vec::new();
        for ws_idx in 0..num_sheets {
            let ws_idx_u32 = ws_idx as u32;
            // collect (row, col) pairs first to avoid holding the ws borrow
            let formula_positions: Vec<(i32, i32)> = {
                let ws = &self.workbook.worksheets[ws_idx];
                ws.sheet_data
                    .iter()
                    .flat_map(|(&row, col_map)| {
                        col_map.iter().filter_map(move |(&col, cell)| {
                            cell.get_formula()?;
                            // skip cells inside the area being moved
                            if ws_idx_u32 == area.sheet
                                && row >= area.row
                                && row < area.row + area.height
                                && col >= area.column
                                && col < area.column + area.width
                            {
                                return None;
                            }
                            // A formula currently in the destination rectangle is overwritten by
                            // the move. It is not an external observer and must not be written back
                            // after the target has been cleared.
                            if ws_idx_u32 == target_sheet
                                && row >= target_row
                                && row < target_row + area.height
                                && col >= target_column
                                && col < target_column + area.width
                            {
                                return None;
                            }
                            Some((row, col))
                        })
                    })
                    .collect()
            };
            // now collect the user-facing formula strings
            for (row, col) in formula_positions {
                let formula_str = self.get_localized_cell_content(ws_idx_u32, row, col)?;
                candidates.push((ws_idx_u32, row, col, formula_str));
            }
        }

        // Phase 2 – rewrite references that land inside the moved area
        let mut updates: Vec<(u32, i32, i32, String)> = Vec::new();
        for (ws_idx_u32, row, col, formula_str) in candidates {
            let sheet_name = self.workbook.worksheets[ws_idx_u32 as usize].get_name();
            let formula_body = match self.formula_without_prefix(&formula_str) {
                Some(s) => s.to_owned(),
                None => continue,
            };
            let cell_ref = CellReferenceRC {
                sheet: sheet_name.clone(),
                row,
                column: col,
            };
            let mut node = self.parser.parse(&formula_body, &cell_ref);
            let new_body = if target_sheet == area.sheet {
                // `move_formula` deliberately leaves partially intersecting ranges alone. That
                // would make an observing formula keep pointing at cleared source cells, so run
                // the strict checker on a disposable AST before accepting the cut.
                let mut validation_node = node.clone();
                Self::retarget_cut_references_in_node(
                    &mut validation_node,
                    row,
                    col,
                    area,
                    target_sheet,
                    &target_sheet_name,
                    row_delta,
                    column_delta,
                )?;
                move_formula(
                    &node,
                    &MoveContext {
                        source_sheet_name: &sheet_name,
                        row,
                        column: col,
                        area,
                        target_sheet_name: &sheet_name,
                        row_delta,
                        column_delta,
                    },
                    self.locale,
                    self.language,
                )
            } else {
                Self::retarget_cut_references_in_node(
                    &mut node,
                    row,
                    col,
                    area,
                    target_sheet,
                    &target_sheet_name,
                    row_delta,
                    column_delta,
                )?;
                to_localized_string(&node, &cell_ref, self.locale, self.language)
            };
            let new_formula = format!("={new_body}");
            if new_formula != formula_str {
                updates.push((ws_idx_u32, row, col, new_formula));
            }
        }

        Ok(updates)
    }

    /// Returns updated formula strings for all defined names whose cell or range
    /// reference falls entirely inside `area`, after a cut-paste that moves `area`
    /// to (`target_row`, `target_column`).
    ///
    /// Returns `(name, scope_sheet_index, old_formula, new_formula)` for each
    /// defined name whose formula changed.
    pub(crate) fn get_defined_name_updates_for_cut(
        &mut self,
        area: &Area,
        target_sheet: u32,
        target_row: i32,
        target_column: i32,
    ) -> Result<Vec<(String, Option<u32>, String, String)>, String> {
        let row_delta = target_row - area.row;
        let column_delta = target_column - area.column;
        if target_sheet == area.sheet && row_delta == 0 && column_delta == 0 {
            return Ok(vec![]);
        }

        let target_sheet_name = self
            .workbook
            .worksheets
            .get(target_sheet as usize)
            .ok_or_else(|| format!("Sheet {target_sheet} not found"))?
            .get_name();
        let quoted_target_sheet_name = utils::quote_name(&target_sheet_name);
        let context = self.defined_name_context();

        let names_with_scope = self.workbook.get_defined_names_with_scope();
        let mut updates = Vec::new();

        for (name, scope, formula) in names_with_scope {
            let parsed = common::ParsedReference::parse_reference_formula(
                None,
                &formula,
                self.locale,
                |n| self.get_sheet_index_by_name(n),
            );

            let new_formula = match parsed {
                Ok(common::ParsedReference::CellReference(cell_ref)) => {
                    if !ref_is_in_area(cell_ref.sheet, cell_ref.row, cell_ref.column, area) {
                        continue;
                    }
                    let new_row = cell_ref.row + row_delta;
                    let new_col = cell_ref.column + column_delta;
                    let col_str = utils::number_to_column(new_col).ok_or_else(|| {
                        format!("Cannot cut defined name '{name}' outside worksheet bounds")
                    })?;
                    if new_row < 1 {
                        return Err(format!(
                            "Cannot cut defined name '{name}' outside worksheet bounds"
                        ));
                    }
                    format!("{quoted_target_sheet_name}!${col_str}${new_row}")
                }
                Ok(common::ParsedReference::Range(left, right)) => {
                    if left.sheet != right.sheet {
                        if left.sheet == area.sheet || right.sheet == area.sheet {
                            return Err(format!(
                                "Cannot safely rewrite multi-sheet defined name '{name}' during cut"
                            ));
                        }
                        continue;
                    }
                    match reference_rectangle_relation(
                        left.sheet,
                        left.row,
                        left.column,
                        right.row,
                        right.column,
                        area,
                    ) {
                        CutRangeRelation::Disjoint => continue,
                        CutRangeRelation::Partial => {
                            return Err(format!(
                                "Cannot cut part of the range used by defined name '{name}'"
                            ));
                        }
                        CutRangeRelation::Contained => {}
                    }
                    let new_row1 = left.row + row_delta;
                    let new_col1 = left.column + column_delta;
                    let new_row2 = right.row + row_delta;
                    let new_col2 = right.column + column_delta;
                    let (Some(col1_str), Some(col2_str)) = (
                        utils::number_to_column(new_col1),
                        utils::number_to_column(new_col2),
                    ) else {
                        return Err(format!(
                            "Cannot cut defined name '{name}' outside worksheet bounds"
                        ));
                    };
                    if new_row1 < 1 || new_row2 < 1 {
                        return Err(format!(
                            "Cannot cut defined name '{name}' outside worksheet bounds"
                        ));
                    }
                    format!(
                        "{quoted_target_sheet_name}!${col1_str}${new_row1}:${col2_str}${new_row2}"
                    )
                }
                Err(_) => {
                    // Imported workbooks can contain formula-valued and union-valued names, not
                    // only plain references. Parse and retarget their AST so no reference to a
                    // cleared source cell is silently retained. Unsupported/partial structures
                    // are rejected here, before clipboard writes begin.
                    let trimmed = formula.trim();
                    let has_eq = trimmed.starts_with('=');
                    let body = if has_eq { &trimmed[1..] } else { trimmed };
                    let mut node = self.parse_internal_formula(body, &context);
                    let changed = Self::retarget_cut_references_in_node(
                        &mut node,
                        context.row,
                        context.column,
                        area,
                        target_sheet,
                        &target_sheet_name,
                        row_delta,
                        column_delta,
                    )
                    .map_err(|error| format!("Defined name '{name}': {error}"))?;
                    if !changed {
                        continue;
                    }
                    let new_body = to_localized_string(
                        &node,
                        &context,
                        get_default_locale(),
                        get_default_language(),
                    );
                    if has_eq {
                        format!("={new_body}")
                    } else {
                        new_body
                    }
                }
            };

            if new_formula != formula {
                updates.push((name, scope, formula, new_formula));
            }
        }

        Ok(updates)
    }

    /// Returns updated range strings and CF rules for all conditional formatting entries
    /// on `area.sheet` whose applied range or formula references land inside `area`,
    /// after a cut-paste that moves `area` to (`target_row`, `target_column`).
    ///
    /// Returns `(sheet, cf_idx, new_range, new_rule)` for each entry that changed.
    pub(crate) fn get_conditional_formatting_updates_for_cut(
        &mut self,
        area: &Area,
        target_row: i32,
        target_column: i32,
    ) -> Result<Vec<(u32, usize, String, CfRule)>, String> {
        let row_delta = target_row - area.row;
        let column_delta = target_column - area.column;
        if row_delta == 0 && column_delta == 0 {
            return Ok(vec![]);
        }

        let sheet = area.sheet;
        let sheet_name = self
            .workbook
            .worksheets
            .get(sheet as usize)
            .ok_or_else(|| format!("Sheet {sheet} not found"))?
            .get_name();

        // Phase 1 – collect CF data (immutable reads)
        let cf_entries: Vec<(String, CfRule)> = self.workbook.worksheets[sheet as usize]
            .conditional_formatting
            .iter()
            .map(|cf| (cf.range.clone(), cf.cf_rule.clone()))
            .collect();

        // Phase 2 – compute updates (may need &mut self.parser)
        let mut updates = Vec::new();
        for (cf_idx, (old_range, old_rule)) in cf_entries.into_iter().enumerate() {
            let mut contained = 0usize;
            let mut disjoint = 0usize;
            for part in old_range.split_whitespace() {
                match cf_range_part_relation(part, area).ok_or_else(|| {
                    format!("Cannot cut conditional formatting with invalid range '{part}'")
                })? {
                    CutRangeRelation::Disjoint => disjoint += 1,
                    CutRangeRelation::Contained => contained += 1,
                    CutRangeRelation::Partial => {
                        return Err(format!(
                            "Cannot cut part of conditional-formatting range '{part}'"
                        ));
                    }
                }
            }
            if contained != 0 && disjoint != 0 {
                return Err(
                    "Cannot cut only part of a multi-area conditional-formatting rule".to_string(),
                );
            }
            let new_range = cf_sqref_update_for_cut(&old_range, area, row_delta, column_delta);

            // Use the top-left cell of the CF range as the formula parse anchor.
            let anchor = cf_sqref_anchor(&old_range);
            let new_rule = if let Some((anchor_row, anchor_col)) = anchor {
                self.cf_rule_move_formulas(
                    old_rule.clone(),
                    &sheet_name,
                    &sheet_name,
                    anchor_row,
                    anchor_col,
                    area,
                    row_delta,
                    column_delta,
                )?
            } else {
                old_rule.clone()
            };

            if new_range != old_range || new_rule != old_rule {
                updates.push((sheet, cf_idx, new_range, new_rule));
            }
        }

        Ok(updates)
    }

    /// Updates formula fields inside a `CfRule` using `move_formula`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn cf_rule_move_formulas(
        &mut self,
        rule: CfRule,
        source_sheet_name: &str,
        target_sheet_name: &str,
        anchor_row: i32,
        anchor_col: i32,
        area: &Area,
        row_delta: i32,
        col_delta: i32,
    ) -> Result<CfRule, String> {
        let target_sheet = self
            .get_sheet_index_by_name(target_sheet_name)
            .ok_or_else(|| format!("Sheet '{target_sheet_name}' not found"))?;
        let mut move_f = |formula: &str| -> Result<String, String> {
            let trimmed = formula.trim();
            let has_eq = trimmed.starts_with('=');
            let body = if has_eq { &trimmed[1..] } else { trimmed };
            let cell_ref = CellReferenceRC {
                sheet: source_sheet_name.to_string(),
                row: anchor_row,
                column: anchor_col,
            };
            // CF formulas are stored internally in English.
            let node = self.parse_internal_formula(body, &cell_ref);
            // Validate on a clone before asking `move_formula` to transform the formula. In
            // particular, `move_formula` intentionally preserves partial ranges; that is unsafe
            // for a cut because the source cells are about to be cleared.
            let mut validation_node = node.clone();
            Self::retarget_cut_references_in_node(
                &mut validation_node,
                anchor_row,
                anchor_col,
                area,
                target_sheet,
                target_sheet_name,
                row_delta,
                col_delta,
            )?;
            let new_body = move_formula(
                &node,
                &MoveContext {
                    source_sheet_name,
                    row: anchor_row,
                    column: anchor_col,
                    area,
                    target_sheet_name,
                    row_delta,
                    column_delta: col_delta,
                },
                get_default_locale(),
                get_default_language(),
            );
            if has_eq {
                Ok(format!("={new_body}"))
            } else {
                Ok(new_body)
            }
        };

        Ok(match rule {
            CfRule::Formula {
                formula,
                dxf_id,
                stop_if_true,
            } => CfRule::Formula {
                formula: move_f(&formula)?,
                dxf_id,
                stop_if_true,
            },
            CfRule::CellIs {
                operator,
                formula,
                formula2,
                dxf_id,
                stop_if_true,
            } => CfRule::CellIs {
                operator,
                formula: move_f(&formula)?,
                formula2: formula2.as_deref().map(&mut move_f).transpose()?,
                dxf_id,
                stop_if_true,
            },
            CfRule::ColorScale { mut thresholds } => {
                for threshold in &mut thresholds {
                    threshold.cfvo = move_cfvo_formula(threshold.cfvo.clone(), &mut move_f)?;
                }
                CfRule::ColorScale { thresholds }
            }
            CfRule::DataBar {
                min,
                max,
                positive_color,
                negative_color,
                is_gradient,
                show_value,
            } => CfRule::DataBar {
                min: min
                    .map(|value| move_cfvo_formula(value, &mut move_f))
                    .transpose()?,
                max: max
                    .map(|value| move_cfvo_formula(value, &mut move_f))
                    .transpose()?,
                positive_color,
                negative_color,
                is_gradient,
                show_value,
            },
            CfRule::IconSet {
                mut thresholds,
                show_value,
            } => {
                for threshold in &mut thresholds {
                    threshold.cfvo = move_cfvo_formula(threshold.cfvo.clone(), &mut move_f)?;
                }
                CfRule::IconSet {
                    thresholds,
                    show_value,
                }
            }
            CfRule::IconRating {
                icon,
                color,
                thresholds,
                show_value,
            } => CfRule::IconRating {
                icon,
                color,
                thresholds: thresholds
                    .into_iter()
                    .map(|(value, strict)| Ok((move_cfvo_formula(value, &mut move_f)?, strict)))
                    .collect::<Result<Vec<_>, String>>()?,
                show_value,
            },
            other => other,
        })
    }

    /// Returns CF rules to add when copy-pasting cells from `source_sheet`.
    ///
    /// For each CF rule on `source_sheet` that overlaps the copied rectangle
    /// (`src_row1..src_row2`, `src_col1..src_col2`), computes the intersection
    /// and maps it to the target location starting at (`tgt_row`, `tgt_col`).
    ///
    /// Returns `(new_range_sqref, cf_rule)` for each overlapping CF entry.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn get_cf_rules_to_copy(
        &self,
        source_sheet: u32,
        src_row1: i32,
        src_col1: i32,
        src_row2: i32,
        src_col2: i32,
        tgt_row: i32,
        tgt_col: i32,
    ) -> Vec<(String, CfRule)> {
        let ws = match self.workbook.worksheets.get(source_sheet as usize) {
            Some(ws) => ws,
            None => return vec![],
        };

        let mut results = Vec::new();
        for cf in &ws.conditional_formatting {
            let new_range = map_cf_sqref_to_target(
                &cf.range, src_row1, src_col1, src_row2, src_col2, tgt_row, tgt_col,
            );
            if !new_range.is_empty() {
                results.push((new_range, cf.cf_rule.clone()));
            }
        }
        results
    }
}
