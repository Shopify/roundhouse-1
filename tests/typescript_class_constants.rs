//! #1223: the TypeScript emit declares a class's own constants. A
//! literal value (`nil`, a Boolean, number, String, Symbol or regexp, or
//! an Array or Hash of those, `.freeze`d or not) becomes a `static`
//! member, so a `Klass.NAME` read resolves; before, nothing declared it,
//! `tsc` rejected every read (TS2339) and it was `undefined` at run
//! time. The run itself is
//! `typescript_toolchain::real_blog_class_constants_run_under_tsx`.

#[path = "support/emit_and_run.rs"]
#[allow(dead_code)]
mod emit_and_run;
#[path = "support/class_constants.rs"]
mod class_constants;

use roundhouse::project::BuildTarget;

#[test]
fn literal_constants_are_declared_as_static_members() {
    let (emitted, errors) = class_constants::overlay().emit(BuildTarget::Typescript);
    assert!(errors.is_empty(), "{errors:?}");
    let scale = std::fs::read_to_string(emitted.join("app/models/scale.ts")).expect("scale.ts");
    for decl in [
        "static WEIGHTS = { \"small\": 1, \"large\": 9 };",
        "static NAMES = [\"small\", \"large\"];",
        "static LABEL = \"scale\";",
        "static LIMIT = 9;",
        "static RATIO = 1.5;",
        "static FLAGS = [true, false, null];",
        "static NESTED = { \"a\": [1, 2], \"b\": { \"c\": \"d\" } };",
    ] {
        assert!(scale.contains(decl), "{decl}:\n{scale}");
    }
}

/// A value that is not a literal (a constant read, a call) is not
/// declared, nor is a constant whose name a method also takes.
#[test]
fn only_literal_unclaimed_constants_are_declared() {
    let (emitted, errors) = emit_and_run::real_blog()
        .write(
            "app/models/gauge.rb",
            "class Gauge\n  WEIGHTS = { \"a\" => 1 }.freeze\n  ALIAS = WEIGHTS\n  STARTED = Time.now\n  LEVEL = 3\n\n  def self.LEVEL\n    4\n  end\n\n  def self.size\n    WEIGHTS.size\n  end\nend\n",
        )
        .emit(BuildTarget::Typescript);
    assert!(errors.is_empty(), "{errors:?}");
    let gauge = std::fs::read_to_string(emitted.join("app/models/gauge.ts")).expect("gauge.ts");
    assert!(gauge.contains("static WEIGHTS = { \"a\": 1 };"), "{gauge}");
    for absent in ["static ALIAS", "static STARTED", "static LEVEL ="] {
        assert!(!gauge.contains(absent), "{absent}:\n{gauge}");
    }
}
