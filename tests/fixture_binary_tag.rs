//! A `!!binary` fixture value is base64 text for bytes (yaml.org/type/binary).
//! Psych decodes it before the row is typed, so a `bytea` column gets the
//! bytes, not the base64 spelling (which a length CHECK on the column rejects).

use roundhouse::dialect::FixtureValue;
use roundhouse::Symbol;

fn scalar(yaml: &str, field: &str) -> String {
    let tree = vec![(std::path::PathBuf::from("test/fixtures/blobs.yml"), yaml.as_bytes().to_vec())]
        .into_iter()
        .collect();
    let app = roundhouse::ingest::ingest_app_from_tree(tree).expect("ingest");
    match &app.fixtures[0].records[&Symbol::from("one")][&Symbol::from(field)] {
        FixtureValue::Scalar(s) => s.clone(),
        other => panic!("expected a scalar, got {other:?}"),
    }
}

#[test]
fn a_binary_tag_is_decoded_to_its_bytes() {
    let yaml = "one:\n  digest: !!binary \"MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=\"\n";
    assert_eq!(scalar(yaml, "digest"), "0123456789abcdef0123456789abcdef");
}

#[test]
fn an_untagged_scalar_is_left_alone() {
    let yaml = "one:\n  digest: \"MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=\"\n";
    assert_eq!(scalar(yaml, "digest"), "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=");
}
