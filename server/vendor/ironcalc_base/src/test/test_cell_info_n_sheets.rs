#![allow(clippy::unwrap_used)]

use crate::test::util::new_empty_model;

#[test]
fn arguments() {
    let mut model = new_empty_model();
    model._set("A1", "=CELL(\"address\",A1)");
    model._set("A2", "=CELL()");

    model._set("A3", "=INFO(\"recalc\")");
    model._set("A4", "=INFO()");

    model._set("A5", "=N(TRUE)");
    model._set("A6", "=N()");
    model._set("A7", "=N(1, 2)");

    model._set("A8", "=SHEETS()");
    model._set("A9", "=SHEETS(1)");

    model.evaluate();

    assert_eq!(model._get_text("A1"), *"$A$1");
    assert_eq!(model._get_text("A2"), *"#ERROR!");

    assert_eq!(model._get_text("A3"), *"Automatic");
    assert_eq!(model._get_text("A4"), *"#ERROR!");

    assert_eq!(model._get_text("A5"), *"1");
    assert_eq!(model._get_text("A6"), *"#ERROR!");
    assert_eq!(model._get_text("A7"), *"#ERROR!");

    assert_eq!(model._get_text("A8"), *"1");
    assert_eq!(model._get_text("A9"), *"#VALUE!");
}

#[test]
fn sheets_reference_cell_cross_sheet_address_and_n_arrays() {
    let mut model = new_empty_model();
    model.new_sheet();
    model._set("A1", "=SHEETS(Sheet2!B3)");
    model._set("A2", "=CELL(\"address\",Sheet2!B3)");
    model._set("A4", "=N({TRUE,\"text\";#N/A,42})");
    model._set("C1", "TRUE");
    model._set("C2", "'text");
    model._set("D1", "=SHEETS(A1:A2)");
    model._set("D2", "=N(C1:C2)");

    model.evaluate();

    assert_eq!(model._get_text("A1"), "1");
    // CELL(\"address\") returns the absolute A1 address, not a sheet-qualified
    // address, even when its reference is on a different worksheet.
    assert_eq!(model._get_text("A2"), "$B$3");
    assert_eq!(model._get_text("A4"), "1");
    assert_eq!(model._get_text("B4"), "0");
    assert_eq!(model._get_text("A5"), "#N/A");
    assert_eq!(model._get_text("B5"), "42");
    assert_eq!(model._get_text("D1"), "1");
    assert_eq!(model._get_text("D2"), "1");
    assert_eq!(model._get_text("D3"), "0");
}

#[test]
fn info_timezone() {
    let mut model = new_empty_model();
    model._set("A1", "=INFO(\"timezone\")");

    model.evaluate();

    assert_eq!(model._get_text("A1"), *"UTC");

    model.set_timezone("America/Panama").unwrap();

    model.evaluate();

    assert_eq!(model._get_text("A1"), *"America/Panama");
}

#[test]
fn info_timezones() {
    let mut model = new_empty_model();
    model._set("A1", "=INFO(\"timezones\")");
    model._set("B1", "=COUNTA(A:A)");

    model.evaluate();

    assert_ne!(model._get_text("A1"), *"");
    let timezones = model._get_text("B1").parse::<i32>().unwrap();
    assert!(timezones > 400);
}

#[test]
fn info_environment_values_are_available() {
    let mut model = new_empty_model();
    {
        let view = model
            .workbook
            .worksheet_mut(0)
            .unwrap()
            .views
            .get_mut(&0)
            .unwrap();
        view.top_row = 9;
        view.left_column = 4;
    }
    model._set("A1", "=INFO(\"origin\")");
    model._set("A2", "=INFO(\"directory\")");
    model._set("A3", "=INFO(\"osversion\")");
    model._set("A4", "=INFO(\"system\")");
    model.evaluate();

    assert_eq!(model._get_text("A1"), "$A:$D$9");
    assert_ne!(model._get_text("A2"), "#N/IMPL!");
    assert!(!model._get_text("A3").is_empty());
    assert_ne!(model._get_text("A3"), "#N/IMPL!");
    assert!(!model._get_text("A4").is_empty());
}

#[test]
fn cell_filename() {
    // Default workbook name is model
    let mut model = new_empty_model();

    model._set("A1", "=CELL(\"filename\")");

    model.evaluate();

    assert_eq!(model._get_text("A1"), *"[model.xlsx]Sheet1");

    model.workbook.name = "Expenses".to_string();
    model.rename_sheet("Sheet1", "2026").unwrap();

    model.evaluate();

    assert_eq!(model._get_text("A1"), *"[Expenses.xlsx]2026");
}
