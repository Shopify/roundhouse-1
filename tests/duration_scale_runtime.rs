//! ActiveSupport::Duration `*`, `/`, `%`, `Duration.build` and a number
//! on the left of a Duration operator (`2 * 30.minutes`, `3600 /
//! 30.minutes`, `1.5 <= 2.seconds`, Rails' `coerce` + `Duration::Scalar`)
//! on the runtime ports against the Rails native Core runs:
//! tests/duration_scale_runtime.rb prints the same observations natively
//! and on each port and requires them equal. Core's
//! `ExternalProviderTokens` reads `SESSION_TTL * 2` at class load, which
//! raised NoMethodError for `ActiveSupport::Duration#*` in every
//! test's all-model setup.
use std::path::Path;
use std::process::Command;

#[test]
fn duration_scaling_answers_like_activesupport() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new("ruby")
        .arg(root.join("tests/duration_scale_runtime.rb"))
        .arg("compare")
        .current_dir(root)
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("Native ActiveSupport and the Duration ports agree"),
        "Duration scaling diverged from activesupport\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    // Rails keeps the part it scaled: "60 minutes", not "1 hour".
    assert!(stdout.contains("30.minutes * 2: Duration 60 minutes 3600 3600\n"), "{stdout}");
    assert!(stdout.contains("1.hours / 1.minutes: Integer 60\n"), "{stdout}");
    assert!(stdout.contains("1.hours / 2: Duration 1800 seconds 1800 1800\n"), "{stdout}");
    assert!(stdout.lines().count() > 3000, "too few observations:\n{stdout}");
}
