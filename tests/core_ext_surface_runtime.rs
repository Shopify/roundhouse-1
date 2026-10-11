//! The activesupport core_ext methods Shopify Core sends to builtins
//! (`exclude?`, `in?`, `index_with`, `squish`, `deep_dup`, `in_groups_of`,
//! `except!`, `with`, ...) on the CRuby overlay against the Rails native Core
//! runs: tests/core_ext_surface_runtime.rb prints the same observations
//! natively and on runtime/spinel/scaffold/ruby_overlay/runtime/
//! active_support_core_ext.rb and requires them equal. The emitted Core
//! tree raised NoMethodError for `Array#exclude?` at boot
//! (`Podding::Database::PodConnectionConfigProvider`): Core defines
//! `exclude?` on its own classes, so `lower::exclude_predicate` stands
//! down and the sends reach the runtime as written.
use std::path::Path;
use std::process::Command;

#[test]
fn core_ext_surface_answers_like_activesupport() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new("ruby")
        .arg(root.join("tests/core_ext_surface_runtime.rb"))
        .arg("compare")
        .current_dir(root)
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("Native ActiveSupport and the overlay core_ext agree"),
        "core_ext diverged from activesupport\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    assert!(stdout.contains("[1, 2, 3].exclude?(4): TrueClass true\n"), "{stdout}");
    assert!(stdout.contains("\"ab\".exclude?('b'): FalseClass false\n"), "{stdout}");
    assert!(stdout.contains("{a: 1}.exclude?(:a): FalseClass false\n"), "{stdout}");
    assert!(stdout.contains("1.in?([1, 2, 3]): TrueClass true\n"), "{stdout}");
    assert!(stdout.lines().count() > 900, "too few observations:\n{stdout}");
}
