//! Module queries on a value's class.
//!
//! `x.class` is a class object, though the analyzer cannot say which
//! one. A pure query (`const_get`, `const_defined?`, `method_defined?`,
//! `instance_methods`) reads the class and changes nothing, so that is
//! enough: Core's `self.class.const_get(:CACHE_KEY_PREFIX) + identifier`
//! picks the subclass's constant at run time. A method that changes the
//! class (`Probe.new.class.define_method`) still needs to know which
//! class, and stays refused (tests/critic_admission.rs). Native Ruby on
//! the same source is the oracle.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const EVENTS: &str = r#"module Events
  class Observed
    PREFIX = "observed:"

    #: (String) -> String
    def key(id) = self.class.const_get(:PREFIX) + id

    #: -> bool
    def flagged? = self.class.const_defined?(:FLAGS)

    #: -> Array[Symbol]
    def own_methods = self.class.instance_methods(false).sort

    def shout = "!"
  end

  class Checkout < Observed
    PREFIX = "checkout:"
    FLAGS = [:a].freeze
  end

  def self.report
    plain = Observed.new
    checkout = Checkout.new
    [plain.key("1"), checkout.key("2"), plain.flagged?, checkout.flagged?,
     checkout.class.method_defined?(:shout), checkout.class.method_defined?(:shout, false),
     plain.own_methods]
  end
end
"#;

const REPORT: &str = "p Events.report\n";

#[test]
fn queries_on_a_values_class_run_as_in_native_ruby() {
    let dir = std::env::temp_dir().join(format!("roundhouse-class-queries-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("events.rb");
    std::fs::write(&file, EVENTS).unwrap();
    let native = std::process::Command::new("ruby")
        .arg("-e")
        .arg(format!("require {:?}\n{REPORT}", file.display().to_string()))
        .output()
        .expect("native Ruby control");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = String::from_utf8_lossy(&native.stdout).into_owned();
    assert_eq!(
        expected,
        "[\"observed:1\", \"checkout:2\", false, true, true, false, [:flagged?, :key, :own_methods, :shout]]\n",
        "native control"
    );

    let (emitted, errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("lib/events.rb", EVENTS)
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    let output = emit_and_run::ruby()
        .arg("-e")
        .arg(format!(
            "require \"./app/models/events\"\nrequire \"./app/models/events/observed\"\nrequire \"./app/models/events/checkout\"\n{REPORT}"
        ))
        .current_dir(&emitted)
        .output()
        .expect("spawn ruby");
    assert!(output.status.success(), "emitted program failed in {}:\n{}", emitted.display(), String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8_lossy(&output.stdout), expected);
}
