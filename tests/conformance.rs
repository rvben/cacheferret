//! `cacheferret schema` must validate against clispec.dev v0.3.

#[test]
fn schema_conforms_to_clispec_v0_3() {
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../schemas/clispec-v0.3.json"))
            .expect("vendored clispec schema is valid JSON");
    let instance = cacheferret::schema::contract();
    let validator = jsonschema::validator_for(&schema).expect("compile clispec schema");

    if !validator.is_valid(&instance) {
        let errors: Vec<String> = validator
            .iter_errors(&instance)
            .map(|error| format!("{} at {}", error, error.instance_path()))
            .collect();
        panic!(
            "schema does not conform to clispec v0.3:\n{}",
            errors.join("\n")
        );
    }
}
