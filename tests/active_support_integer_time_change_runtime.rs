// Integer#months / #years (runtime/spinel/active_support_integer_time.rb)
// and Time#change (runtime/spinel/active_support_time_change.rb) against
// the Rails native Core runs (Shopify/rails 5813559f4aed, 8.2.0.alpha;
// tests/support/rails_oracle.rb).
// tests/active_support_integer_time_change_runtime.rb runs the same
// receivers through the pinned activesupport and through the ports and
// requires every observation equal.
//
// Rails' :load_active_support initializer loads both with
// active_support/all, so Core's class bodies call them with no require
// (`TTL_CASES = [{ expected_ttl: 1.year.to_i }]`,
// `TRIGGERED_TIMESTAMP = Time.now.utc.change(nsec: 0)`); the emitted
// PostgreSQL tree loads the ports where a call names them
// (project.rs::BOOT_LOADED_CORE_EXTENSIONS).

use std::path::Path;
use std::process::Command;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn integer_time_and_time_change_answer_like_rails() {
    let out = Command::new("ruby")
        .arg(root().join("tests/active_support_integer_time_change_runtime.rb"))
        .arg("compare")
        .current_dir(root())
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("Native Rails and emitted Integer#year / Time#change agree"),
        "ports diverged from Rails\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    assert!(stdout.lines().count() > 25, "too few observations:\n{stdout}");
    for expected in [
        "1.year: [\"ActiveSupport::Duration\", \"1 year\", 31556952]",
        "Float responds to year: false",
        "utc nsec and usec: \"ArgumentError: ",
        "Time.now.utc.change(nsec: 0) nsec: 0",
    ] {
        assert!(stdout.contains(expected), "missing {expected:?}:\n{stdout}");
    }
}
