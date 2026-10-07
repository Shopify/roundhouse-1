//! Emitted-program regression: `k = Class.new(Base); k.extend(Mod)`
//! restructures a CLASS, which every Ruby-family tree runs, not a live
//! object's method table.
//!
//! Core's `Checkouts::Core::Facade.define` (run at load by the
//! constants of `checkouts/core.rb`) builds each facade as
//! `Class.new(Facade) #: singleton(Facade)` and then
//! `new_facade.extend(Forwardable)`. The Object#extend stub turned the
//! call into a raise, so the boot failed with "Object#extend not
//! supported". The expected output is what CRuby prints for the same
//! source with `forwardable` loaded (bin/probe-check, RUBYOPT=-rforwardable).

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const FACADE_PROBE: &str = r##"module FacadeProbe
  class Facade
    class << self
      def define(allowed_methods:)
        new_facade = Class.new(Facade) #: singleton(Facade)
        new_facade.extend(Forwardable)
        new_facade.def_delegators(:@target_object, *allowed_methods)
        new_facade
      end
    end

    def initialize(target_object)
      @target_object = target_object
    end

    def inspect
      "<#{self.class}: #{@target_object.inspect}>"
    end
  end

  StringFacade = Facade.define(allowed_methods: [:upcase, :size])

  def self.report
    f = StringFacade.new("abc")
    [f.upcase, f.size, f.respond_to?(:downcase), StringFacade.superclass]
  end
end
"##;

#[test]
fn extending_a_class_new_value_runs_on_the_ruby_tree() {
    let run = emit_and_run::real_blog()
        .write("app/models/facade_probe.rb", FACADE_PROBE)
        .run_ruby("p FacadeProbe.report\n");
    run.assert_passes();
    assert_eq!(run.stdout.trim_end(), r#"["ABC", 3, false, FacadeProbe::Facade]"#);
    let facade = std::fs::read_to_string(run.emitted.join("app/models/facade_probe/facade.rb"))
        .expect("emitted facade.rb");
    assert!(facade.contains("new_facade.extend(Forwardable)"), "extend was stubbed:\n{facade}");
    assert!(facade.contains("require \"forwardable\""), "the stdlib module is not required:\n{facade}");
}

/// The instance form stays the stub: a local that is ever assigned
/// anything but `Class.new` / `Module.new` proves nothing.
#[test]
fn extending_a_local_that_may_hold_an_object_is_still_stubbed() {
    let (emitted, _errors) = emit_and_run::real_blog()
        .write(
            "app/models/extend_probe.rb",
            "module ExtendProbe\n  def self.build(flag)\n    k = Class.new\n    k = Object.new if flag\n    k.extend(Comparable)\n    k\n  end\nend\n",
        )
        .emit(roundhouse::project::BuildTarget::Ruby);
    let src = std::fs::read_to_string(emitted.join("app/models/extend_probe.rb")).expect("emitted");
    assert!(src.contains("Object#extend not supported"), "instance extend not stubbed:\n{src}");
}
