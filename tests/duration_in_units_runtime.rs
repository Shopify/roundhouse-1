//! ActiveSupport::Duration readers (`in_seconds`, `in_minutes` ..
//! `in_years`, `to_i`, `to_f`) on both runtime ports against the Rails
//! native Core runs: tests/duration_in_units_runtime.rb prints the same
//! observations natively and on each port and requires them equal.
//! Core's Hedwig::Configuration reads `ActiveSupport::Duration.minutes(2)
//! .in_seconds` at load time.
use std::path::Path;
use std::process::Command;

#[test]
fn duration_readers_answer_like_activesupport() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new("ruby")
        .arg(root.join("tests/duration_in_units_runtime.rb"))
        .arg("compare")
        .current_dir(root)
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("Native ActiveSupport and the Duration ports agree"),
        "Duration readers diverged from activesupport\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    assert!(stdout.lines().count() > 400, "too few observations:\n{stdout}");
}
