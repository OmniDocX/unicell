#![allow(clippy::unwrap_used)]

use crate::{
    cf_types::{
        CfRule, CfRuleInput, Cfvo, ColorScaleThreshold, ConditionalFormatting, Icon, IconThreshold,
    },
    test::{user_model::util::new_empty_user_model, util::new_empty_model},
    types::{Color, DefinedName},
    user_model::fail_clipboard_paste_after_writes,
    UserModel,
};

fn copy_cell(model: &mut UserModel<'_>, row: i32, column: i32) -> crate::user_model::ClipboardData {
    model.set_selected_cell(row, column).unwrap();
    model.set_selected_range(row, column, row, column).unwrap();
    model.copy_to_clipboard().unwrap().data
}

#[test]
fn injected_write_failure_restores_exact_model_and_does_not_commit_history() {
    let mut model = new_empty_user_model();
    model.set_user_input(0, 1, 1, "source").unwrap();
    model.set_user_input(0, 4, 4, "destination").unwrap();
    let clipboard = copy_cell(&mut model, 1, 1);
    model.set_selected_cell(4, 4).unwrap();

    // Isolate this operation from setup diffs, then capture the exact persisted workbook/view.
    let empty_queue = {
        model.flush_send_queue();
        model.flush_send_queue()
    };
    let before = model.to_bytes();
    let undo_count = model.undo_depth();

    fail_clipboard_paste_after_writes(1);
    let error = model
        .paste_from_clipboard(0, (1, 1, 1, 1), &clipboard, false)
        .unwrap_err();
    assert!(error.contains("Injected clipboard paste failure"));
    assert_eq!(model.to_bytes(), before);
    assert_eq!(model.undo_depth(), undo_count);
    assert_eq!(model.flush_send_queue(), empty_queue);
    assert_eq!(model.get_cell_content(0, 1, 1).unwrap(), "source");
    assert_eq!(model.get_cell_content(0, 4, 4).unwrap(), "destination");
}

#[test]
fn same_sheet_partial_conditional_formatting_cut_is_atomic() {
    let mut model = new_empty_user_model();
    model.set_user_input(0, 2, 1, "source").unwrap();
    model
        .add_conditional_formatting(
            0,
            "A1:A3",
            CfRuleInput::ColorScale {
                thresholds: vec![
                    ColorScaleThreshold {
                        cfvo: Cfvo::Min,
                        color: Color::Rgb("#FF0000".to_string()),
                    },
                    ColorScaleThreshold {
                        cfvo: Cfvo::Max,
                        color: Color::Rgb("#00FF00".to_string()),
                    },
                ],
            },
        )
        .unwrap();
    let clipboard = copy_cell(&mut model, 2, 1);
    model.set_selected_cell(2, 2).unwrap();
    let before = model.to_bytes();
    let history = model.undo_depth();

    let error = model
        .paste_from_clipboard(0, (2, 1, 2, 1), &clipboard, true)
        .unwrap_err();
    assert!(error.contains("part of conditional-formatting range"));
    assert_eq!(model.to_bytes(), before);
    assert_eq!(model.undo_depth(), history);
    assert_eq!(model.get_cell_content(0, 2, 1).unwrap(), "source");
    assert_eq!(model.get_cell_content(0, 2, 2).unwrap(), "");
}

