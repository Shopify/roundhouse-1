// ActiveModel's attribute types and ActiveRecord::Type's names over them
// (runtime/spinel/active_model_type.rb: Type::{Value,ImmutableString,
// String,Integer,BigInteger,Binary,Boolean,DateTime,Decimal,Time} with their helpers,
// ActiveRecord::Type::{Text,DateTime,Time,Json}, OID::Jsonb) against activemodel / activerecord 8.2.0.alpha
// (Core's pinned Shopify/rails 5813559f, from CORE_GEM_HOME):
// tests/active_model_type_runtime.rb runs the same observations natively
// and on the port and requires every observation equal.

use std::path::Path;
use std::process::Command;

#[test]
fn attribute_types_answer_like_activemodel() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new("ruby")
        .arg(root.join("tests/active_model_type_runtime.rb"))
        .arg("compare")
        .current_dir(root)
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("Native ActiveModel and the emitted ActiveModel types agree"),
        "ActiveModel types diverged from activemodel\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    // Not a vacuous agreement: every type saw the whole battery, and the
    // failure paths ran.
    assert!(stdout.lines().count() > 10_000, "too few observations: {}", stdout.lines().count());
    for seen in [
        "AR::Integer(limit: 8) type: :integer",
        "AR::Text type: :text",
        "AR::Time(precision: 6) type: :time",
        "OID::Jsonb type: :jsonb",
        "Json changed_in_place?: [false, true]",
        r#"lookup time json: "[\"ActiveRecord::Type::Time\" US-ASCII frozen=true, \"ActiveRecord::Type::Json\""#,
        "AR::DateTime(precision: 6) type: :datetime",
        "ActiveModel::RangeError: 9223372036854775808 is out of range",
        "utc=false zone=\"ABC\"",
        "AR::DateTime(timezone: :local, precision: 3) from schema json: [true, true, nil, true, ",
        "QueryPredicates type?: [true, false, true, false, true, false]",
        "AR::BigInteger serialize 9223372036854775808: 9223372036854775808",
        "AR::Binary type: :binary",
        "AR::Decimal type: :decimal",
        "AR::Decimal(precision: 20, scale: 3) cast \"1.7\": BigDecimal(0.17e1 precision=2 frozen=true)",
        "Decimal#1 cast BigDecimal(0.123456e1): \"BigDecimal(0.1235e1 precision=4 frozen=true)\"",
        "MoneyColumn-like serialize: [nil, BigDecimal(0.12e2 precision=2 frozen=true), BigDecimal(0.35e1 precision=2 frozen=true), BigDecimal(0.725e1 precision=3 frozen=true), :decimal, false]",
        r#"Binary serialize: "[\"ActiveModel::Type::Binary::Data\""#,
        "AR::DateTime(timezone: :local, precision: 3) as_schema_json: {\"precision\" => 3, \"scale\" => nil, \"limit\" => nil, \"timezone\" => :local}",
    ] {
        assert!(stdout.contains(seen), "missing observation {seen:?}");
    }
}
