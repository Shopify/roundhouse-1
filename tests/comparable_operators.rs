//! Orderings that are not value comparisons of builtins.
//!
//! A class that includes `Comparable` answers `<`, `<=`, `>`, `>=` from
//! its `<=>` (Core's measure and API-change value classes), and
//! `x.class < Base` is Module#< on the class `x` has at run time (Core's
//! `error.class <= retryable_error_class`). `Gem::Version` includes
//! Comparable in its vendored RBS only (Core's `GraphQL::Cardinal::
//! SUPPORTED`). All were refused as incompatible operands. Native Ruby
//! on the same source is the oracle.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const ORDERS: &str = r#"module Orders
  class Weight
    include Comparable
    attr_reader :n
    def initialize(n) = @n = n
    def <=>(other) = n <=> other.n
  end

  class Base; end
  class Child < Base; end

  MINIMUM = "1.2"
  SUPPORTED = Gem::Version.new("2.0") >= Gem::Version.new(MINIMUM) #: bool

  def self.report
    light = Weight.new(1)
    heavy = Weight.new(2)
    child = Child.new
    base = Base.new
    [heavy > light, heavy < light, light <= light, heavy >= light,
     child.class < Base, base.class < Base, base.class <= Base, child.class <= String,
     Gem::Version.new("1.10") > Gem::Version.new("1.9"), Gem::Version.new(MINIMUM) >= Gem::Version.new("2.0")]
  end
end
"#;

const REPORT: &str = "p Orders.report\np Orders::SUPPORTED\n";

#[test]
fn comparable_and_class_orderings_run_as_in_native_ruby() {
    let dir = std::env::temp_dir().join(format!("roundhouse-comparable-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("orders.rb");
    std::fs::write(&file, ORDERS).unwrap();
    let native = std::process::Command::new("ruby")
        .arg("-e")
        .arg(format!("require {:?}\n{REPORT}", file.display().to_string()))
        .output()
        .expect("native Ruby control");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = String::from_utf8_lossy(&native.stdout).into_owned();
    assert_eq!(expected, "[true, false, true, true, true, false, true, nil, true, false]\ntrue\n", "native control");

    let (emitted, errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("lib/orders.rb", ORDERS)
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    let output = emit_and_run::ruby()
        .arg("-e")
        .arg(format!(
            "require \"./app/models/orders\"\nrequire \"./app/models/orders/weight\"\nrequire \"./app/models/orders/base\"\nrequire \"./app/models/orders/child\"\n{REPORT}"
        ))
        .current_dir(&emitted)
        .output()
        .expect("spawn ruby");
    assert!(output.status.success(), "emitted program failed in {}:\n{}", emitted.display(), String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8_lossy(&output.stdout), expected);
}
