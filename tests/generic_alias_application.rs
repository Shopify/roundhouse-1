//! `Alias[X]` where `Alias = ::Generic::Module` and the module `extend`s
//! `T::Generic`. sorbet-runtime's `T::Generic#[]` answers the module
//! itself (sorbet-runtime generic.rb), so a constant that names the
//! generic applies like the generic. The emitter erases the `extend
//! T::Generic` (src/ingest/sorbet_check.rs, erase_generic_applications),
//! so the application has to be erased with it through the alias too:
//! a gem's `Optional[Context]` reached through an app constant raised
//! "undefined method '[]' for module" at the first call.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const GENERIC: &str = r#"module Adt
  module Optional
    extend(T::Generic)

    ValueType = type_member(:out)

    def self.none
      :none
    end
  end
end
"#;

/// A constant naming the generic by a rooted path, and another by a
/// relative one through the first.
const ALIASES: &str = r#"module Optionals
  Optional = ::Adt::Optional
  Again = Optional
end
"#;

/// The class-body DSL the macro below calls.
const PROPERTIES: &str = r#"module Properties
  def property(name, accepts: nil, default: nil)
    (@properties ||= {})[name] = [accepts, default]
  end

  def properties
    @properties
  end
end
"#;

const USER: &str = r#"class Installer
  ACCEPTS = ->(value) { value == Optionals::Optional[String].none }
  ALSO = ->(value) { value == Optionals::Again[Integer].none }
  ROOTED = ->(value) { value == ::Optionals::Optional[Symbol].none }

  extend Properties

  # A class-body macro whose lambdas apply the alias (an `accepts:` check).
  property :context, accepts: ->(context) { context.is_a?(Optionals::Optional[String]) }, default: -> { Optionals::Optional.none }

  def self.report
    accepts, default = properties.fetch(:context)
    [ACCEPTS.call(:none), ALSO.call(:none), ROOTED.call(:none), Optionals::Optional[String].equal?(Adt::Optional), accepts.call(1), default.call]
  end
end
"#;

#[test]
fn a_constant_naming_a_generic_module_applies_like_the_module() {
    let run = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("app/models/adt/optional.rb", GENERIC)
        .write("app/models/optionals.rb", ALIASES)
        .write("app/models/properties.rb", PROPERTIES)
        .write("app/models/installer.rb", USER)
        .run_ruby("puts Installer.report.inspect\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout.trim(), "[true, true, true, true, false, :none]", "{}", run.stderr);
}
