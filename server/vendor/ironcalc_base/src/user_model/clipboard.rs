#![deny(missing_docs)]

use std::{
    collections::{HashMap, HashSet},
    io::Cursor,
};

#[cfg(test)]
use std::cell::Cell as ThreadCell;

use csv::{ReaderBuilder, WriterBuilder};
use serde::{Deserialize, Serialize};

use crate::{
    cf_types::ConditionalFormatting,
    expressions::types::{Area, CellReferenceIndex},
    model::CellStructure,
    types::{ArrayKind, Cell, Style},
    UserModel,
};

use crate::user_model::history::Diff;

#[cfg(test)]
thread_local! {
    static CLIPBOARD_PASTE_WRITES_BEFORE_FAILURE: ThreadCell<usize> = const { ThreadCell::new(0) };
}

/// Arms the clipboard-paste failpoint used by atomicity regression tests.
///
/// A value of one fails immediately after the first target cell has been
/// written. The failpoint is thread-local so parallel tests cannot interfere.
#[cfg(test)]
pub(crate) fn fail_clipboard_paste_after_writes(writes: usize) {
    CLIPBOARD_PASTE_WRITES_BEFORE_FAILURE.with(|remaining| remaining.set(writes));
}

#[cfg(test)]
fn clipboard_paste_write_failpoint() -> Result<(), String> {
    CLIPBOARD_PASTE_WRITES_BEFORE_FAILURE.with(|remaining| match remaining.get() {
        0 => Ok(()),
        1 => {
            remaining.set(0);
            Err("Injected clipboard paste failure".to_string())
        }
        count => {
            remaining.set(count - 1);
            Ok(())
        }
    })
}

#[cfg(not(test))]
#[inline]
fn clipboard_paste_write_failpoint() -> Result<(), String> {
    Ok(())
}

/// Data for the clipboard
pub type ClipboardData = HashMap<i32, HashMap<i32, ClipboardCell>>;

pub type ClipboardTuple = (i32, i32, i32, i32);

#[derive(Serialize, Deserialize)]
pub struct ClipboardCell {
    text: String,
    is_spill: bool,
    style: Style,
}

#[derive(Serialize, Deserialize)]
pub struct Clipboard {
    pub(crate) csv: String,
    pub(crate) data: ClipboardData,
    pub(crate) sheet: u32,
    pub(crate) range: (i32, i32, i32, i32),
}

impl<'a> UserModel<'a> {
    fn validate_cut_array_area(
        &self,
        area: &Area,
        reject_static_source_arrays: bool,
        label: &str,
        permitted_dynamic_anchor_area: Option<&Area>,
    ) -> Result<(), String> {
        let area_last_row = area.row + area.height - 1;
        let area_last_column = area.column + area.width - 1;
        for row in area.row..=area_last_row {
            for column in area.column..=area_last_column {
                let structure = self.model.get_cell_structure(area.sheet, row, column)?;
                let (anchor_row, anchor_column, width, height, is_static) = match structure {
                    CellStructure::SingleCell => continue,
                    CellStructure::ArrayFormula {
                        range: (width, height),
                    } => (row, column, width, height, true),
                    CellStructure::DynamicFormula { .. } => {
                        // The anchor owns its dynamic spill. Moving or replacing that anchor is
                        // safe even when the selection does not include every generated child.
                        continue;
                    }
                    CellStructure::SpillArray {
                        anchor: (anchor_row, anchor_column),
                        range: (width, height),
                    } => (anchor_row, anchor_column, width, height, true),
                    CellStructure::SpillDynamic { anchor, .. } => {
                        let (anchor_row, anchor_column) = anchor;
                        let anchor_is_selected = anchor_row >= area.row
                            && anchor_row <= area_last_row
                            && anchor_column >= area.column
                            && anchor_column <= area_last_column;
                        let anchor_is_being_moved =
                            permitted_dynamic_anchor_area.is_some_and(|source| {
                                source.sheet == area.sheet
                                    && anchor_row >= source.row
                                    && anchor_row < source.row + source.height
                                    && anchor_column >= source.column
                                    && anchor_column < source.column + source.width
                            });
                        if anchor_is_selected || anchor_is_being_moved {
                            continue;
                        }
                        return Err(format!(
                            "Cannot cut because the {label} range partially overlaps a dynamic array spill"
                        ));
                    }
                };
                if reject_static_source_arrays && is_static {
                    return Err(
                        "Cannot cut a legacy array formula; move its complete formula definition in Excel"
                            .to_string(),
                    );
                }
                if anchor_row < area.row
                    || anchor_column < area.column
                    || anchor_row + height - 1 > area_last_row
                    || anchor_column + width - 1 > area_last_column
                {
                    return Err(format!(
                        "Cannot cut because the {label} range partially overlaps an array formula"
                    ));
                }
            }
        }
        Ok(())
    }

