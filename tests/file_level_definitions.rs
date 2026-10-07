//! Support-file definitions at FILE level load as Ruby loads them, and
//! the ones the ingest cannot carry that way are named.
//!
//! Core's `minitest/type_exfiltrator_plugin/wrappers/test_wrapper.rb`
//! writes `IGNORE_TEST_INSTANCE_METHODS` above `module Minitest`; the
//! constant kept the file out of autoload, so the emitted boot loaded a
//! file native's lazy boot never loads. Core's `strict_warnings.rb`
//! defines the test-env `StrictWarning` inside a file-level `if`, which
//! the ingest does not reach, and `minitest/type_exfiltrator.rb` defines
//! a silenced `StrictWarning` after `return unless
//! ENV["EXFILTRATE_TYPES"]`, which the ingest carried unconditionally.
//! Both were silent.
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
fn a_conditional_definition_and_definitions_behind_a_file_level_return_are_named() {
    let (_, errors) = emit_and_run::empty_app()
        .write(
            "lib/strict_warnings.rb",
            "if ENV[\"RH_NEVER_SET\"] == \"1\"\n  module StrictWarning\n    def self.offense!(msg)\n      raise msg\n    end\n  end\nelse\n  module StrictWarning\n  end\nend\n",
        )
        .write(
            "lib/guarded.rb",
            "flag = ENV[\"RH_NEVER_SET\"]\nreturn unless flag\n\nLIMIT = 1\n\nmodule Guarded\nend\n",
        )
        .write("lib/plain.rb", "module Plain\n  A = 1\nend\n")
        .emit(roundhouse::project::BuildTarget::Ruby);
    assert!(
        errors.iter().any(|e| e.contains("file_level_definition not supported")
            && e.contains("`module StrictWarning` defined inside a file-level conditional")),
        "{errors:#?}"
    );
    assert!(
        errors.iter().any(|e| e.contains("file_level_definition not supported")
            && e.contains("file-level `return`")
            && e.contains("(`LIMIT`, `module Guarded`)")),
        "{errors:#?}"
    );
    assert!(!errors.iter().any(|e| e.contains("plain.rb")), "{errors:#?}");
}
