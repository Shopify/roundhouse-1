// `Rails.event` / `Rails.error` (runtime/ruby/active_support_reporters.rb)
// against the activesupport native Core runs (Shopify/rails 5813559f4aed,
// 8.2.0.alpha; tests/support/rails_oracle.rb): tests/reporters_runtime.rb runs
// the same observations natively and on the runtime port and requires them equal.

use std::path::Path;
use std::process::Command;

#[test]
fn event_and_error_reporters_answer_like_activesupport() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new("ruby")
        .arg(root.join("tests/reporters_runtime.rb"))
        .arg("compare")
        .current_dir(root)
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("Native ActiveSupport and emitted Rails.event/Rails.error agree"),
        "reporters diverged from activesupport\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    // Not a vacuous agreement: both sides observed the whole script.
    assert!(stdout.lines().count() > 70, "too few observations:\n{stdout}");
}