    /// Returns a copy of the selected area
    pub fn copy_to_clipboard(&self) -> Result<Clipboard, String> {
        let selected_area = self.get_selected_view();
        let sheet = selected_area.sheet;
        let mut wtr = WriterBuilder::new().delimiter(b'\t').from_writer(vec![]);

        let mut data = HashMap::new();
        let [row_start, column_start, row_end, column_end] = selected_area.range;
        let dimension = self.model.workbook.worksheet(sheet)?.dimension();
        let row_end = row_end.min(dimension.max_row).max(row_start);
        let column_end = column_end.min(dimension.max_column).max(column_start);
        for row in row_start..=row_end {
            let mut data_row = HashMap::new();
            let mut text_row = Vec::new();
            for column in column_start..=column_end {
                let text = self.get_formatted_cell_value(sheet, row, column)?;
                let content = self.get_cell_content(sheet, row, column)?;
                let style = self.model.get_style_for_cell(sheet, row, column)?;
                let is_spill = matches!(
                    self.model.get_cell_structure(sheet, row, column)?,
                    CellStructure::SpillArray { .. } | CellStructure::SpillDynamic { .. }
                );
                data_row.insert(
                    column,
                    ClipboardCell {
                        text: content,
                        is_spill,
                        style,
                    },
                );
                text_row.push(text);
            }
            wtr.write_record(text_row)
                .map_err(|e| format!("Error while processing csv: {e}"))?;
            data.insert(row, data_row);
        }

        let csv = String::from_utf8(
            wtr.into_inner()
                .map_err(|e| format!("Processing error: '{e}'"))?,
        )
        .map_err(|e| format!("Error converting from utf8: '{e}'"))?;

        Ok(Clipboard {
            csv: csv.trim().to_string(),
            data,
            sheet,
            range: (row_start, column_start, row_end, column_end),
        })
    }

    /// Paste text that we copied
    pub fn paste_from_clipboard(
        &mut self,
        source_sheet: u32,
        source_range: ClipboardTuple,
        clipboard: &ClipboardData,
        is_cut: bool,
    ) -> Result<(), String> {
        self.paste_from_clipboard_impl(source_sheet, source_range, clipboard, is_cut, true)
    }

    /// Pastes a self-contained clipboard payload that originated in another
    /// workbook/application. Cell contents and styles are portable, while
    /// source-workbook conditional-formatting lookups are deliberately skipped;
    /// the embedding application can carry those rules in its own rich payload.
    pub fn paste_from_external_clipboard(
        &mut self,
        source_range: ClipboardTuple,
        clipboard: &ClipboardData,
    ) -> Result<(), String> {
        let target_sheet = self.get_selected_view().sheet;
        self.paste_from_clipboard_impl(target_sheet, source_range, clipboard, false, false)
    }

