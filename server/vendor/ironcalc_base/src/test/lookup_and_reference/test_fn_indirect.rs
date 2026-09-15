#![allow(clippy::unwrap_used)]

use crate::test::util::new_empty_model;

#[test]
fn indirect_supports_a1_switch_and_relative_r1c1_references() {
    let mut model = new_empty_model();
    model._set("A1", "11");
    model._set("B2", "22");
    model._set("A8", "88");

    model._set("D4", r#"=INDIRECT("R[-2]C[-2]",FALSE)"#);
    model._set("D5", r#"=INDIRECT("R1C1",0)"#);
    model._set("D6", r#"=INDIRECT("B2",TRUE)"#);
    model._set("D8", r#"=INDIRECT("RC[-3]",FALSE)"#);
    model.evaluate();

    assert_eq!(model._get_text("D4"), "22");
    assert_eq!(model._get_text("D5"), "11");
    assert_eq!(model._get_text("D6"), "22");
    assert_eq!(model._get_text("D8"), "88");
}

#[test]
fn indirect_supports_r1c1_ranges_quoted_sheets_and_defined_names() {
    let mut model = new_empty_model();
    model._set("A1", "1");
    model._set("B1", "2");
    model._set("A2", "3");
    model._set("B2", "4");
    model
        .new_defined_name("ChosenCell", None, "Sheet1!B2")
        .unwrap();

    model.new_sheet();
    model.rename_sheet("Sheet2", "Data Set").unwrap();
    model.set_user_input(1, 2, 3, "91".to_string()).unwrap();
    model._set("D4", r#"=SUM(INDIRECT("R1C1:R2C2",FALSE))"#);
    model._set("D5", r#"=INDIRECT("'Data Set'!R2C3",FALSE)"#);
    model._set("D6", r#"=INDIRECT("ChosenCell",FALSE)"#);
    model.evaluate();

    assert_eq!(model._get_text("D4"), "10");
    assert_eq!(model._get_text("D5"), "91");
    assert_eq!(model._get_text("D6"), "4");
}

#[test]
fn indirect_rejects_invalid_or_out_of_bounds_r1c1_references() {
    let mut model = new_empty_model();
    model._set("A1", r#"=INDIRECT("R[-1]C",FALSE)"#);
    model._set("A2", r#"=INDIRECT("R0C1",FALSE)"#);
    model._set("A3", r#"=INDIRECT("R1C16385",FALSE)"#);
    model._set("A4", r#"=INDIRECT("R1C1:C2",FALSE)"#);
    model.evaluate();

    assert_eq!(model._get_text("A1"), "#REF!");
    assert_eq!(model._get_text("A2"), "#REF!");
    assert_eq!(model._get_text("A3"), "#REF!");
    assert_eq!(model._get_text("A4"), "#REF!");
}
