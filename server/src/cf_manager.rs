//! Excel-style conditional-formatting rule manager.
//!
//! The executable model intentionally edits only the semantic fields it owns.
//! Imported standard/x14 extension payload remains the responsibility of the
//! OOXML differential merge in `main.rs`; metadata-only edits use IronCalc's
//! non-rebuilding path so the original DXF identity remains stable.

use ironcalc::base::{
    UserModel,
    cf_types::{CfRuleInput, ConditionalFormattingView},
};
use serde_json::{Value, json};

const MAX_ROWS: i64 = 1_048_576;
const MAX_COLS: i64 = 16_384;

#[derive(Debug)]
pub struct CfManagerOutcome {
    pub response: Value,
    pub has_rules: bool,
}

fn string_field<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing str field: {key}"))
}

fn int_field(value: &Value, key: &str) -> Result<i64, String> {
    value
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("missing int field: {key}"))
}

fn index_field(value: &Value) -> Result<u32, String> {
    u32::try_from(int_field(value, "index")?)
        .map_err(|_| "conditional-formatting index must be non-negative".to_string())
}

fn column_name(mut column: i64) -> String {
    let mut result = String::new();
    while column > 0 {
        column -= 1;
        result.insert(0, (b'A' + (column % 26) as u8) as char);
        column /= 26;
    }
    result
}

fn coordinate_range(value: &Value) -> Result<String, String> {
    let mut r0 = int_field(value, "r0")?.clamp(1, MAX_ROWS);
    let mut c0 = int_field(value, "c0")?.clamp(1, MAX_COLS);
    let mut r1 = int_field(value, "r1")?.clamp(1, MAX_ROWS);
    let mut c1 = int_field(value, "c1")?.clamp(1, MAX_COLS);
    if r0 > r1 {
        std::mem::swap(&mut r0, &mut r1);
    }
    if c0 > c1 {
        std::mem::swap(&mut c0, &mut c1);
    }
    Ok(format!(
        "{}{}:{}{}",
        column_name(c0),
        r0,
        column_name(c1),
        r1
    ))
}

fn request_range(value: &Value, fallback: Option<&str>) -> Result<String, String> {
    if let Some(range) = value.get("range").and_then(Value::as_str) {
        let range = range.trim();
        if range.is_empty() {
            return Err("Applies To cannot be empty".to_string());
        }
        if range.len() > 16_384 {
            return Err("Applies To is too long".to_string());
        }
        return Ok(range.to_string());
    }
    if value.get("r0").is_some() {
        return coordinate_range(value);
    }
    fallback
        .map(str::to_string)
        .ok_or_else(|| "missing conditional-formatting range".to_string())
}

fn rule_type(rule: &Value) -> &str {
    rule.get("type")
        .and_then(Value::as_str)
        .unwrap_or("Unknown")
}

fn stop_if_true(rule: &Value) -> Option<bool> {
    rule.get("stop_if_true").and_then(Value::as_bool)
}

fn patch_stop_if_true(rule: &mut Value, stop: bool) -> Result<(), String> {
    let object = rule
        .as_object_mut()
        .ok_or_else(|| "conditional-formatting rule must be an object".to_string())?;
    if !object.contains_key("stop_if_true") {
        return Err(
            "stopIfTrue is not executable for this visual conditional-format rule".to_string(),
        );
    }
    object.insert("stop_if_true".to_string(), Value::Bool(stop));
    Ok(())
}

fn rule_summary(rule: &Value) -> String {
    let text = |key: &str| rule.get(key).and_then(Value::as_str).unwrap_or("");
    match rule_type(rule) {
        "CellIs" => {
            let operator = text("operator");
            let formula = text("formula");
            let formula2 = text("formula2");
            if formula2.is_empty() {
                format!("CellIs · {operator} · {formula}")
            } else {
                format!("CellIs · {operator} · {formula}, {formula2}")
            }
        }
        "Text" => format!("Text · {} · {}", text("operator"), text("value")),
        "Formula" => format!("Formula · {}", text("formula")),
        "TimePeriod" => format!("TimePeriod · {}", text("time_period")),
        "Top10" | "Bottom10" => format!(
            "{} · {}{}",
            rule_type(rule),
            rule.get("rank").and_then(Value::as_u64).unwrap_or(0),
            if rule
                .get("percent")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                "%"
            } else {
                ""
            }
        ),
        other => other.to_string(),
    }
}

