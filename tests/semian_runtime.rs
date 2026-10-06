// semian 0.28.3 (runtime/spinel/semian.rb and
// runtime/spinel/semian_activerecord_postgresql_adapter.rb) against the gem:
// tests/semian_runtime.rb runs the same observations natively and on the
// runtime port and requires them equal.
//
// Both tests need the semian 0.28.3 gem (its compiled bulkhead ext) on
// GEM_PATH; the adapter test also needs activerecord 8.1.4, the pg gem and a
// local PostgreSQL server in ROUNDHOUSE_PG_TEST_URL. It creates and drops its
// own database there.

use std::path::Path;
use std::process::Command;

fn compare(part: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new("ruby")
        .arg(root.join("tests/semian_runtime.rb"))
        .args(["compare", part])
        .current_dir(root)
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success()
            && stdout.contains(&format!("Native semian 0.28.3 and the emitted semian port agree on {part}")),
        "semian {part} diverged from the gem\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    stdout
}

#[test]
#[ignore = "needs the semian 0.28.3 gem on GEM_PATH"]
fn semian_core_answers_like_the_gem() {
    let stdout = compare("core");
    // Not a vacuous agreement: every SEMIAN_* environment ran the whole script.
    assert!(stdout.lines().count() > 400, "too few observations:\n{stdout}");
    for env in ["SEMIAN_DISABLED", "SEMIAN_SEMAPHORES_DISABLED", "SEMIAN_BULKHEAD_DISABLED", "SEMIAN_CIRCUIT_BREAKER_DISABLED"] {
        assert!(stdout.contains(&format!("## env {env}")), "missing environment {env}:\n{stdout}");
    }
    // The circuit breakers the port does not model are refused, by name.
    for kind in ["adaptive_circuit_breaker", "dual_circuit_breaker"] {
        assert!(
            stdout.contains(&format!("{kind} !! Semian::UnsupportedCircuitBreakerError")),
            "{kind} is not refused:\n{stdout}"
        );
    }
}

#[test]
#[ignore = "needs semian 0.28.3, activerecord 8.1.4 and ROUNDHOUSE_PG_TEST_URL"]
fn semian_postgresql_adapter_answers_like_the_gem_on_rails() {
    assert!(std::env::var_os("ROUNDHOUSE_PG_TEST_URL").is_some(), "ROUNDHOUSE_PG_TEST_URL is not set");
    let stdout = compare("adapter");
    assert!(stdout.lines().count() > 200, "too few observations:\n{stdout}");
    for observation in [
        "prepended Semian::ActiveRecordPostgreSQLAdapter < Semian::ActiveRecordAdapter",
        "identity => [\"ActiveRecord::ConnectionAdapters::PostgreSQLAdapter\", :postgresql_rhgood",
        "busy statement !! ActiveRecord::ConnectionAdapters::PostgreSQLAdapter::ResourceBusyError",
        "down 2 !! ActiveRecord::ConnectionAdapters::PostgreSQLAdapter::CircuitOpenError: [postgresql_rhdown] Semian::OpenCircuitError caused by",
        "reconnects inside the statement => :ok",
        "recovered circuit => [true, false]",
        // database.yml session settings and pool size, as Rails applies them.
        "entry configured pool => 2",
        "entry configured client_encoding => \"LATIN1\"",
        "entry configured statement_timeout => \"5s\"",
        "entry configured IntervalStyle => \"iso_8601\"",
        "entry zoned pool => 3",
        "entry zoned TimeZone => \"America/New_York\"",
        "entry defaults pool => 5",
        "entry ambiguous !! RuntimeError: Ambiguous configuration",
    ] {
        assert!(stdout.contains(observation), "missing {observation:?}:\n{stdout}");
    }
}
