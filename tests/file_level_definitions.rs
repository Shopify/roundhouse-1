//! Support-file definitions and statements at FILE level load as Ruby
//! loads the file.
//!
//! Core's `minitest/type_exfiltrator_plugin/wrappers/test_wrapper.rb`
//! writes `IGNORE_TEST_INSTANCE_METHODS` above `module Minitest`; the
//! constant kept the file out of autoload, so the emitted boot loaded a
//! file native's lazy boot never loads. Core's `strict_warnings.rb`
//! defines the test-env `StrictWarning` inside a file-level `if`, and
//! `minitest/type_exfiltrator.rb` defines a silenced `StrictWarning`
//! after `return unless ENV["EXFILTRATE_TYPES"]`; shopify-money runs
//! `Money::Converters.register(…)` after its declarations and defines
//! `Money.deprecate` in a file-level `Money.class_eval do … end`. A file
//! like these is carried as its file program
//! (app/models/roundhouse_files/), which the boot loads: each branch's
//! definitions where Ruby defines them, nothing after a guard that
//! returns, each statement run, and one that fails named.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

#[test]
fn a_file_level_constant_loads_with_its_file_on_first_reference() {
    let run = emit_and_run::empty_app()
        // Lazy, as Core's local test boot is (`run_ruby` boots development).
        .write("config/environments/development.rb", "Rails.application.configure do\n  config.eager_load = false\nend\n")
        .write(
            "lib/minitest/wrappers/test_wrapper.rb",
            "IGNORE_TEST_INSTANCE_METHODS = %i[\n  setup\n  teardown\n].freeze\n\nmodule Minitest\n  module Wrappers\n    class TestWrapper\n      A = 1\n    end\n  end\nend\n",
        )
        .run_ruby("p defined?(IGNORE_TEST_INSTANCE_METHODS)\np Minitest::Wrappers::TestWrapper::A\np IGNORE_TEST_INSTANCE_METHODS\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "nil\n1\n[:setup, :teardown]\n");
}

#[test]
fn file_level_statements_conditional_definitions_and_a_file_level_return_run_as_ruby_loads_the_file() {
    let run = emit_and_run::empty_app()
        // strict_warnings.rb: a definition per branch of a file-level `if`,
        // and statements around it.
        .write(
            "lib/strict_warnings.rb",
            "require \"json\"\n\nif ENV[\"RH_NEVER_SET\"] == \"1\"\n  module StrictWarning\n    def self.mode\n      :raise\n    end\n  end\nelsif ENV[\"RH_NEVER_SET\"].nil?\n  TRACE << \"strict\"\n  module StrictWarning\n    Offense = Class.new(Exception)\n\n    class << self\n      def mode\n        :test\n      end\n    end\n  end\n  StrictWarning.mode\nelse\n  module StrictWarning\n  end\nend\n",
        )
        // minitest/type_exfiltrator.rb: definitions behind `return unless`.
        .write(
            "lib/guarded.rb",
            "flag = ENV[\"RH_NEVER_SET\"]\nreturn unless flag\n\nLIMIT = 1\n\nmodule Guarded\nend\n\nTRACE << \"guarded\"\n",
        )
        // shopify-money's iso4217_converter.rb: a statement after the
        // file's declarations.
        .write(
            "lib/registry.rb",
            "module Registry\n  def self.all\n    @all ||= {}\n  end\n\n  def self.register(key, klass)\n    all[key] = klass\n  end\nend\n",
        )
        .write(
            "lib/registry/iso.rb",
            "require_relative \"../registry\"\nrequire_relative \"errors\" if defined?(Registry)\n\nmodule Registry\n  class Iso\n    def code\n      \"ISO\"\n    end\n  end\nend\n\nRegistry.register(:iso, Registry::Iso)\n$rh_flag = Registry::Error.superclass\nrequire_relative \"extras\" if defined?(Registry)\nrequire_relative \"never\" if TRACE.include?(\"nope\")\n",
        )
        // shopify-money's money_column.rb: requires only, loaded where
        // a file requires it.
        .write("lib/registry/extras.rb", "require_relative \"extra_one\"\n")
        .write("lib/registry/extra_one.rb", "TRACE << \"extra_one\"\n")
        // Required only where a condition Ruby finds false: never loaded.
        .write("lib/registry/never.rb", "TRACE << \"never\"\n")
        // shopify-money's errors.rb: empty classes only.
        .write("lib/registry/errors.rb", "module Registry\n  class Error < StandardError\n  end\nend\n")
        // A `return` in a block the file runs ends its load too.
        .write("lib/each_guard.rb", "[1].each { |x| return if x == 1 }\n\nmodule EachGuarded\nend\n")
        // shopify-money's deprecations.rb: a statement-only file.
        .write(
            "lib/registry/deprecations.rb",
            "Registry.class_eval do\n  REGISTRY_DEFINED = defined?(Registry)\n\n  def self.deprecator\n    @deprecator ||= \"dep\"\n  end\n\n  if Thread.respond_to?(:each_caller_location)\n    def self.deprecated?\n      REGISTRY_DEFINED == \"constant\" && deprecator == \"dep\"\n    end\n  else\n    def self.deprecated?\n      false\n    end\n  end\nend\n",
        )
        .write("lib/broken.rb", "Registry.no_such_method(1)\n\nmodule AfterBroken\nend\n")
        // A condition that raises (strict_warnings.rb reads BootLib,
        // which config/boot.rb defines): neither branch runs.
        .write("lib/raising_condition.rb", "if NoSuchBootLib::RAILS_ENV == \"test\"\n  module RaisingThen\n  end\nelse\n  module RaisingElse\n  end\nend\nTRACE << \"after-condition\" if false\n")
        .write("lib/plain.rb", "module Plain\n  A = 1\nend\n")
        .write("lib/trace.rb", "TRACE = []\n")
        .run_ruby(
            "p StrictWarning.mode\n\
             p StrictWarning::Offense.superclass\n\
             p defined?(Guarded)\n\
             p defined?(LIMIT)\n\
             p Registry.all[:iso].new.code\n\
             p Registry.deprecated?\n\
             p Object.const_defined?(:REGISTRY_DEFINED, false)\n\
             p defined?(AfterBroken)\n\
             p Plain::A\n\
             p $rh_flag\n\
             p defined?(EachGuarded)\n\
             p [defined?(RaisingThen), defined?(RaisingElse)]\n\
             p TRACE\n",
        );
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(
        run.stdout,
        ":test\nException\nnil\nnil\n\"ISO\"\ntrue\ntrue\n\"constant\"\n1\nStandardError\nnil\n[nil, nil]\n[\"extra_one\", \"strict\"]\n",
        "{}",
        run.stderr
    );
    assert!(
        run.stderr.contains("roundhouse: error[file_statement_not_run]: the file-level statement at lib/broken.rb:1 did not run: NoMethodError"),
        "{}",
        run.stderr
    );
    for file in ["strict_warnings", "guarded", "registry/iso", "registry/deprecations", "broken", "each_guard"] {
        assert!(
            run.emitted.join(format!("app/models/roundhouse_files/lib/{file}.rb")).exists(),
            "{file} has a file program"
        );
    }
    for file in ["plain", "registry", "registry/errors", "trace"] {
        assert!(
            !run.emitted.join(format!("app/models/roundhouse_files/lib/{file}.rb")).exists(),
            "{file} is declarations only"
        );
    }
    assert!(
        run.stderr.contains("roundhouse: error[file_statement_not_run]: the file-level condition at lib/raising_condition.rb:1 did not run: NameError"),
        "{}",
        run.stderr
    );
    assert!(!run.errors.iter().any(|e| e.contains("file_level")), "{:#?}", run.errors);
    assert!(!run.stderr.contains("not_carried"), "{}", run.stderr);
    let models = std::fs::read_to_string(run.emitted.join("app/models.rb")).expect("app/models.rb");
    // Loaded where they are required, not at boot.
    for file in ["extras", "extra_one", "never"] {
        // A boot require line; the autoload table below names them too, to load them on demand.
        assert!(!models.contains(&format!("  \"models/roundhouse_files/lib/registry/{file}\",\n")), "{models}");
    }
    assert!(models.contains("  \"models/roundhouse_files/lib/registry/iso\",\n"), "{models}");
    let iso = std::fs::read_to_string(run.emitted.join("app/models/roundhouse_files/lib/registry/iso.rb")).expect("iso program");
    assert!(iso.contains("require RoundhouseAutoload.path(\"models/registry/error\") if defined?(Registry)"), "{iso}");
}