fn editable_rule_json(
    model: &UserModel<'_>,
    sheet: u32,
    view: &ConditionalFormattingView,
) -> Result<Value, String> {
    let mut rule = serde_json::to_value(&view.cf_rule).map_err(|error| error.to_string())?;
    if let Some(object) = rule.as_object_mut() {
        if object.remove("dxf_id").is_some() {
            let format = model
                .get_dxf_for_conditional_formatting(sheet, view.index as u32)?
                .map(serde_json::to_value)
                .transpose()
                .map_err(|error| error.to_string())?
                .unwrap_or(Value::Null);
            object.insert("format".to_string(), format);
        }
    }
    Ok(rule)
}

fn list_value(model: &UserModel<'_>, sheet: u32) -> Result<Value, String> {
    let views = model.get_conditional_formatting_list(sheet)?;
    let mut rules = Vec::with_capacity(views.len());
    for (position, view) in views.iter().enumerate() {
        let raw_rule = serde_json::to_value(&view.cf_rule).map_err(|error| error.to_string())?;
        let editable_rule = editable_rule_json(model, sheet, view)?;
        let can_stop = stop_if_true(&editable_rule).is_some();
        rules.push(json!({
            "index": view.index,
            "position": position,
            "priority": view.priority,
            "range": view.range,
            "cf_rule": raw_rule,
            "rule": editable_rule,
            "ruleType": rule_type(&editable_rule),
            "summary": rule_summary(&editable_rule),
            "stopIfTrue": stop_if_true(&editable_rule),
            "canStopIfTrue": can_stop,
            "transportPatchSafe": true,
        }));
    }
    Ok(json!({
        "rules": rules,
        "capabilities": {
            "updateAppliesTo": true,
            "reorderPriority": true,
            "stopIfTrue": true,
            "duplicate": true,
            "delete": true,
            "standardRuntime": true,
            "x14Runtime": false,
            "unknownExtensions": "preserve-only"
        },
        "notice": "Standard rules execute locally. Unknown standard/x14 extensions are preserved for Excel round-trip and are not claimed as locally executable."
    }))
}

fn current_view(
    model: &UserModel<'_>,
    sheet: u32,
    index: u32,
) -> Result<ConditionalFormattingView, String> {
    model
        .get_conditional_formatting_list(sheet)?
        .into_iter()
        .find(|view| view.index == index as usize)
        .ok_or_else(|| format!("Conditional formatting index {index} out of bounds"))
}

fn move_rule(
    model: &mut UserModel<'_>,
    sheet: u32,
    index: u32,
    target_position: usize,
) -> Result<bool, String> {
    let list = model.get_conditional_formatting_list(sheet)?;
    if target_position >= list.len() {
        return Err(format!(
            "conditional-formatting target position {target_position} out of bounds"
        ));
    }
    let mut position = list
        .iter()
        .position(|view| view.index == index as usize)
        .ok_or_else(|| format!("Conditional formatting index {index} out of bounds"))?;
    let changed = position != target_position;
    while position > target_position {
        model.raise_conditional_formatting_priority(sheet, index)?;
        position -= 1;
    }
    while position < target_position {
        model.lower_conditional_formatting_priority(sheet, index)?;
        position += 1;
    }
    Ok(changed)
}

fn optional_stop(value: &Value) -> Result<Option<bool>, String> {
    for key in ["stopIfTrue", "stop_if_true"] {
        if let Some(raw) = value.get(key) {
            return raw
                .as_bool()
                .map(Some)
                .ok_or_else(|| format!("{key} must be a boolean"));
        }
    }
    Ok(None)
}

fn mutation_response(
    model: &UserModel<'_>,
    sheet: u32,
    operation: &str,
    changed: bool,
) -> Result<Value, String> {
    let mut response = list_value(model, sheet)?;
    let object = response
        .as_object_mut()
        .expect("conditional-formatting list response is an object");
    object.insert("operation".to_string(), json!(operation));
    object.insert("changed".to_string(), json!(changed));
    Ok(response)
}

