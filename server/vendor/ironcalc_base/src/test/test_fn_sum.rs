#![allow(clippy::unwrap_used)]

use crate::test::util::new_empty_model;

#[test]
fn bounded_ranges_and_whole_axes_preserve_sum_semantics() {
    let mut model = new_empty_model();
    model._set("A1", "2");
    model._set("A3", "5");
    model._set("C1", "7");
    model._set("B2", "ignored text");
    model._set("D4", "TRUE");
    for (cell, formula) in [
        ("F5", "=SUM(A1:C3)"),
        ("F6", "=SUM(C3:A1)"),
        ("F7", "=SUM(A:A)"),
        ("F8", "=SUM(1:1)"),
        ("F9", "=SUM(A1)"),
        ("F10", "=SUM(X1:Y2)"),
        ("F11", "=SUM(X:X)"),
        ("F12", "=SUM(20:20)"),
    ] {
        model._set(cell, formula);
    }
    model.evaluate();
    for (cell, expected) in [
        ("F5", "14"),
        ("F6", "14"),
        ("F7", "7"),
        ("F8", "9"),
        ("F9", "2"),
        ("F10", "0"),
        ("F11", "0"),
        ("F12", "0"),
    ] {
        assert_eq!(model._get_text(cell), expected, "{cell}");
    }

    // Whole-axis bounds must be recomputed after the sheet grows.
    model._set("A100", "11");
    model._set("Z1", "13");
    model.evaluate();
    assert_eq!(model._get_text("F7"), "18");
    assert_eq!(model._get_text("F8"), "22");
    assert_eq!(model._get_text("F5"), "14");
}

#[test]
fn bounded_sum_preserves_errors_and_last_cell_references() {
    let mut model = new_empty_model();
    model._set("XFD1048576", "19");
    model._set("A1", "=SUM(XFC1048575:XFD1048576)");
    model._set("B1", "=1/0");
    model._set("A2", "=SUM(B1:C1)");
    model.evaluate();
    assert_eq!(model._get_text("A1"), "19");
    assert_eq!(model._get_text("A2"), "#DIV/0!");
}

#[test]
fn test_fn_sum_arguments() {
    let mut model = new_empty_model();
    model._set("A1", "=SUM()");
    model._set("A2", "=SUM(1, 2, 3)");
    model._set("A3", "=SUM(1, )");
    model._set("A4", "=SUM(1,   , 3)");

    model.evaluate();

    assert_eq!(model._get_text("A1"), *"#ERROR!");
    assert_eq!(model._get_text("A2"), *"6");
    assert_eq!(model._get_text("A3"), *"1");
    assert_eq!(model._get_text("A4"), *"4");
}

#[test]
fn arrays() {
    let mut model = new_empty_model();
    model._set("A1", "=SUM({1, 2, 3})");
    model._set("A2", "=SUM({1; 2; 3})");
    model._set("A3", "=SUM({1, 2; 3, 4})");
    model._set("A4", "=SUM({1, 2; 3, 4; 5, 6})");

    model.evaluate();

    assert_eq!(model._get_text("A1"), *"6");
    assert_eq!(model._get_text("A2"), *"6");
    assert_eq!(model._get_text("A3"), *"10");
    assert_eq!(model._get_text("A4"), *"21");
}

#[test]
fn test_fn_sum_text_converted_to_number() {
    let mut model = new_empty_model();

    model._set("A1", r#"=SUM("1")"#);
    model._set("A2", r#"=SUM("1e2")"#);

    model.evaluate();

    assert_eq!(model._get_text("A1"), *"1");
    assert_eq!(model._get_text("A2"), *"100");
}

#[test]
fn test_fn_sum_invalid_text() {
    let mut model = new_empty_model();

    model._set("A1", r#"=SUM("a")"#);

    model.evaluate();

    assert_eq!(model._get_text("A1"), *"#VALUE!");
}

#[test]
fn test_fn_sum_text_in_range_not_converted() {
    let mut model = new_empty_model();

    model._set("A1", r#"=SUM(B1:D1)"#);
    model._set("B1", r#"="100""#);

    model.evaluate();

    assert_eq!(model._get_text("A1"), *"0");
}

#[test]
fn test_fn_sum_text_in_reference_not_converted() {
    let mut model = new_empty_model();

    model._set("A1", r#"=SUM(B1)"#);
    model._set("B1", r#"="100""#);

    model.evaluate();

    assert_eq!(model._get_text("A1"), *"0");
}

#[test]
fn test_fn_sum_text_in_indirect_reference_not_converted() {
    let mut model = new_empty_model();

    model._set("A1", r#"=SUM(INDIRECT("B1"))"#);
    model._set("B1", r#"="100""#);

    model.evaluate();

    assert_eq!(model._get_text("A1"), *"0");
}

#[test]
fn test_fn_sum_text_in_indirect_reference() {
    let mut model = new_empty_model();

    model._set("A1", r#"=SUM(INDIRECT("B1"))"#);
    model._set("B1", r#"100"#);

    model.evaluate();

    assert_eq!(model._get_text("A1"), *"100");
}

#[test]
fn test_fn_sum_invalid_text_in_range() {
    let mut model = new_empty_model();

    model._set("A1", r#"=SUM(B1:D1)"#);
    model._set("B1", "a");

    model.evaluate();

    assert_eq!(model._get_text("A1"), *"0");
}

#[test]
fn test_fn_sum_invalid_text_in_reference() {
    let mut model = new_empty_model();

    model._set("A1", r#"=SUM(B1)"#);
    model._set("B1", r#"a"#);

    model.evaluate();

    assert_eq!(model._get_text("A1"), *"0");
}

#[test]
fn test_fn_sum_boolean_values_converted() {
    let mut model = new_empty_model();

    model._set("A1", r#"=SUM(TRUE)"#);
    model._set("A2", r#"=SUM(FALSE)"#);

    model.evaluate();

    assert_eq!(model._get_text("A1"), *"1");
    assert_eq!(model._get_text("A2"), *"0");
}
