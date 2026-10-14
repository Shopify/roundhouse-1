// ActiveModel::Attribute and its private subclasses
// (runtime/spinel/active_model_attribute.rb) against activemodel
// 8.2.0.alpha (Core's pinned Shopify/rails 5813559f,
// tests/support/rails_oracle.rb): tests/active_model_attribute_runtime.rb
// runs the same observations natively and on the port and requires every
// observation equal.

use std::path::Path;
use std::process::Command;

#[test]
fn attributes_answer_like_activemodel() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new("ruby")
        .arg(root.join("tests/active_model_attribute_runtime.rb"))
        .arg("compare")
        .current_dir(root)
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("Native ActiveModel::Attribute and the port agree"),
        "ActiveModel::Attribute diverged from activemodel\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    assert!(stdout.lines().count() > 90, "too few observations: {}", stdout.lines().count());
    for seen in [
        "const_get ActiveModel::Attribute::FromDatabase: \"ActiveModel::Attribute::FromDatabase\"",
        "const_get ActiveModel::Attribute::FromUser: \"ActiveModel::Attribute::FromUser\"",
        "const_get ActiveModel::Attribute::Uninitialized: \"ActiveModel::Attribute::Uninitialized\"",
        "direct ActiveModel::Attribute::FromDatabase: raised NameError: private constant ActiveModel::Attribute::FromDatabase referenced",
        "from_database value: 1",
        "with_value_from_user changed?: [true, false]",
        "QueryAttribute superclass: \"ActiveModel::Attribute\"",
        "QueryAttribute value_for_database: 1",
        "QueryAttribute unboundable?: [nil, 1, -1]",
        "null with_value_from_user: raised ActiveModel::MissingAttributeError: can't write unknown attribute `n`",
    ] {
        assert!(stdout.contains(seen), "missing observation {seen:?}\n{stdout}");
    }
}
