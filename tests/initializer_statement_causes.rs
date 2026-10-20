//! Two causes of Core's `initializer_statement_not_run` boot errors, found
//! by classifying the 201 distinct statements of the sweep-1 run:
//!
//! - Core's privacy processors (`Apps::Redaction::ProcessCustomerAccessAssignment`
//!   and 20 others) set their dataset with `public_facing_dataset_name
//!   "third_party_apps"`, a class macro of `DatasetConfiguration`, which
//!   `CustomerProcessor` gives its includers through
//!   `mixes_in_class_methods`. The emit dropped the call (a lower_residue
//!   warning), so each `PrivacyTooling::...::Registry.register_customer_processor`
//!   initializer raised "Processor must define a dataset name" (40 statements).
//! - A module whose body replays its calls and ends `ActiveSupport.
//!   run_load_hooks(:h)` (`GraphApi::Admin::FilterViewModels::FilterInterface`)
//!   fired the hook twice: once where the body has it, once from the line
//!   `project::apply_class_load_hooks` appended. Each `on_load` initializer
//!   registered its filters twice and raised `DuplicateRegistrationError`
//!   (48 statements).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const PROCESSOR: &str = r#"module Processor
  extend T::Helpers

  interface!

  module ClassMethods
    extend T::Helpers

    abstract!

    #: () -> Symbol
    def kind = :processor
  end

  module DatasetConfiguration
    #: (?String? name) -> String?
    def public_facing_dataset_name(name = nil)
      return @public_facing_dataset_name if name.nil?

      @public_facing_dataset_name = name
    end
  end

  mixes_in_class_methods(ClassMethods)
  mixes_in_class_methods(DatasetConfiguration)
end
"#;

const ASSIGNMENT: &str = r#"class Assignment
  include Processor

  public_facing_dataset_name "third_party_apps"
end
"#;

const SUBCLASS: &str = r#"class Reassignment < Assignment
  public_facing_dataset_name "second_party_apps"
end
"#;

#[test]
fn a_class_macro_of_a_mixes_in_class_methods_module_runs_in_the_body() {
    let app = || {
        emit_and_run::empty_app()
            .write("app/models/processor.rb", PROCESSOR)
            .write("app/models/assignment.rb", ASSIGNMENT)
            .write("app/models/reassignment.rb", SUBCLASS)
    };
    let (dir, _) = app().emit(BuildTarget::Ruby);
    let assignment = std::fs::read_to_string(dir.join("app/models/assignment.rb")).expect("emitted assignment.rb");
    assert!(
        assignment.contains("public_facing_dataset_name \"third_party_apps\""),
        "the class macro call is dropped:\n{assignment}"
    );
    let run = app().run_ruby("p Assignment.public_facing_dataset_name\np Reassignment.public_facing_dataset_name\n");
    run.assert_passes();
    assert_eq!(run.stdout, "\"third_party_apps\"\n\"second_party_apps\"\n");
}

/// Not every bare call is a macro: a call no mixed-in module defines
/// stays the dropped call it was (reported), and is not written.
const UNKNOWN_MACRO: &str = r#"class Unmixed
  include Processor

  not_a_macro "x"
end
"#;

#[test]
fn a_call_no_mixed_in_module_defines_is_still_dropped() {
    let (dir, _) = emit_and_run::empty_app()
        .write("app/models/processor.rb", PROCESSOR)
        .write("app/models/unmixed.rb", UNKNOWN_MACRO)
        .emit(BuildTarget::Ruby);
    let unmixed = std::fs::read_to_string(dir.join("app/models/unmixed.rb")).expect("emitted unmixed.rb");
    assert!(!unmixed.contains("not_a_macro"), "an unanswered call is written:\n{unmixed}");
}

/// A class over a base the tree does not model replays its body verbatim,
/// so its `run_load_hooks` call is the body's own.
const HOOKED: &str = r#"class Hooked < Foreign::Base
  value :a

  ActiveSupport.run_load_hooks(:hooked_registry, self)

  value :b
end
"#;

const HOOK_INITIALIZER: &str = r#"ActiveSupport.on_load(:hooked_registry) do
  Registry.register(self)
end
"#;

const REGISTRY: &str = r#"class Registry
  def self.register(klass)
    (@registered ||= []) << klass
  end
end
"#;

#[test]
fn a_replayed_body_fires_its_load_hook_once() {
    let (dir, _) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\ndatabase_adapter: postgresql\n")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_initializer_statement_causes\n")
        .write("app/models/registry.rb", REGISTRY)
        .write("app/models/hooked.rb", HOOKED)
        .write("config/initializers/hooked.rb", HOOK_INITIALIZER)
        .emit(BuildTarget::Ruby);
    let hooked = std::fs::read_to_string(dir.join("app/models/hooked.rb")).expect("emitted hooked.rb");
    assert_eq!(
        hooked.matches("run_load_hooks(:hooked_registry").count(),
        1,
        "the hook fires from the body and again after it:\n{hooked}"
    );
}