#[test]
fn cut_moves_formula_cfvo_in_every_formula_bearing_rule_family() {
    let mut model = new_empty_user_model();
    model.set_user_input(0, 1, 1, "10").unwrap();
    let red = Color::Rgb("#FF0000".to_string());
    let blue = Color::Rgb("#0000FF".to_string());
    let formula = || Cfvo::Formula("=$A$1".to_string());

    model
        .add_conditional_formatting(
            0,
            "A1",
            CfRuleInput::ColorScale {
                thresholds: vec![
                    ColorScaleThreshold {
                        cfvo: formula(),
                        color: red.clone(),
                    },
                    ColorScaleThreshold {
                        cfvo: Cfvo::Max,
                        color: blue.clone(),
                    },
                ],
            },
        )
        .unwrap();
    model
        .add_conditional_formatting(
            0,
            "A1",
            CfRuleInput::DataBar {
                min: Some(formula()),
                max: Some(Cfvo::Max),
                positive_color: blue.clone(),
                negative_color: red.clone(),
                is_gradient: false,
                show_value: true,
            },
        )
        .unwrap();
    model
        .add_conditional_formatting(
            0,
            "A1",
            CfRuleInput::IconSet {
                thresholds: vec![IconThreshold {
                    icon: Icon::ArrowUp,
                    cfvo: formula(),
                    color: blue.clone(),
                    is_strict: false,
                }],
                show_value: true,
            },
        )
        .unwrap();
    model
        .add_conditional_formatting(
            0,
            "A1",
            CfRuleInput::IconRating {
                icon: Icon::Star,
                color: blue,
                thresholds: vec![(formula(), false)],
                show_value: true,
            },
        )
        .unwrap();

    let clipboard = copy_cell(&mut model, 1, 1);
    model.set_selected_cell(2, 2).unwrap();
    model
        .paste_from_clipboard(0, (1, 1, 1, 1), &clipboard, true)
        .unwrap();

    let rules = model.get_conditional_formatting_list(0).unwrap();
    assert_eq!(rules.len(), 4);
    assert!(rules.iter().all(|entry| entry.range == "B2"));
    let mut formulas = Vec::new();
    for entry in rules {
        match entry.cf_rule {
            CfRule::ColorScale { thresholds } => {
                for threshold in thresholds {
                    if let Cfvo::Formula(formula) = threshold.cfvo {
                        formulas.push(formula);
                    }
                }
            }
            CfRule::DataBar { min, max, .. } => {
                for value in [min, max].into_iter().flatten() {
                    if let Cfvo::Formula(formula) = value {
                        formulas.push(formula);
                    }
                }
            }
            CfRule::IconSet { thresholds, .. } => {
                for threshold in thresholds {
                    if let Cfvo::Formula(formula) = threshold.cfvo {
                        formulas.push(formula);
                    }
                }
            }
            CfRule::IconRating { thresholds, .. } => {
                for (value, _) in thresholds {
                    if let Cfvo::Formula(formula) = value {
                        formulas.push(formula);
                    }
                }
            }
            _ => {}
        }
    }
    assert_eq!(formulas.len(), 4);
    assert!(formulas.iter().all(|formula| {
        let normalized = formula.replace('$', "");
        normalized.contains("B2") && !normalized.contains("A1")
    }));
}

#[test]
fn unsupported_cfvo_formula_rejects_before_any_write() {
    let mut base = new_empty_model();
    base.workbook.worksheets[0]
        .conditional_formatting
        .push(ConditionalFormatting {
            range: "A1".to_string(),
            cf_rule: CfRule::ColorScale {
                thresholds: vec![ColorScaleThreshold {
                    cfvo: Cfvo::Formula("=BROKEN(".to_string()),
                    color: Color::Rgb("#FF0000".to_string()),
                }],
            },
            priority: 1,
        });
    let mut model = UserModel::from_model(base);
    model.set_user_input(0, 1, 1, "source").unwrap();
    let clipboard = copy_cell(&mut model, 1, 1);
    model.set_selected_cell(2, 2).unwrap();
    let before = model.to_bytes();
    let history = model.undo_depth();

    assert!(model
        .paste_from_clipboard(0, (1, 1, 1, 1), &clipboard, true)
        .is_err());
    assert_eq!(model.to_bytes(), before);
    assert_eq!(model.undo_depth(), history);
}

#[test]
fn partial_defined_name_range_rejects_without_clearing_source() {
    let mut model = new_empty_user_model();
    model.set_user_input(0, 2, 1, "source").unwrap();
    model
        .new_defined_name("WholeRange", None, "Sheet1!$A$1:$A$3")
        .unwrap();
    let clipboard = copy_cell(&mut model, 2, 1);
    model.set_selected_cell(2, 2).unwrap();
    let before = model.to_bytes();

    let error = model
        .paste_from_clipboard(0, (2, 1, 2, 1), &clipboard, true)
        .unwrap_err();
    assert!(error.contains("defined name 'WholeRange'"));
    assert_eq!(model.to_bytes(), before);
}

