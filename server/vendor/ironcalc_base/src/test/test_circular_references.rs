#![allow(clippy::unwrap_used)]

use crate::test::util::new_empty_model;
use crate::{cell::CellValue, types::IterationSettings, Model};

fn number_at(model: &Model<'_>, cell: &str) -> f64 {
    match model
        .get_cell_value_by_ref(&format!("Sheet1!{cell}"))
        .unwrap()
    {
        CellValue::Number(value) => value,
        other => panic!("Expected a number at {cell}, got {other:?}"),
    }
}

#[test]
fn test_simple_circ() {
    let mut model = new_empty_model();
    model._set("A1", "=A1+1");
    model.evaluate();
    assert_eq!(model._get_text("A1"), "#CIRC!");
}

#[test]
fn test_simple_circ_propagate() {
    let mut model = new_empty_model();
    model._set("A1", "=B6");
    model._set("A2", "=A1+1");
    model._set("A3", "=A2+1");
    model._set("A4", "=A3+5");
    model._set("B6", "=A4*7");
    model.evaluate();
    assert_eq!(model._get_text("A1"), "#CIRC!");
    assert_eq!(model._get_text("A2"), "#CIRC!");
    assert_eq!(model._get_text("A3"), "#CIRC!");
    assert_eq!(model._get_text("A4"), "#CIRC!");
    assert_eq!(model._get_text("B6"), "#CIRC!");
}

#[test]
fn test_iterative_self_reference_converges() {
    let mut model = new_empty_model();
    model._set("A1", "=(A1+10)/2");
    model.set_iteration_options(true, 100, 1e-9).unwrap();

    model.evaluate();

    assert!((number_at(&model, "A1") - 10.0).abs() <= 1e-9);
}

#[test]
fn test_iterative_mutual_reference_converges() {
    let mut model = new_empty_model();
    model._set("A1", "=(B1+1)/2");
    model._set("B1", "=(A1+1)/2");
    model.set_iteration_options(true, 100, 1e-9).unwrap();

    model.evaluate();

    assert!((number_at(&model, "A1") - 1.0).abs() <= 1e-9);
    assert!((number_at(&model, "B1") - 1.0).abs() <= 1e-9);
}

#[test]
fn test_iterative_maximum_iterations_is_exact() {
    let mut model = new_empty_model();
    model._set("A1", "=A1+1");
    model.set_iteration_options(true, 5, 0.0).unwrap();

    model.evaluate();

    assert_eq!(number_at(&model, "A1"), 5.0);
}

#[test]
fn test_iterative_maximum_change_stops_calculation() {
    let mut model = new_empty_model();
    model._set("A1", "=(A1+10)/2");
    model.set_iteration_options(true, 100, 1.0).unwrap();

    model.evaluate();

    assert_eq!(number_at(&model, "A1"), 9.375);
}

#[test]
fn test_iteration_settings_validate_and_roundtrip() {
    let mut model = new_empty_model();
    let settings = IterationSettings {
        enabled: true,
        maximum_iterations: 37,
        maximum_change: 0.000_01,
    };
    model.set_iteration_settings(settings.clone()).unwrap();

    let bytes = model.to_bytes();
    let restored = Model::from_bytes(&bytes, "en").unwrap();

    assert_eq!(restored.get_iteration_settings(), settings);
    assert!(model.set_iteration_options(true, 0, 0.1).is_err());
    assert!(model.set_iteration_options(true, 32_768, 0.1).is_err());
    assert!(model.set_iteration_options(true, 100, -0.1).is_err());
    assert!(model.set_iteration_options(true, 100, f64::NAN).is_err());
}

#[test]
fn test_disabling_iteration_restores_circular_error() {
    let mut model = new_empty_model();
    model._set("A1", "=A1+1");
    model.set_iteration_options(true, 5, 0.0).unwrap();
    model.evaluate();
    assert_eq!(number_at(&model, "A1"), 5.0);

    model.set_iteration_options(false, 5, 0.0).unwrap();
    model.evaluate();

    assert_eq!(model._get_text("A1"), "#CIRC!");
}
