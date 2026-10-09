// `Rails.logger` on the ruby-family trees (runtime/spinel/rails_configuration.rb)
// against the Rails native Core runs (Shopify/rails 5813559f4aed, 8.2.0.alpha;
// tests/support/rails_oracle.rb). tests/rails_logger_runtime.rb boots a minimal
// Rails app in the test environment and loads the emitted runtime, from the
// same kind of app root, logs through Rails.logger in every call form and
// requires every observation equal, the log file's bytes included.
//
// Before, the emitted Rails.logger was the shared runtime's stand-in, which
// wrote "[rails] WARN ..." to stderr through Kernel#warn. Core's test
// environment turns every warning into StrictWarning::Offense, so the first
// log line at boot (bugsnag's "No API key has been set") killed the process
// for all 1,000 unit-1000 ids. Rails writes that line to log/test.log.

use std::path::Path;
use std::process::Command;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn rails_logger_writes_the_log_device_like_rails() {
    let out = Command::new("ruby")
        .arg(root().join("tests/rails_logger_runtime.rb"))
        .arg("compare")
        .current_dir(root())
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("Native Rails and the emitted Rails.logger agree"),
        "Rails.logger diverged from Rails\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    // Not a vacuous agreement: what Rails answered is a file logger that
    // never reaches Kernel#warn, at :debug by default and at the
    // configured level otherwise.
    for expected in [
        "default: level: 0",
        "default: outputs to log/test.log: true",
        "default: warnings: []",
        "default: log: \"debug line\\ninfo line\\nx\\n",
        "info: level: 1",
        "info: debug block called: false",
        "info: log: \"info line\\nx\\n",
        "unwritable: outputs to STDERR: true",
        "unwritable: stderr: \"Rails Error: Unable to access log file. Please ensure that ROOT/log/test.log exists",
    ] {
        assert!(stdout.contains(expected), "missing {expected:?}:\n{stdout}");
    }
}

/// A configured logger the tree cannot carry (no ::Logger: Rails 8.2's
/// TaggedLogging/BroadcastLogger shapes) is refused by name, and the log
/// still goes to the framework default device rather than stderr.
#[test]
fn rails_logger_refuses_a_logger_it_cannot_carry_by_name() {
    let dir = std::env::temp_dir().join(format!("rh_rails_logger_refusal_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let out = Command::new("ruby")
        .arg(root().join("tests/rails_logger_runtime.rb"))
        .arg("emitted")
        .arg(&dir)
        .arg("refusal")
        .current_dir(&dir)
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let log = std::fs::read_to_string(dir.join("log/test.log")).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "refusal run failed\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}");
    assert!(
        stderr.contains("roundhouse: error[rails_logger_not_carried]: config.logger is Object")
            && stderr.contains("Rails.logger is the framework default, an ActiveSupport::Logger on "),
        "no named refusal:\n{stderr}"
    );
    assert_eq!(stderr.matches("rails_logger_not_carried").count(), 1, "refused more than once:\n{stderr}");
    assert!(stdout.contains("warnings: []"), "{stdout}");
    assert!(log.starts_with("debug line\ninfo line\nx\n"), "log/test.log: {log:?}");
}
