#![allow(clippy::unwrap_used)]

use crate::test::util::new_empty_model;

#[test]
fn switch_uses_legacy_implicit_intersection_for_expression_and_case_ranges() {
    let mut model = new_empty_model();
    model._set("A1", "10");
    model._set("A2", "20");
    model._set("A3", "30");

    // The expression range intersects row 2 at A2.
    model._set("B2", "=SWITCH(A1:A3,20,\"hit\",\"miss\")");
    // The case range intersects row 3 at A3.
    model._set("B3", "=SWITCH(30,A1:A3,\"hit\",\"miss\")");
    // Row 5 does not intersect the vertical range.
    model._set("B5", "=SWITCH(A1:A3,20,\"hit\",\"miss\")");

    model.evaluate();

    assert_eq!(model._get_text("B2"), "hit");
    assert_eq!(model._get_text("B3"), "hit");
    assert_eq!(model._get_text("B5"), "#VALUE!");
}
