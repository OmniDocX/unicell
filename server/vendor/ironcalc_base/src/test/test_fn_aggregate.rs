#![allow(clippy::unwrap_used)]

use crate::{functions::Function, language::get_default_language, test::util::new_empty_model};

#[test]
fn aggregate_is_registered_as_a_legacy_excel_function() {
    let functions = &get_default_language().functions;
    assert_eq!(functions.lookup("aggregate"), Some(Function::Aggregate));
    assert_eq!(Function::Aggregate.to_xlsx_string(), "AGGREGATE");
    assert!(Function::into_iter().any(|function| function == Function::Aggregate));
}

#[test]
fn aggregate_reference_functions_match_excel_semantics() {
    let mut model = new_empty_model();
    for (cell, value) in [("A1", "1"), ("A2", "2"), ("A3", "2"), ("A4", "5")] {
        model._set(cell, value);
    }
    for (cell, formula) in [
        ("C1", "=AGGREGATE(1,4,A1:A4)"),
        ("C2", "=AGGREGATE(2,4,A1:A4)"),
        ("C3", "=AGGREGATE(3,4,A1:A4)"),
        ("C4", "=AGGREGATE(4,4,A1:A4)"),
        ("C5", "=AGGREGATE(5,4,A1:A4)"),
        ("C6", "=AGGREGATE(6,4,A1:A4)"),
        ("C7", "=AGGREGATE(9,4,A1:A4)"),
        ("C8", "=AGGREGATE(10,4,A1:A4)"),
        ("C9", "=AGGREGATE(11,4,A1:A4)"),
        ("C10", "=AGGREGATE(12,4,A1:A4)"),
        ("C11", "=AGGREGATE(13,4,A1:A4)"),
    ] {
        model._set(cell, formula);
    }
    model.evaluate();

    for (cell, expected) in [
        ("C1", "2.5"),
        ("C2", "4"),
        ("C3", "4"),
        ("C4", "5"),
        ("C5", "1"),
        ("C6", "20"),
        ("C7", "10"),
        ("C8", "3"),
        ("C9", "2.25"),
        ("C10", "2"),
        ("C11", "2"),
    ] {
        assert_eq!(model._get_text(cell), expected, "cell {cell}");
    }
}

#[test]
fn aggregate_array_functions_and_error_options_work() {
    let mut model = new_empty_model();
    for (cell, value) in [("A1", "1"), ("A2", "2"), ("A3", "2"), ("A4", "5")] {
        model._set(cell, value);
    }
    model._set("A5", "=1/0");
    model._set("C1", "=AGGREGATE(14,6,A1:A5,2)");
    model._set("C2", "=AGGREGATE(15,6,A1:A5,2)");
    model._set("C3", "=AGGREGATE(16,6,A1:A5,0.25)");
    model._set("C4", "=AGGREGATE(18,6,A1:A5,0.4)");
    model._set("C5", "=AGGREGATE(9,6,A1:A5)");
    model._set("C6", "=AGGREGATE(9,4,A1:A5)");
    model.evaluate();

    assert_eq!(model._get_text("C1"), "2");
    assert_eq!(model._get_text("C2"), "2");
    assert_eq!(model._get_text("C3"), "1.75");
    assert_eq!(model._get_text("C4"), "2");
    assert_eq!(model._get_text("C5"), "10");
    assert_eq!(model._get_text("C6"), "#DIV/0!");
}

#[test]
fn aggregate_can_ignore_nested_subtotals_and_aggregates() {
    let mut model = new_empty_model();
    model._set("A1", "1");
    model._set("A2", "2");
    model._set("A3", "=SUBTOTAL(9,A1:A2)");
    model._set("A4", "=AGGREGATE(9,4,A1:A2)");
    model._set("C1", "=AGGREGATE(9,0,A1:A4)");
    model._set("C2", "=AGGREGATE(9,4,A1:A4)");
    model.evaluate();

    assert_eq!(model._get_text("C1"), "3");
    assert_eq!(model._get_text("C2"), "9");
}

#[test]
fn averagea_accepts_arrays_and_treats_text_as_zero() {
    let mut model = new_empty_model();
    model._set("A1", "=AVERAGEA({2,TRUE,\"text\"})");
    model.evaluate();
    assert_eq!(model._get_text("A1"), "1");
}

#[test]
fn implicit_intersection_reads_the_referenced_sheet() {
    let mut model = new_empty_model();
    model.add_sheet("Sheet2").unwrap();
    model._set("Sheet2!A1", "10");
    model._set("Sheet2!A2", "20");
    model._set("Sheet2!A3", "30");
    model._set("C2", "=@Sheet2!A1:A3");
    model.evaluate();
    assert_eq!(model._get_text("C2"), "20");
}
