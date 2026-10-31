use super::required;

#[test]
fn required_trims_the_value() {
    assert_eq!(required("  work  ", "Login id"), Ok("work".to_string()));
}

#[test]
fn required_rejects_a_blank_value_naming_the_field() {
    assert_eq!(
        required(" \t ", "Login id"),
        Err("Login id is required".to_string())
    );
    assert_eq!(
        required("", "Login id"),
        Err("Login id is required".to_string())
    );
}
