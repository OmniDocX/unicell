#![allow(clippy::unwrap_used)]

use crate::{functions::Function, language::get_default_language, test::util::new_empty_model};

#[test]
fn groupby_and_pivotby_are_registered_as_modern_excel_functions() {
    let functions = &get_default_language().functions;
    assert_eq!(functions.lookup("groupby"), Some(Function::Groupby));
    assert_eq!(functions.lookup("PIVOTBY"), Some(Function::Pivotby));
    assert_eq!(Function::Groupby.to_xlsx_string(), "_xlfn.GROUPBY");
    assert_eq!(Function::Pivotby.to_xlsx_string(), "_xlfn.PIVOTBY");
    assert!(Function::into_iter().any(|function| function == Function::Groupby));
    assert!(Function::into_iter().any(|function| function == Function::Pivotby));
}

#[test]
fn groupby_sum_preserves_headers_and_adds_grand_total() {
    let mut model = new_empty_model();
    for (cell, value) in [
        ("A1", "Region"),
        ("B1", "Sales"),
        ("A2", "East"),
        ("B2", "10"),
        ("A3", "West"),
        ("B3", "20"),
        ("A4", "East"),
        ("B4", "5"),
        ("A5", "West"),
        ("B5", "30"),
    ] {
        model._set(cell, value);
    }
    model._set("D1", "=GROUPBY(A1:A5,B1:B5,SUM,3,1)");
    model.evaluate();

    assert_eq!(model._get_text("D1"), "Region");
    assert_eq!(model._get_text("E1"), "Sales");
    assert_eq!(model._get_text("D2"), "East");
    assert_eq!(model._get_text("E2"), "15");
    assert_eq!(model._get_text("D3"), "West");
    assert_eq!(model._get_text("E3"), "50");
    assert_eq!(model._get_text("D4"), "Grand Total");
    assert_eq!(model._get_text("E4"), "65");
    assert_eq!(model._get_text("D5"), "");
}

#[test]
fn groupby_common_aggregators_match_excel_range_semantics() {
    let mut model = new_empty_model();
    for (cell, value) in [
        ("A1", "A"),
        ("B1", "2"),
        ("A2", "A"),
        ("B2", "4"),
        ("A3", "B"),
        ("B3", "10"),
        ("A4", "B"),
        ("B4", "text"),
    ] {
        model._set(cell, value);
    }
    model._set("D1", "=GROUPBY(A1:A4,B1:B4,AVERAGE,0,0)");
    model._set("G1", "=GROUPBY(A1:A4,B1:B4,COUNT,0,0)");
    model._set("J1", "=GROUPBY(A1:A4,B1:B4,MAX,0,0)");
    model._set("M1", "=GROUPBY(A1:A4,B1:B4,MIN,0,0)");
    model.evaluate();

    assert_eq!(model._get_text("E1"), "3");
    assert_eq!(model._get_text("E2"), "10");
    assert_eq!(model._get_text("H1"), "2");
    assert_eq!(model._get_text("H2"), "1");
    assert_eq!(model._get_text("K1"), "4");
    assert_eq!(model._get_text("K2"), "10");
    assert_eq!(model._get_text("N1"), "2");
    assert_eq!(model._get_text("N2"), "10");
}

#[test]
fn groupby_multiple_fields_emits_subtotals_and_grand_total() {
    let mut model = new_empty_model();
    for (cell, value) in [
        ("A1", "Region"),
        ("B1", "Product"),
        ("C1", "Sales"),
        ("A2", "East"),
        ("B2", "A"),
        ("C2", "1"),
        ("A3", "East"),
        ("B3", "B"),
        ("C3", "2"),
        ("A4", "West"),
        ("B4", "A"),
        ("C4", "3"),
        ("A5", "West"),
        ("B5", "B"),
        ("C5", "4"),
    ] {
        model._set(cell, value);
    }
    model._set("E1", "=GROUPBY(A1:B5,C1:C5,SUM,3,2)");
    model.evaluate();

    let expected = [
        ("E1", "Region"),
        ("F1", "Product"),
        ("G1", "Sales"),
        ("E2", "East"),
        ("F2", "A"),
        ("G2", "1"),
        ("E3", "East"),
        ("F3", "B"),
        ("G3", "2"),
        ("E4", "East Total"),
        ("G4", "3"),
        ("E5", "West"),
        ("F5", "A"),
        ("G5", "3"),
        ("E6", "West"),
        ("F6", "B"),
        ("G6", "4"),
        ("E7", "West Total"),
        ("G7", "7"),
        ("E8", "Grand Total"),
        ("G8", "10"),
    ];
    for (cell, value) in expected {
        assert_eq!(model._get_text(cell), value, "unexpected value in {cell}");
    }
}

