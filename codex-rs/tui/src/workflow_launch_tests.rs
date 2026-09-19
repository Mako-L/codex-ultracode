use super::validate_arguments;
use serde_json::json;

#[test]
fn workflow_concurrency_accepts_only_bounded_integers() {
    for limit in [json!(1), json!(16)] {
        assert!(validate_arguments(&json!({"script":"return 1;","concurrency":limit})).is_ok());
    }
    for limit in [
        json!(0),
        json!(17),
        json!(-1),
        json!(1.5),
        json!("1"),
        json!(null),
        json!(true),
    ] {
        let error = validate_arguments(&json!({"script":"return 1;","concurrency":limit}));
        assert!(error.is_err(), "invalid concurrency was accepted: {limit}");
    }
    assert!(validate_arguments(&json!({"script":"return 1;"})).is_ok());
}

#[test]
fn workflow_isolate_writes_accepts_only_booleans() {
    assert!(validate_arguments(&json!({"script":"return 1;","isolateWrites":false})).is_ok());
    assert!(validate_arguments(&json!({"script":"return 1;","isolateWrites":true})).is_ok());
    assert!(validate_arguments(&json!({"script":"return 1;","isolateWrites":"false"})).is_err());
    let merged = super::with_isolate_writes(&json!({"script":"return 1;"}), false);
    assert_eq!(merged["isolateWrites"], json!(false));
    let kept = super::with_isolate_writes(&json!({"script":"return 1;","isolateWrites":true}), false);
    assert_eq!(kept["isolateWrites"], json!(true));
}
