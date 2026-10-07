// useragent 0.16.11's UserAgent::Version (runtime/spinel/user_agent_version.rb,
// loaded over the port runtime/ruby/user_agent.rb as the CRuby PostgreSQL
// tree loads it) against the installed gem from Core's gem home:
// tests/user_agent_version.rb runs the same observations natively and on
// the port and requires every observation equal.

use std::path::Path;
use std::process::Command;

#[test]
fn version_answers_like_the_useragent_gem() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new("ruby")
        .arg(root.join("tests/user_agent_version.rb"))
        .arg("compare")
        .current_dir(root)
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("Native useragent and the ported UserAgent::Version agree"),
        "UserAgent::Version diverged from useragent\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    // Not a vacuous agreement: every pair saw every operator, and the
    // gem's edges (tag below number, six-segment cap, eql? by string,
    // non-comparable versions, the ArgumentError) are among them.
    assert!(stdout.lines().count() > 5_000, "too few observations: {}", stdout.lines().count());
    for seen in [
        r#""10" vs "9": [1, false, false, false, false, true, true, false]"#,
        r#""4.0b2" vs "4.0": [-1, false, false, true, true, false, false, false]"#,
        r#""1.2.3.4.5.6.7" vs "1.2.3.4.5.6.8": [0, false, false, false, true, false, true, true]"#,
        r#""Unknown" vs "1": [-1, false, false, true, true, false, false, false]"#,
        r#""126.0.6478.126" vs String "46": [1, false, false, false, true]"#,
        "new 1: ArgumentError: invalid value for Version: 1",
        "new(Version) identity: true",
        r#"ancestors: ["UserAgent::Version", "Comparable"]"#,
        r#"secure_headers Version "126.0": [true, true]"#,
    ] {
        assert!(stdout.contains(seen), "missing observation {seen:?}");
    }
}
