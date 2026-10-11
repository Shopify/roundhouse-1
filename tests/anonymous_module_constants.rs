//! A class-body constant assigned `Module.new do .. end` or
//! `Class.new(Parent) { .. }` is a constant of that class, so a later
//! statement of the body, a block inside it and a method read it by
//! name (Ruby's lexical constant lookup, `Module.nesting`). The value
//! is the module or class `Module#initialize` / `Class#initialize`
//! create when the body runs, so the read is gradual, not a gap.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const PROBE: &str = r##"module AnonProbe
  class Parser
    ALLOWED = %w[a b].freeze

    FILTERS = Module.new do
      def a = "a"
      def b = "b"
    end

    STRAINER = Class.new(Object) { include FILTERS }

    BUILT = [1].map do |_x| STRAINER end

    def self.invokable?(name)
      STRAINER.method_defined?(name)
    end
  end

  def self.report
    [Parser.invokable?(:a), Parser.invokable?(:z), Parser::BUILT.first == Parser::STRAINER]
  end
end
"##;

#[test]
fn class_body_constants_from_anonymous_modules_and_classes_resolve() {
    let run = emit_and_run::real_blog()
        .write("app/models/anon_probe.rb", PROBE)
        .run_ruby("p AnonProbe.report\n");
    run.assert_passes();
    assert_eq!(run.stdout.trim_end(), "[true, false, true]");
    let src = std::fs::read_to_string(run.emitted.join("app/models/anon_probe/parser.rb")).expect("emitted");
    assert!(!src.contains("constant not supported"), "unresolved constant:\n{src}");
}

/// A module of constants an initializer defines, read through its full
/// path from the body of an `app/` module (Rails loads initializers
/// before the app's code): the emitted program defines it where the
/// initializer stands, so the read is a real reference.
#[test]
fn an_initializer_defined_module_is_read_from_a_module_body() {
    let run = emit_and_run::real_blog()
        .write(
            "config/initializers/zz_pods.rb",
            "module PodsProbe\n  module Settings\n    module Enum\n      LEADER = \"0\"\n      OTHER = \"1\"\n    end\n  end\nend\n",
        )
        .write(
            "app/models/pods_config.rb",
            "module PodsConfig\n  TARGETS = [\n    PodsProbe::Settings::Enum::LEADER,\n    PodsProbe::Settings::Enum::OTHER,\n  ].freeze\nend\n",
        )
        .run_ruby("p PodsConfig::TARGETS\n");
    run.assert_passes();
    assert_eq!(run.stdout.trim_end(), r#"["0", "1"]"#);
}
