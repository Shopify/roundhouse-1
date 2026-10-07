// ActiveModel's attribute types and ActiveRecord::Type's names over them
// (runtime/spinel/active_model_type.rb: Type::{Value,ImmutableString,
// String,Integer,Boolean,DateTime} with their helpers, ActiveRecord::
// Type::{Text,DateTime}) against activemodel / activerecord 8.2.0.alpha
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
        "AR::DateTime(precision: 6) type: :datetime",
        "ActiveModel::RangeError: 9223372036854775808 is out of range",
        "utc=false zone=\"ABC\"",
        "AR::DateTime(timezone: :local, precision: 3) from schema json: [true, true, nil, true, ",
        "AR::DateTime(timezone: :local, precision: 3) as_schema_json: {\"precision\" => 3, \"scale\" => nil, \"limit\" => nil, \"timezone\" => :local}",
    ] {
        assert!(stdout.contains(seen), "missing observation {seen:?}");
    }
}
