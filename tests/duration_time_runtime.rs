//! ActiveSupport::Duration and Time arithmetic as Core uses it, on both
//! runtime ports against the Rails native Core runs: `Time + Duration`,
//! `Duration#since`/`ago` with calendar parts in the order they were
//! added, `to_i` with calendar parts, and `==`/`<=>`/`eql?`/`<` between
//! mixed parts. tests/duration_time_runtime.rb prints the same
//! observations natively and on each port, under three time zones (two
//! with DST), and requires them equal.
use std::path::Path;
use std::process::Command;

#[test]
fn duration_time_arithmetic_answers_like_activesupport() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new("ruby")
        .arg(root.join("tests/duration_time_runtime.rb"))
        .arg("compare")
        .current_dir(root)
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success()
            && stdout.contains("Native ActiveSupport and the Duration ports agree"),
        "Duration/Time arithmetic diverged from activesupport\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    for line in [
        "T utc jan31 + 1.months: Time 2024-02-29 12:30:15.5 UTC 0 false\n",
        "1.month + 1.day.since(utc leap): Time 2024-03-30 00:00:00 UTC 0 false\n",
        "1.day + 1.month.since(utc leap): Time 2024-04-01 00:00:00 UTC 0 false\n",
        "T local mar9 + 1.days: Time 2024-03-10 12:00:00 -0400 -14400 true\n",
        "1.day <=> 24.hours: Integer 0\n",
    ] {
        assert!(stdout.contains(line), "missing {line:?}");
    }
    assert!(
        stdout.lines().count() > 20000,
        "too few observations:\n{stdout}"
    );
}