pub fn execute(
    model: &mut UserModel<'_>,
    sheet: u32,
    request: &Value,
) -> Result<CfManagerOutcome, String> {
    let operation = string_field(request, "op")?;
    let changed = match operation {
        "list" => false,
        "add" => {
            let range = request_range(request, None)?;
            let rule: CfRuleInput = serde_json::from_value(
                request
                    .get("rule")
                    .cloned()
                    .ok_or_else(|| "missing conditional-formatting rule".to_string())?,
            )
            .map_err(|error| format!("bad cf rule: {error}"))?;
            model.add_conditional_formatting(sheet, &range, rule)?;
            true
        }
        "update" => {
            let index = index_field(request)?;
            let current = current_view(model, sheet, index)?;
            let range = request_range(request, Some(&current.range))?;
            let requested_stop = optional_stop(request)?;
            if let Some(mut replacement) = request.get("rule").cloned() {
                if let Some(stop) = requested_stop {
                    patch_stop_if_true(&mut replacement, stop)?;
                }
                let replacement: CfRuleInput = serde_json::from_value(replacement)
                    .map_err(|error| format!("bad cf rule: {error}"))?;
                model.update_conditional_formatting(sheet, index, &range, replacement)?;
                true
            } else {
                let current_rule =
                    serde_json::to_value(&current.cf_rule).map_err(|error| error.to_string())?;
                let is_changed = range != current.range
                    || requested_stop
                        .is_some_and(|value| stop_if_true(&current_rule) != Some(value));
                if is_changed {
                    model.update_conditional_formatting_metadata(
                        sheet,
                        index,
                        &range,
                        requested_stop,
                    )?;
                }
                is_changed
            }
        }
        "raise" => {
            let index = index_field(request)?;
            let before = model.get_conditional_formatting_list(sheet)?;
            let position = before
                .iter()
                .position(|view| view.index == index as usize)
                .ok_or_else(|| format!("Conditional formatting index {index} out of bounds"))?;
            if position > 0 {
                model.raise_conditional_formatting_priority(sheet, index)?;
                true
            } else {
                false
            }
        }
        "lower" => {
            let index = index_field(request)?;
            let before = model.get_conditional_formatting_list(sheet)?;
            let position = before
                .iter()
                .position(|view| view.index == index as usize)
                .ok_or_else(|| format!("Conditional formatting index {index} out of bounds"))?;
            if position + 1 < before.len() {
                model.lower_conditional_formatting_priority(sheet, index)?;
                true
            } else {
                false
            }
        }
        "move" => {
            let index = index_field(request)?;
            let position = usize::try_from(int_field(request, "position")?)
                .map_err(|_| "conditional-formatting position must be non-negative".to_string())?;
            move_rule(model, sheet, index, position)?
        }
        "duplicate" | "copy" => {
            let index = index_field(request)?;
            let before = model.get_conditional_formatting_list(sheet)?;
            let source_position = before
                .iter()
                .position(|view| view.index == index as usize)
                .ok_or_else(|| format!("Conditional formatting index {index} out of bounds"))?;
            let source = &before[source_position];
            let range = request_range(request, Some(&source.range))?;
            let new_index = before.len() as u32;
            model.duplicate_conditional_formatting(sheet, index, &range)?;
            let target_position = (source_position + 1).min(before.len());
            move_rule(model, sheet, new_index, target_position)?;
            true
        }
        "delete" => {
            model.delete_conditional_formatting(sheet, index_field(request)?)?;
            true
        }
        "clear" => {
            let mut indices = model
                .get_conditional_formatting_list(sheet)?
                .into_iter()
                .map(|view| view.index as u32)
                .collect::<Vec<_>>();
            indices.sort_unstable_by(|left, right| right.cmp(left));
            let changed = !indices.is_empty();
            for index in indices {
                model.delete_conditional_formatting(sheet, index)?;
            }
            changed
        }
        other => return Err(format!("bad cf op: {other}")),
    };

    let response = if operation == "list" {
        list_value(model, sheet)?
    } else {
        mutation_response(model, sheet, operation, changed)?
    };
    let has_rules = response
        .get("rules")
        .and_then(Value::as_array)
        .is_some_and(|rules| !rules.is_empty());
    Ok(CfManagerOutcome {
        response,
        has_rules,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ironcalc::base::{
        cf_types::{CfRule, Cfvo},
        types::Dxf,
    };

    fn model() -> UserModel<'static> {
        UserModel::new_empty("cf-manager", "en", "UTC", "en").unwrap()
    }

    fn formula_rule(stop_if_true: bool) -> CfRuleInput {
        CfRuleInput::Formula {
            formula: "=A1>0".to_string(),
            format: Dxf::default(),
            stop_if_true,
        }
    }

    #[test]
    fn metadata_update_preserves_dxf_and_supports_undo_redo() {
        let mut model = model();
        model
            .add_conditional_formatting(0, "A1:A3", formula_rule(false))
            .unwrap();
        let before = model.get_conditional_formatting_list(0).unwrap();
        let (index, dxf_id) = match &before[0].cf_rule {
            CfRule::Formula { dxf_id, .. } => (before[0].index as u32, *dxf_id),
            other => panic!("unexpected rule: {other:?}"),
        };

        execute(
            &mut model,
            0,
            &json!({
                "op": "update",
                "index": index,
                "range": "B2:B8 D2:D8",
                "stopIfTrue": true
            }),
        )
        .unwrap();
        let changed = model.get_conditional_formatting_list(0).unwrap();
        assert_eq!(changed[0].range, "B2:B8 D2:D8");
        assert!(matches!(
            changed[0].cf_rule,
            CfRule::Formula {
                dxf_id: current,
                stop_if_true: true,
                ..
            } if current == dxf_id
        ));

        model.undo().unwrap();
        let undone = model.get_conditional_formatting_list(0).unwrap();
        assert_eq!(undone[0].range, "A1:A3");
        assert!(matches!(
            undone[0].cf_rule,
            CfRule::Formula {
                dxf_id: current,
                stop_if_true: false,
                ..
            } if current == dxf_id
        ));
        model.redo().unwrap();
        assert_eq!(
            model.get_conditional_formatting_list(0).unwrap()[0].range,
            "B2:B8 D2:D8"
        );
    }

    #[test]
    fn duplicate_is_adjacent_and_reuses_dxf() {
        let mut model = model();
        model
            .add_conditional_formatting(0, "A1", formula_rule(false))
            .unwrap();
        model
            .add_conditional_formatting(0, "B1", formula_rule(true))
            .unwrap();
        let before = model.get_conditional_formatting_list(0).unwrap();
        let source = &before[1];
        let source_dxf = match source.cf_rule {
            CfRule::Formula { dxf_id, .. } => dxf_id,
            _ => unreachable!(),
        };
        execute(
            &mut model,
            0,
            &json!({"op":"duplicate", "index":source.index, "range":"C1:C4"}),
        )
        .unwrap();
        let after = model.get_conditional_formatting_list(0).unwrap();
        assert_eq!(after.len(), 3);
        assert_eq!(after[2].range, "C1:C4");
        assert!(matches!(
            after[2].cf_rule,
            CfRule::Formula { dxf_id, .. } if dxf_id == source_dxf
        ));
    }

    #[test]
    fn move_clear_and_manager_capabilities_are_reported() {
        let mut model = model();
        for (range, formula) in [("A1", "=1"), ("B1", "=2"), ("C1", "=3")] {
            model
                .add_conditional_formatting(
                    0,
                    range,
                    CfRuleInput::Formula {
                        formula: formula.to_string(),
                        format: Dxf::default(),
                        stop_if_true: false,
                    },
                )
                .unwrap();
        }
        let before = model.get_conditional_formatting_list(0).unwrap();
        let moved_index = before[2].index;
        let moved = execute(
            &mut model,
            0,
            &json!({"op":"move", "index":moved_index, "position":0}),
        )
        .unwrap();
        assert_eq!(moved.response["rules"][0]["index"], moved_index);
        assert_eq!(moved.response["capabilities"]["x14Runtime"], false);
        assert_eq!(
            moved.response["capabilities"]["unknownExtensions"],
            "preserve-only"
        );

        let cleared = execute(&mut model, 0, &json!({"op":"clear"})).unwrap();
        assert!(!cleared.has_rules);
        assert_eq!(cleared.response["rules"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn visual_stop_if_true_rejection_is_atomic() {
        let mut model = model();
        model
            .add_conditional_formatting(
                0,
                "A1:A4",
                CfRuleInput::DataBar {
                    min: Some(Cfvo::Min),
                    max: Some(Cfvo::Max),
                    positive_color: ironcalc::base::types::Color::Rgb("#00AA00".to_string()),
                    negative_color: ironcalc::base::types::Color::Rgb("#AA0000".to_string()),
                    is_gradient: true,
                    show_value: true,
                },
            )
            .unwrap();
        let index = model.get_conditional_formatting_list(0).unwrap()[0].index;
        let depth = model.undo_depth();
        let error = execute(
            &mut model,
            0,
            &json!({
                "op":"update", "index":index, "range":"Z1:Z9", "stopIfTrue":true
            }),
        )
        .unwrap_err();
        assert!(error.contains("stopIfTrue"));
        assert_eq!(model.undo_depth(), depth);
        assert_eq!(
            model.get_conditional_formatting_list(0).unwrap()[0].range,
            "A1:A4"
        );
    }
}