    fn paste_from_clipboard_impl(
        &mut self,
        source_sheet: u32,
        source_range: ClipboardTuple,
        clipboard: &ClipboardData,
        is_cut: bool,
        copy_conditional_formatting: bool,
    ) -> Result<(), String> {
        // Keep an exact runtime snapshot, not just a workbook serialization. A paste can fail
        // after cell/formula/style writes (for example while applying a later defined name or
        // conditional-formatting change). Restoring the cloned Model also restores calculation
        // state, spill ownership, parser tables, CF caches and the active view. History and the
        // outbound diff queue are committed only at the very end of the transaction below.
        let snapshot = self.model.clone();
        match self.paste_from_clipboard_transaction(
            source_sheet,
            source_range,
            clipboard,
            is_cut,
            copy_conditional_formatting,
        ) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.model = snapshot;
                Err(error)
            }
        }
    }

    fn paste_from_clipboard_transaction(
        &mut self,
        source_sheet: u32,
        source_range: ClipboardTuple,
        clipboard: &ClipboardData,
        is_cut: bool,
        copy_conditional_formatting: bool,
    ) -> Result<(), String> {
        let mut diff_list = Vec::new();
        let view = self.get_selected_view();
        let (source_first_row, source_first_column, source_last_row, source_last_column) =
            source_range;
        let sheet = view.sheet;
        let [selected_row, selected_column, _, _] = view.range;
        let mut max_row = selected_row;
        let mut max_column = selected_column;
        let area = &Area {
            sheet: source_sheet,
            row: source_first_row,
            column: source_first_column,
            width: source_last_column - source_first_column + 1,
            height: source_last_row - source_first_row + 1,
        };
        let target_area = &Area {
            sheet,
            row: selected_row,
            column: selected_column,
            width: source_last_column - source_first_column + 1,
            height: source_last_row - source_first_row + 1,
        };

        // Every fallible semantic transformation is prepared before the first target cell is
        // cleared. This makes a rejected cut genuinely atomic even before the application-level
        // history wrapper gets a chance to roll back committed diff lists.
        let (
            external_formula_updates,
            defined_name_updates,
            same_sheet_cf_updates,
            cross_sheet_cf_moves,
        ) = if is_cut {
            self.validate_cut_array_area(area, true, "source", None)?;
            self.validate_cut_array_area(target_area, false, "target", Some(area))?;
            let external_formula_updates = self.model.get_external_formula_updates_for_cut(
                area,
                sheet,
                selected_row,
                selected_column,
            )?;
            let defined_name_updates = self.model.get_defined_name_updates_for_cut(
                area,
                sheet,
                selected_row,
                selected_column,
            )?;
            let same_sheet_cf_updates = if source_sheet == sheet {
                self.model.get_conditional_formatting_updates_for_cut(
                    area,
                    selected_row,
                    selected_column,
                )?
            } else {
                Vec::new()
            };
            let cross_sheet_cf_moves = if source_sheet != sheet {
                self.model
                    .get_cross_sheet_conditional_formatting_moves_for_cut(
                        area,
                        sheet,
                        selected_row,
                        selected_column,
                    )?
            } else {
                Vec::new()
            };
            (
                external_formula_updates,
                defined_name_updates,
                same_sheet_cf_updates,
                cross_sheet_cf_moves,
            )
        } else {
            (Vec::new(), Vec::new(), Vec::new(), Vec::new())
        };

        let mut seen_cells = HashSet::new();
        // Compute all changes
        let mut changes = Vec::new();
        for (source_row, data_row) in clipboard {
            let delta_row = source_row - source_first_row;
            let target_row = selected_row + delta_row;
            max_row = max_row.max(target_row);
            for (source_column, value) in data_row {
                let delta_column = source_column - source_first_column;
                let target_column = selected_column + delta_column;
                max_column = max_column.max(target_column);

                if value.is_spill {
                    // Spill cells carry no formula/value, but their style should still be copied.
                    let old_style =
                        self.model
                            .get_cell_style_or_none(sheet, target_row, target_column)?;
                    changes.push((
                        target_row,
                        target_column,
                        None,
                        old_style,
                        None,
                        value.style.clone(),
                    ));
                    seen_cells.insert((target_row, target_column));
                    continue;
                }

                // We are copying the value in
                // (source_row, source_column) to (target_row , target_column)
                // References in formulas are displaced

                // remain in the copied area
                let source = &CellReferenceIndex {
                    sheet: source_sheet,
                    column: *source_column,
                    row: *source_row,
                };
                let target = &CellReferenceIndex {
                    sheet,
                    column: target_column,
                    row: target_row,
                };
                let new_value = if is_cut {
                    self.model
                        .move_cell_value_to_area(&value.text, source, target, area)?
                } else {
                    self.model
                        .extend_copied_value(&value.text, source, target)?
                };

                let old_value = self
                    .model
                    .workbook
                    .worksheet(sheet)?
                    .cell(target_row, target_column)
                    .cloned();

                let old_style =
                    self.model
                        .get_cell_style_or_none(sheet, target_row, target_column)?;
                changes.push((
                    target_row,
                    target_column,
                    old_value.clone(),
                    old_style.clone(),
                    Some(new_value.clone()),
                    value.style.clone(),
                ));
                seen_cells.insert((target_row, target_column));
            }
        }
        // clear the whole area (this resets array formulas)
        self.model.range_clear_contents(target_area)?;
        // set the new values and styles
        for (target_row, target_column, old_value, old_style, new_value, style) in changes {
            if let Some(ref v) = new_value {
                self.model
                    .set_user_input(sheet, target_row, target_column, v.clone())?;
                diff_list.push(Diff::SetCellValue {
                    sheet,
                    row: target_row,
                    column: target_column,
                    new_value: v.clone(),
                    old_value: Box::new(old_value),
                });
            }
            self.model
                .set_cell_style(sheet, target_row, target_column, &style)?;

            diff_list.push(Diff::SetCellStyle {
                sheet,
                row: target_row,
                column: target_column,
                old_value: Box::new(old_style),
                new_value: Box::new(style),
            });
            clipboard_paste_write_failpoint()?;
        }
        if is_cut {
            for row in source_first_row..=source_last_row {
                for column in source_first_column..=source_last_column {
                    if (source_sheet == sheet) && seen_cells.contains(&(row, column)) {
                        continue;
                    }
                    let old_value = self
                        .model
                        .workbook
                        .worksheet(source_sheet)?
                        .cell(row, column)
                        .cloned();

                    diff_list.push(Diff::RangeClearContents {
                        sheet: source_sheet,
                        row,
                        column,
                        width: 1,
                        height: 1,
                        old_value: vec![vec![old_value.clone()]],
                    });

                    // If the source is a dynamic formula anchor, range_clear_contents
                    // would erase its entire spill — including cells that were just
                    // written to by this paste.  Clear the anchor and its spill cells
                    // individually instead, skipping any paste-target cells.
                    let spill_dims = match &old_value {
                        Some(Cell::ArrayFormula {
                            kind: ArrayKind::Dynamic,
                            r,
                            ..
                        }) => Some(*r),
                        _ => None,
                    };
                    if let Some((spill_w, spill_h)) = spill_dims {
                        let ws = self.model.workbook.worksheet_mut(source_sheet)?;
                        for sr in row..row + spill_h {
                            for sc in column..column + spill_w {
                                if (source_sheet == sheet) && seen_cells.contains(&(sr, sc)) {
                                    continue;
                                }
                                let _ = ws.cell_clear_contents(sr, sc);
                            }
                        }
                    } else {
                        let area = Area {
                            sheet: source_sheet,
                            row,
                            column,
                            width: 1,
                            height: 1,
                        };
                        self.model.range_clear_contents(&area)?;
                    }
                    let old_style = self
                        .model
                        .get_cell_style_or_none(source_sheet, row, column)?;
                    let default_style = Style::default();
                    self.model
                        .set_cell_style(source_sheet, row, column, &default_style)?;
                    diff_list.push(Diff::SetCellStyle {
                        sheet: source_sheet,
                        row,
                        column,
                        old_value: Box::new(old_style),
                        new_value: Box::new(default_style),
                    });
                }
            }
            // Update external formulas that reference cells in the moved area.
            for (ext_sheet, ext_row, ext_col, new_formula) in external_formula_updates {
                let old_cell = self
                    .model
                    .workbook
                    .worksheet(ext_sheet)?
                    .cell(ext_row, ext_col)
                    .cloned();
                self.model
                    .set_user_input(ext_sheet, ext_row, ext_col, new_formula.clone())?;
                diff_list.push(Diff::SetCellValue {
                    sheet: ext_sheet,
                    row: ext_row,
                    column: ext_col,
                    new_value: new_formula,
                    old_value: Box::new(old_cell),
                });
            }
            // Update defined names whose references land inside the moved area.
            for (dn_name, dn_scope, old_formula, new_formula) in defined_name_updates {
                diff_list.push(Diff::UpdateDefinedName {
                    name: dn_name.clone(),
                    scope: dn_scope,
                    old_formula: old_formula.clone(),
                    new_name: dn_name.clone(),
                    new_scope: dn_scope,
                    new_formula: new_formula.clone(),
                });
                self.model.update_defined_name(
                    &dn_name,
                    dn_scope,
                    &dn_name,
                    dn_scope,
                    &new_formula,
                )?;
            }
            // Update conditional formatting ranges and formula references.
            // A cross-sheet move cannot update an existing source-sheet CF entry in place: the
            // rule belongs to its worksheet. The application carries native CF/DV sidecars for
            // that case. Keep the established in-sheet move semantics here and, importantly, do
            // not leave a source-sheet rule pointing at target-sheet coordinates.
            for (cf_sheet, cf_idx, new_range, new_rule) in same_sheet_cf_updates {
                let old_cf = self
                    .model
                    .workbook
                    .worksheet(cf_sheet)?
                    .conditional_formatting
                    .get(cf_idx)
                    .ok_or_else(|| format!("CF index {cf_idx} not found"))?
                    .clone();
                {
                    let ws = self.model.workbook.worksheet_mut(cf_sheet)?;
                    ws.conditional_formatting[cf_idx].range = new_range.clone();
                    ws.conditional_formatting[cf_idx].cf_rule = new_rule.clone();
                }
                diff_list.push(Diff::UpdateConditionalFormatting {
                    sheet: cf_sheet,
                    index: cf_idx as u32,
                    old_range: old_cf.range,
                    old_rule: Box::new(old_cf.cf_rule),
                    old_priority: old_cf.priority,
                    new_range,
                    new_rule: Box::new(new_rule),
                });
            }
            if source_sheet != sheet {
                let mut moves = cross_sheet_cf_moves;
                moves.sort_by_key(|(index, _, _)| std::cmp::Reverse(*index));
                let mut additions = Vec::with_capacity(moves.len());
                for (index, new_range, new_rule) in moves {
                    let old = self
                        .model
                        .workbook
                        .worksheet_mut(source_sheet)?
                        .conditional_formatting
                        .get(index)
                        .cloned()
                        .ok_or_else(|| format!("CF index {index} not found"))?;
                    self.model
                        .workbook
                        .worksheet_mut(source_sheet)?
                        .conditional_formatting
                        .remove(index);
                    diff_list.push(Diff::DeleteConditionalFormatting {
                        sheet: source_sheet,
                        index: index as u32,
                        old_range: old.range,
                        old_rule: Box::new(old.cf_rule),
                        old_priority: old.priority,
                    });
                    additions.push((new_range, new_rule));
                }
                additions.reverse();
                let mut priority = self
                    .model
                    .workbook
                    .worksheet(sheet)?
                    .conditional_formatting
                    .iter()
                    .map(|entry| entry.priority)
                    .max()
                    .unwrap_or(0);
                for (new_range, new_rule) in additions {
                    priority += 1;
                    self.model
                        .workbook
                        .worksheet_mut(sheet)?
                        .conditional_formatting
                        .push(ConditionalFormatting {
                            range: new_range.clone(),
                            cf_rule: new_rule.clone(),
                            priority,
                        });
                    diff_list.push(Diff::AddConditionalFormatting {
                        sheet,
                        range: new_range,
                        rule: Box::new(new_rule),
                        priority,
                    });
                }
            }
        } else if copy_conditional_formatting {
            // Copy-paste: duplicate CF rules from the source area to the target.
            let cf_copies = self.model.get_cf_rules_to_copy(
                source_sheet,
                source_first_row,
                source_first_column,
                source_last_row,
                source_last_column,
                selected_row,
                selected_column,
            );
            for (new_range, new_rule) in cf_copies {
                let priority = self
                    .model
                    .workbook
                    .worksheet(sheet)?
                    .conditional_formatting
                    .iter()
                    .map(|cf| cf.priority)
                    .max()
                    .map(|m| m + 1)
                    .unwrap_or(1);
                self.model
                    .workbook
                    .worksheet_mut(sheet)?
                    .conditional_formatting
                    .push(ConditionalFormatting {
                        range: new_range.clone(),
                        cf_rule: new_rule.clone(),
                        priority,
                    });
                diff_list.push(Diff::AddConditionalFormatting {
                    sheet,
                    range: new_range,
                    rule: Box::new(new_rule),
                    priority,
                });
            }
        }
        // select the pasted area
        self.set_selected_range(selected_row, selected_column, max_row, max_column)?;
        self.push_diff_list(diff_list);
        self.evaluate_if_not_paused();
        Ok(())
    }

    /// Paste a csv-string into the model
    pub fn paste_csv_string(&mut self, area: &Area, csv: &str) -> Result<(), String> {
        let sheet = area.sheet;

        // First pass: parse all records so we know the full extent before touching any cells.
        let mut records: Vec<Vec<String>> = Vec::new();
        let mut max_width: i32 = 0;
        let csv_reader = Cursor::new(csv);
        let mut reader = ReaderBuilder::new()
            .delimiter(b'\t')
            .has_headers(false)
            .from_reader(csv_reader);
        for r in reader.records().flatten() {
            let row_data: Vec<String> = r.iter().map(|v| v.to_string()).collect();
            max_width = max_width.max(row_data.len() as i32);
            records.push(row_data);
        }
        if records.is_empty() {
            return Ok(());
        }

        // Check whether any static array formula would be partially overwritten.
        let paste_area = Area {
            sheet,
            row: area.row,
            column: area.column,
            width: max_width,
            height: records.len() as i32,
        };

        // Capture old values BEFORE clearing so undo can restore them correctly.
        let mut old_values: HashMap<(i32, i32), Option<Cell>> = HashMap::new();
        {
            let ws = self.model.workbook.worksheet(sheet)?;
            for r in area.row..area.row + records.len() as i32 {
                for c in area.column..area.column + max_width {
                    old_values.insert((r, c), ws.cell(r, c).cloned());
                }
            }
        }

        self.model.range_clear_contents(&paste_area)?;

        // Second pass: write values and build diff list.
        let mut diff_list = Vec::new();
        let mut row = area.row;
        let mut last_column = area.column;
        for row_data in &records {
            let mut column = area.column;
            for value in row_data {
                let old_value = old_values.remove(&(row, column)).unwrap_or(None);
                self.model
                    .set_user_input(sheet, row, column, value.to_string())?;
                diff_list.push(Diff::SetCellValue {
                    sheet,
                    row,
                    column,
                    new_value: value.to_string(),
                    old_value: Box::new(old_value),
                });
                column += 1;
            }
            last_column = last_column.max(column - 1);
            row += 1;
        }
        self.push_diff_list(diff_list);
        // select the pasted area
        self.set_selected_range(area.row, area.column, row - 1, last_column)?;
        self.evaluate_if_not_paused();
        Ok(())
    }
}
