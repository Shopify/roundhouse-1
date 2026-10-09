//! ActiveSupport::Duration's class predicates (`is_a?`, `kind_of?`,
//! `instance_of?`, `Duration.===`) and sorbet's inline `T.let` over
//! them, on both runtime ports against the Rails native Core runs with
//! Core's sorbet-runtime: tests/duration_type_predicates_runtime.rb
//! prints the same observations natively and on each port and requires
//! them equal. Core's ShopifyPayments::Capability loads
//! `CACHE_TTL = T.let(10.minutes, Integer)` at boot; without
//! activesupport's `is_a?` it raised "T.let: Expected type Integer, got
//! type ActiveSupport::Duration with value 10 minutes".
use std::path::Path;
use std::process::Command;

#[test]
fn duration_class_predicates_answer_like_activesupport() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new("ruby")
        .arg(root.join("tests/duration_type_predicates_runtime.rb"))
        .arg("compare")
        .current_dir(root)
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("Native ActiveSupport and the Duration ports agree"),
        "Duration class predicates diverged from activesupport\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    // The boot site itself: a Duration passes `T.let(_, Integer)`.
    assert!(
        stdout.contains("T.let(minutes(10), Integer): ActiveSupport::Duration 10 minutes"),
        "T.let(10.minutes, Integer) does not pass:\n{stdout}"
    );
    assert!(stdout.lines().count() > 300, "too few observations:\n{stdout}");
}
