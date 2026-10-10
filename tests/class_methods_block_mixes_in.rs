//! `mixes_in_class_methods(ClassMethods)` in a Concern whose class side
//! is a `class_methods do … end` block, Core's
//! `MaintenanceTasks::Task::DangerouslySkipWriteThrottle`
//! (components/platform/essentials/app/models/maintenance_tasks/task/):
//!
//! ```ruby
//! module DangerouslySkipWriteThrottle
//!   extend ActiveSupport::Concern
//!   extend T::Helpers
//!   def dangerously_skip_write_throttle? = ...
//!   class_methods do
//!     def dangerously_skip_write_throttle = (@dangerously_skip_write_throttle = true)
//!   end
//!   mixes_in_class_methods(ClassMethods)
//! end
//! ```
//!
//! `class_methods` defines the `ClassMethods` the call names. The emitted
//! Concern carries the block's defs as class methods the concern splice
//! copies onto includers and emits no `ClassMethods`, so the module's file
//! raised `uninitialized constant ...::ClassMethods` at load (5 unit-1000
//! ids), and its next require warned "already initialized constant" for
//! every constant written before it (7 more, under Core's StrictWarning).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const CONCERN: &str = r##"# typed: strong
module SkipThrottle
  extend ActiveSupport::Concern
  extend T::Helpers

  MARKER = "__marker__"

  #: -> bool
  def skip?
    value = self.class.instance_variable_get(:@skip) #: bool?
    value || false
  end

  class_methods do
    #: -> void
    def skip_throttle
      @skip = true #: bool?
    end
  end
  mixes_in_class_methods(ClassMethods)
end
"##;

const WIDGET: &str = r##"class Widget
  include SkipThrottle
end
"##;

#[test]
fn a_class_methods_block_concern_naming_its_class_methods_loads_and_answers() {
    let run = emit_and_run::empty_app()
        .write("app/models/concerns/skip_throttle.rb", CONCERN)
        .write("app/models/widget.rb", WIDGET)
        .run_ruby("p Widget.new.skip?\nWidget.skip_throttle\np Widget.new.skip?\np SkipThrottle::MARKER\n");
    run.assert_passes();
    assert_eq!(run.stdout.trim(), "false\ntrue\n\"__marker__\"", "{}", run.stderr);
    assert!(!run.stderr.contains("already initialized constant"), "{}", run.stderr);
}

#[test]
fn an_explicit_class_methods_module_keeps_the_call() {
    let concern = CONCERN.replace(
        "  class_methods do\n    #: -> void\n    def skip_throttle\n      @skip = true #: bool?\n    end\n  end\n",
        "  module ClassMethods\n    #: -> void\n    def skip_throttle\n      @skip = true #: bool?\n    end\n  end\n",
    );
    let (emitted, errors) = emit_and_run::empty_app()
        .write("app/models/concerns/skip_throttle.rb", &concern)
        .write("app/models/widget.rb", WIDGET)
        .emit(roundhouse::project::BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:?}");
    let text = std::fs::read_to_string(emitted.join("app/models/skip_throttle.rb")).unwrap();
    assert!(text.contains("SorbetRuntimeCheck.mixes_in_class_methods(self, ClassMethods)"), "{text}");
}