#[test]
fn complex_defined_name_ast_is_retargeted() {
    let mut model = new_empty_user_model();
    model.set_user_input(0, 1, 1, "7").unwrap();
    model
        .new_defined_name("ComplexName", None, "=LAMBDA(x,SUM(Sheet1!$A$1,x))")
        .unwrap();
    let clipboard = copy_cell(&mut model, 1, 1);
    model.set_selected_cell(2, 2).unwrap();
    model
        .paste_from_clipboard(0, (1, 1, 1, 1), &clipboard, true)
        .unwrap();

    let formula = model
        .get_defined_name_list()
        .into_iter()
        .find(|(name, _, _)| name == "ComplexName")
        .unwrap()
        .2;
    let normalized = formula.replace('$', "");
    assert!(normalized.contains("B2"), "{formula}");
    assert!(!normalized.contains("A1"), "{formula}");
}

#[test]
fn unsupported_defined_name_formula_rejects_atomically() {
    let mut base = new_empty_model();
    base.workbook.defined_names.push(DefinedName {
        name: "ImportedBroken".to_string(),
        formula: "=SUM(Sheet1!$A$1,".to_string(),
        sheet_id: None,
    });
    let mut model = UserModel::from_model(base);
    model.set_user_input(0, 1, 1, "source").unwrap();
    let clipboard = copy_cell(&mut model, 1, 1);
    model.set_selected_cell(2, 2).unwrap();
    let before = model.to_bytes();

    assert!(model
        .paste_from_clipboard(0, (1, 1, 1, 1), &clipboard, true)
        .is_err());
    assert_eq!(model.to_bytes(), before);
}

#[test]
fn partial_dynamic_source_spill_cut_is_rejected_but_full_spill_moves() {
    let mut model = new_empty_user_model();
    model.set_user_input(0, 1, 1, "=SEQUENCE(3)").unwrap();

    let partial = copy_cell(&mut model, 2, 1);
    model.set_selected_cell(1, 3).unwrap();
    let before = model.to_bytes();
    assert!(model
        .paste_from_clipboard(0, (2, 1, 2, 1), &partial, true)
        .is_err());
    assert_eq!(model.to_bytes(), before);

    model.set_selected_cell(1, 1).unwrap();
    model.set_selected_range(1, 1, 3, 1).unwrap();
    let full = model.copy_to_clipboard().unwrap();
    model.set_selected_cell(1, 3).unwrap();
    model
        .paste_from_clipboard(0, full.range, &full.data, true)
        .unwrap();
    assert_eq!(model.get_cell_content(0, 1, 1).unwrap(), "");
    assert_eq!(model.get_formatted_cell_value(0, 1, 3).unwrap(), "1");
    assert_eq!(model.get_formatted_cell_value(0, 2, 3).unwrap(), "2");
    assert_eq!(model.get_formatted_cell_value(0, 3, 3).unwrap(), "3");
}

#[test]
fn partial_dynamic_target_and_legacy_array_source_are_rejected_atomically() {
    let mut model = new_empty_user_model();
    model.set_user_input(0, 1, 4, "source").unwrap();
    model.set_user_input(0, 1, 1, "=SEQUENCE(3)").unwrap();
    let clipboard = copy_cell(&mut model, 1, 4);
    model.set_selected_cell(2, 1).unwrap();
    let before = model.to_bytes();
    assert!(model
        .paste_from_clipboard(0, (1, 4, 1, 4), &clipboard, true)
        .is_err());
    assert_eq!(model.to_bytes(), before);

    let mut legacy = new_empty_user_model();
    legacy.set_user_array_formula(0, 1, 1, 2, 1, "=42").unwrap();
    let clipboard = copy_cell(&mut legacy, 1, 1);
    legacy.set_selected_cell(3, 1).unwrap();
    let before = legacy.to_bytes();
    assert!(legacy
        .paste_from_clipboard(0, (1, 1, 1, 1), &clipboard, true)
        .is_err());
    assert_eq!(legacy.to_bytes(), before);
}
