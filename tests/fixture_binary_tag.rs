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

fn field(yaml: &str, field: &str) -> FixtureValue {
    let tree = vec![(std::path::PathBuf::from("test/fixtures/blobs.yml"), yaml.as_bytes().to_vec())]
        .into_iter()
        .collect();
    let app = roundhouse::ingest::ingest_app_from_tree(tree).expect("ingest");
    app.fixtures[0].records[&Symbol::from("one")][&Symbol::from(field)].clone()
}

// Rails renders the ERB before Psych reads the YAML, so a `!!binary` block
// around a tag is base64 text the row decodes to bytes.
#[test]
fn a_binary_tag_around_erb_decodes_the_rendered_base64() {
    let yaml = "one:\n  digest: !binary |-\n    <%= Base64.strict_encode64(\"abc\") %>\n";
    let FixtureValue::Ruby(expr) = field(yaml, "digest") else { panic!("expected a Ruby value") };
    let dump = format!("{expr:?}");
    assert!(dump.contains("unpack1"), "the rendered base64 is decoded: {dump}");
    assert!(dump.contains("strict_encode64"), "the tag's expression is kept: {dump}");
}

#[test]
fn a_binary_tag_around_embedded_erb_decodes_the_rendered_base64() {
    let yaml = "one:\n  digest: !!binary |\n    <%= prefix %>QUJD\n";
    let FixtureValue::Ruby(expr) = field(yaml, "digest") else { panic!("expected a Ruby value") };
    let dump = format!("{expr:?}");
    assert!(dump.contains("unpack1"), "{dump}");
}

#[test]
fn an_untagged_erb_scalar_is_not_decoded() {
    let yaml = "one:\n  digest: <%= Base64.strict_encode64(\"abc\") %>\n";
    let FixtureValue::Ruby(expr) = field(yaml, "digest") else { panic!("expected a Ruby value") };
    assert!(!format!("{expr:?}").contains("unpack1"));
}

#[test]
fn the_single_bang_binary_shorthand_is_decoded_too() {
    let yaml = "one:\n  digest: !binary \"MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=\"\n";
    assert_eq!(scalar(yaml, "digest"), "0123456789abcdef0123456789abcdef");
}

// Bytes a Rust String cannot hold (here 0x23 0xe2 0x98 then a stray 0xa0
// lead byte) are decoded by the row at load time.
#[test]
fn a_binary_tag_over_non_utf8_bytes_decodes_at_load_time() {
    let yaml = "one:\n  digest: !binary |\n    I+KYoP8=\n";
    let FixtureValue::Ruby(expr) = field(yaml, "digest") else { panic!("expected a Ruby value") };
    let dump = format!("{expr:?}");
    assert!(dump.contains("unpack1") && dump.contains("I+KYoP8="), "{dump}");
}
