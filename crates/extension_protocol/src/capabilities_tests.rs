use super::*;

#[test]
fn wire_names_round_trip() {
    for capability in Capability::ALL {
        let encoded = serde_json::to_value(capability).expect("capability serializes");
        assert_eq!(encoded, serde_json::json!(capability.as_str()));
        let decoded: Capability = serde_json::from_value(encoded).expect("capability decodes");
        assert_eq!(decoded, *capability);
    }
}

#[test]
fn all_lists_every_variant_exactly_once() {
    let unique: std::collections::BTreeSet<_> = Capability::ALL.iter().collect();
    assert_eq!(unique.len(), Capability::ALL.len());
}
