//! ActiveSupport::Duration `+`, `-`, unary `-` and `inspect` ("2 days",
//! "1 day and 3 hours") on both runtime ports against the Rails native
//! Core runs: tests/duration_arithmetic_runtime.rb prints the same
//! observations natively and on each port and requires them equal.
use std::path::Path;
use std::process::Command;

#[test]
fn duration_arithmetic_and_inspect_answer_like_activesupport() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new("ruby")
        .arg(root.join("tests/duration_arithmetic_runtime.rb"))
        .arg("compare")
        .current_dir(root)
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("Native ActiveSupport and the Duration ports agree"),
        "Duration arithmetic diverged from activesupport\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    assert!(stdout.contains("2.days: Duration 2 days 172800 172800\n"), "{stdout}");
    assert!(stdout.lines().count() > 1000, "too few observations:\n{stdout}");
}
