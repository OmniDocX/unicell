#[path = "../src/validation_runtime.rs"]
mod validation_runtime;

#[test]
fn public_transport_contract_is_json_round_trippable() {
    use validation_runtime::*;

    let request = ValidationRequest {
        rule: ValidationRule {
            validation_type: ValidationType::Custom,
            operator: None,
            allow_blank: true,
            formula1: Some("LEN(A1)<=10".to_string()),
            formula2: None,
            anchor: CellAddress {
                sheet: 0,
                row: 1,
                column: 1,
            },
        },
        target: CellAddress {
            sheet: 0,
            row: 3,
            column: 1,
        },
        candidate: CandidateValue::Text("hello".to_string()),
    };
    let json = serde_json::to_string(&request).unwrap();
    let decoded: ValidationRequest = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded, request);
    assert_eq!(
        ValidationType::from_ooxml("textLength"),
        Some(ValidationType::TextLength)
    );
    assert_eq!(
        ValidationOperator::from_ooxml("notBetween"),
        Some(ValidationOperator::NotBetween)
    );
    assert_eq!(
        anchor_from_sqref(2, "$C$7:$D$9 A1"),
        Some(CellAddress {
            sheet: 2,
            row: 7,
            column: 3
        })
    );
}
