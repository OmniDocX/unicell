#![allow(clippy::unwrap_used)]

use crate::test::util::new_empty_model;

fn three_sheet_model<'a>() -> crate::model::Model<'a> {
    let mut model = new_empty_model();
    model.rename_sheet("Sheet1", "Summary").unwrap();
    let (name, _) = model.new_sheet();
    model.rename_sheet(&name, "Jan Sales").unwrap();
    let (name, _) = model.new_sheet();
    model.rename_sheet(&name, "Feb Sales").unwrap();
    let (name, _) = model.new_sheet();
    model.rename_sheet(&name, "Mar Sales").unwrap();

    // Jan: 2 numbers, 1 text. Feb: 3 numbers, 1 text.
    // Mar: 3 numbers. Numeric total=27, count=8, non-empty count=10.
    for (sheet, row, column, value) in [
        (1, 1, 1, "1"),
        (1, 1, 2, "2"),
        (1, 2, 1, "jan"),
        (2, 1, 1, "3"),
        (2, 1, 2, "4"),
        (2, 2, 1, "5"),
        (2, 2, 2, "feb"),
        (3, 1, 1, "6"),
        (3, 2, 1, "-2"),
        (3, 2, 2, "8"),
    ] {
        model
            .set_user_input(sheet, row, column, value.to_string())
            .unwrap();
    }
    model
}

#[test]
fn aggregate_functions_evaluate_excel_3d_ranges() {
    let mut model = three_sheet_model();
    let span = "'Jan Sales:Mar Sales'!A1:B2";
    model._set("A1", &format!("=SUM({span})"));
    model._set("A2", &format!("=AVERAGE({span})"));
    model._set("A3", &format!("=COUNT({span})"));
    model._set("A4", &format!("=COUNTA({span})"));
    model._set("A5", &format!("=MIN({span})"));
    model._set("A6", &format!("=MAX({span})"));
    model.evaluate();

    assert_eq!(model._get_text("A1"), "27");
    assert_eq!(model._get_text("A2"), "3.375");
    assert_eq!(model._get_text("A3"), "8");
    assert_eq!(model._get_text("A4"), "10");
    assert_eq!(model._get_text("A5"), "-2");
    assert_eq!(model._get_text("A6"), "8");
    assert_eq!(
        model._get_formula("A1"),
        "=SUM('Jan Sales:Mar Sales'!A1:B2)"
    );
}

#[test]
fn reversed_sheet_endpoints_remain_inclusive() {
    let mut model = three_sheet_model();
    model._set("A1", "=SUM('Mar Sales:Jan Sales'!A1)");
    model.evaluate();

    assert_eq!(model._get_text("A1"), "10");
}

#[test]
fn sheets_and_isref_understand_3d_references() {
    let mut model = three_sheet_model();
    model._set("A1", "=SHEETS('Jan Sales:Mar Sales'!A1)");
    model._set("A2", "=ISREF('Jan Sales:Mar Sales'!A1:B2)");
    model._set("A3", "=FORMULATEXT('Jan Sales:Mar Sales'!A1)");
    model._set("A4", "=ISFORMULA('Jan Sales:Mar Sales'!A1)");
    model.evaluate();

    assert_eq!(model._get_text("A1"), "3");
    assert_eq!(model._get_text("A2"), "TRUE");
    // Excel's documented 3-D aggregate list does not include these scalar
    // inspection functions; they reject a 3-D reference with #VALUE!.
    assert_eq!(model._get_text("A3"), "#VALUE!");
    assert_eq!(model._get_text("A4"), "#VALUE!");
}

#[test]
fn missing_3d_endpoint_is_a_ref_error() {
    let mut model = three_sheet_model();
    model._set("A1", "=SUM('Jan Sales:Missing Sheet'!A1)");
    model.evaluate();

    assert_eq!(model._get_text("A1"), "#REF!");
}
