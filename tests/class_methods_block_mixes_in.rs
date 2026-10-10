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

/// fixture_factory 0.3.0's `FixtureFactory::Registry` spelling: a Concern
/// whose class side is a `class_methods do` block, an `included do`
/// `class_attribute`, an includer calling a block method from its class
/// body, and a helper module naming the carrier itself (Core's
/// `Returns::ReturnTestHelper`: `extend FixtureFactory::Registry::ClassMethods`).
/// Natively (ActiveSupport 8) this prints `{a: "a"}`, `true`, `true`,
/// `false`, `true`.
const REGISTRY: &str = r##"module Registry
  extend ActiveSupport::Concern

  included do
    class_attribute(:defs)
  end

  class_methods do
    def define_defs(&block)
      self.defs = {}
      instance_exec(&block) if block
    end

    def factory(name)
      defs[name] = name.to_s
    end

    def from_carrier?
      true
    end
  end
end
"##;

#[test]
fn a_class_methods_block_is_the_concerns_class_methods_module() {
    let overlay = || {
        emit_and_run::empty_app()
            .write("app/models/concerns/registry.rb", REGISTRY)
            .write("app/models/fixture_helper.rb", "module FixtureHelper\n  extend Registry::ClassMethods\nend\n")
            .write("app/models/gadget.rb", "class Gadget\n  include Registry\n\n  define_defs do\n    factory(:a)\n  end\nend\n")
    };
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:?}");
    let registry = std::fs::read_to_string(emitted.join("app/models/registry.rb")).unwrap();
    assert!(registry.contains("module ClassMethods\n    def define_defs(&block)"), "{registry}");
    assert!(!registry.contains("def self.factory"), "{registry}");
    let gadget = std::fs::read_to_string(emitted.join("app/models/gadget.rb")).unwrap();
    assert!(gadget.contains("extend Registry::ClassMethods") && !gadget.contains("def self.factory"), "{gadget}");
    let run = overlay().run_ruby(
        "p Gadget.defs\np FixtureHelper.from_carrier?\np Gadget.singleton_class.include?(Registry::ClassMethods)\np Registry.respond_to?(:factory)\np Gadget.respond_to?(:from_carrier?)\n",
    );
    run.assert_passes();
    assert_eq!(run.stdout.trim(), "{a: \"a\"}\ntrue\ntrue\nfalse\ntrue", "{}", run.stderr);
}