#[test]
fn groupby_accepts_explicit_lambda_and_filter() {
    let mut model = new_empty_model();
    for (cell, value) in [
        ("A1", "A"),
        ("B1", "1"),
        ("C1", "1"),
        ("A2", "A"),
        ("B2", "3"),
        ("C2", "0"),
        ("A3", "B"),
        ("B3", "8"),
        ("C3", "1"),
    ] {
        model._set(cell, value);
    }
    model._set(
        "E1",
        "=GROUPBY(A1:A3,B1:B3,LAMBDA(items,SUM(items)),0,0,,C1:C3)",
    );
    model.evaluate();

    assert_eq!(model._get_text("E1"), "A");
    assert_eq!(model._get_text("F1"), "1");
    assert_eq!(model._get_text("E2"), "B");
    assert_eq!(model._get_text("F2"), "8");
}

#[test]
fn groupby_sorts_by_aggregate_and_count_ignores_errors() {
    let mut model = new_empty_model();
    for (cell, value) in [
        ("A1", "A"),
        ("B1", "1"),
        ("A2", "A"),
        ("B2", "3"),
        ("A3", "B"),
        ("B3", "8"),
        ("A4", "B"),
    ] {
        model._set(cell, value);
    }
    model._set("B4", "=#N/A");
    model._set("D1", "=GROUPBY(A1:A4,B1:B4,SUM,0,0,-2)");
    model._set("G1", "=GROUPBY(A1:A4,B1:B4,COUNT,0,0)");
    model.evaluate();

    // SUM propagates the error for B, so it sorts after ordinary numbers.
    assert_eq!(model._get_text("D1"), "B");
    assert_eq!(model._get_text("E1"), "#N/A");
    assert_eq!(model._get_text("D2"), "A");
    assert_eq!(model._get_text("E2"), "4");
    // COUNT ignores errors in a referenced vector.
    assert_eq!(model._get_text("G1"), "A");
    assert_eq!(model._get_text("H1"), "2");
    assert_eq!(model._get_text("G2"), "B");
    assert_eq!(model._get_text("H2"), "1");
}

#[test]
fn pivotby_builds_two_axes_and_both_grand_totals() {
    let mut model = new_empty_model();
    for (cell, value) in [
        ("A1", "Region"),
        ("B1", "Quarter"),
        ("C1", "Sales"),
        ("A2", "East"),
        ("B2", "Q1"),
        ("C2", "10"),
        ("A3", "East"),
        ("B3", "Q2"),
        ("C3", "5"),
        ("A4", "West"),
        ("B4", "Q1"),
        ("C4", "20"),
        ("A5", "West"),
        ("B5", "Q2"),
        ("C5", "7"),
        ("A6", "East"),
        ("B6", "Q1"),
        ("C6", "2"),
    ] {
        model._set(cell, value);
    }
    model._set("E1", "=PIVOTBY(A1:A6,B1:B6,C1:C6,SUM,3,1,,1)");
    model.evaluate();

    let expected = [
        ("E1", "Region"),
        ("F1", "Q1"),
        ("G1", "Q2"),
        ("H1", "Grand Total"),
        ("E2", "East"),
        ("F2", "12"),
        ("G2", "5"),
        ("H2", "17"),
        ("E3", "West"),
        ("F3", "20"),
        ("G3", "7"),
        ("H3", "27"),
        ("E4", "Grand Total"),
        ("F4", "32"),
        ("G4", "12"),
        ("H4", "44"),
    ];
    for (cell, value) in expected {
        assert_eq!(model._get_text(cell), value, "unexpected value in {cell}");
    }
}

#[test]
fn groupby_xlfn_import_spills_and_blocked_spill_reports_error() {
    let mut model = new_empty_model();
    model._set("A1", "A");
    model._set("B1", "1");
    model._set("A2", "B");
    model._set("B2", "2");
    model._set("E2", "blocked");
    model._set("D1", "=_xlfn.GROUPBY(A1:A2,B1:B2,SUM,0,0)");
    model.evaluate();

    assert_eq!(model._get_text("D1"), "#SPILL!");
    assert_eq!(model._get_text("E2"), "blocked");
    assert_eq!(model._get_formula("D1"), "=GROUPBY(A1:A2,B1:B2,SUM,0,0)");
}
