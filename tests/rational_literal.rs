//! A Rational literal (`999999.9r`, `3r`, and `1/3r`, which is `1 / 3r`)
//! lowers to its exact value, `Rational(numerator, denominator)`.
//!
//! Ingest refused `RationalNode`, so a Core file holding a Rational
//! constant was not emitted at all. Lowering through Float would round
//! the value, so ingest builds the Kernel `Rational` call from Prism's
//! numerator and denominator. Native Ruby on the same source is the oracle.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const RAT: &str = r#"module Ratlab
  class Limits
    MAX = 999999.9r
    WHOLE = 3r
    BIG = 123456789012345678901234567890.5r

    def self.third
      1/3r
    end

    def self.report
      [
        MAX, MAX.class, MAX * 10, MAX.to_s,
        WHOLE, WHOLE.denominator,
        third, third + third + third, third.class,
        BIG.numerator, BIG.denominator,
        -2.5r, 0.1r + 0.2r == 0.3r,
      ]
    end
  end
end
"#;

const REPORT: &str = "p Ratlab::Limits.report\n";

#[test]
fn rational_literals_keep_their_exact_value_like_native() {
    let dir = std::env::temp_dir().join(format!("roundhouse-rational-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let rat = dir.join("limits.rb");
    std::fs::write(&rat, RAT).unwrap();
    let native = std::process::Command::new("ruby")
        .arg("-e")
        .arg(format!("require {:?}\n{REPORT}", rat.display().to_string()))
        .output()
        .expect("native Ruby control");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        native.status.success(),
        "{}",
        String::from_utf8_lossy(&native.stderr)
    );
    let expected = String::from_utf8_lossy(&native.stdout).into_owned();
    assert_eq!(
        expected,
        "[(9999999/10), Rational, (9999999/1), \"9999999/10\", (3/1), 1, (1/3), (1/1), Rational, \
         246913578024691357802469135781, 2, (-5/2), true]\n",
        "native control"
    );

    let (emitted, errors) = emit_and_run::real_blog()
        .write("app/lib/ratlab/limits.rb", RAT)
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    let source = std::fs::read_to_string(emitted.join("app/models/ratlab/limits.rb"))
        .expect("emitted limits");
    for lowered in [
        "MAX = Rational(9999999, 10)",
        "1 / Rational(3, 1)",
        "BIG = Rational(1234567890123456789012345678905, 10)",
    ] {
        assert!(
            source.contains(lowered),
            "missing `{lowered}` in:\n{source}"
        );
    }

    let main = emitted.join("main").display().to_string();
    let output = emit_and_run::ruby()
        .arg("-e")
        .arg(format!(
            "require {main:?}\nMain.configure_default_adapter!\nrequire_relative \"app/models/ratlab/limits\"\n{REPORT}"
        ))
        .current_dir(&emitted)
        .env("BLOG_DB", ":memory:")
        .output()
        .expect("spawn ruby");
    assert!(
        output.status.success(),
        "emitted program failed in {}:\n{}",
        emitted.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), expected);
}
