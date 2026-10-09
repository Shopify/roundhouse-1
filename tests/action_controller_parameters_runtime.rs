// ActionController::Parameters (runtime/ruby/action_controller/parameters.rb)
// against actionpack 8.2.0.alpha's strong parameters (Core's pinned
// Shopify/rails 5813559f, tests/support/rails_oracle.rb):
// tests/action_controller_parameters_runtime.rb runs the same observations
// natively and on the port and requires every observation equal, and in
// `bare` mode checks the core contract on the port alone (no
// HashWithIndifferentAccess, as on Spinel).

use std::path::Path;
use std::process::{Command, Output};

fn run(mode: &str) -> Output {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    Command::new("ruby")
        .arg(root.join("tests/action_controller_parameters_runtime.rb"))
        .arg(mode)
        .current_dir(root)
        .output()
        .expect("ruby is on PATH")
}

#[test]
fn parameters_answer_like_actionpack() {
    let out = run("compare");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("Native ActionController::Parameters and the port agree"),
        "ActionController::Parameters diverged from actionpack\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    // Not a vacuous agreement: the reads, the permitted gate and the
    // failure paths ran.
    assert!(stdout.lines().count() > 150, "too few observations: {}", stdout.lines().count());
    for seen in [
        "new(a: 1)[\"a\"]: 1",
        "new(a: 1)[:a]: 1",
        "const_get: true",
        "permitted?: false",
        "permitted? after permit!: true",
        "to_h unpermitted: raised ActionController::UnfilteredParameters: unable to convert unpermitted parameters to hash",
    ] {
        assert!(stdout.contains(seen), "missing observation {seen:?}\n{stdout}");
    }
}

#[test]
fn parameters_load_without_the_core_extensions() {
    let out = run("bare");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("bare: ok"),
        "the bare port failed\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
}
