// ActiveSupport::Cache::FileStore (runtime/spinel/active_support_file_store.rb)
// against the activesupport native Core runs (Shopify/rails 5813559f4aed,
// 8.2.0.alpha; tests/support/rails_oracle.rb): tests/file_store_runtime.rb
// runs the same observations natively and on the runtime port, each in
// its own temporary directory, and requires them equal.

use std::path::Path;
use std::process::Command;

#[test]
fn file_store_answers_like_activesupport() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new("ruby")
        .arg(root.join("tests/file_store_runtime.rb"))
        .arg("compare")
        .current_dir(root)
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("Native ActiveSupport and the emitted FileStore agree"),
        "FileStore diverged from activesupport\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    // Not a vacuous agreement: both sides observed the whole script.
    assert!(stdout.lines().count() > 70, "too few observations:\n{stdout}");
}
