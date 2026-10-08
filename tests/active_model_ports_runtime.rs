// The ActiveModel ports of the PostgreSQL CRuby tree (runtime/spinel/
// active_model_naming.rb, active_model_attribute_methods.rb,
// active_model_serialization.rb, active_support_string_inflections.rb)
// against the Rails native Core runs (Shopify/rails 5813559f4aed,
// 8.2.0.alpha; tests/support/rails_oracle.rb).
//
// tests/active_model_ports_runtime.rb runs the same models through the
// pinned activemodel and through the ports and requires every
// observation equal.

use std::path::Path;
use std::process::Command;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn active_model_ports_answer_like_rails() {
    let out = Command::new("ruby")
        .arg(root().join("tests/active_model_ports_runtime.rb"))
        .arg("compare")
        .current_dir(root())
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("Native Rails and the emitted ActiveModel ports agree"),
        "ports diverged from Rails\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    assert!(stdout.lines().count() > 55, "too few observations:\n{stdout}");
    for expected in [
        "model_name Admin::User: [\"Admin::User\", \"admin_user\", \"admin_users\", \"user\", \"admin/users\", \"user\", \"users\", \"user\", :\"admin/user\", \"User\", false, \"admin/users\", \"Admin::User\"]",
        "model_name Sheep: [\"Sheep\", \"sheep\", \"sheep\", \"sheep\", \"sheep\", \"sheep\", \"sheep_index\",",
        "composite: [\"1-2\", \"1_2\", nil, nil, nil, nil]",
        "generated methods: [:age, :age?, :age_changed?, :clear_age, :clear_name, :clear_nickname, :name,",
        "after undefine: [[], true, \"Bob\", true]",
        "include nested: {\"title\" => \"Dune\", \"author\" => {\"name\" => \"Frank\"}, \"coauthors\" => [{\"name\" => \"Brian\"}, {\"name\" => \"Kevin\"}]}",
        "as_json with root: [{\"book\" => {\"pages\" => 412}}, {\"pages\" => 412}, false]",
    ] {
        assert!(stdout.contains(expected), "missing {expected:?}:\n{stdout}");
    }
}
