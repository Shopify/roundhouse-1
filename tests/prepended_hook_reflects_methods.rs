//! A `prepend` whose module's `prepended(base)` hook lists the base's
//! methods runs after the class body's `def`s, as it does where the
//! source writes the `prepend` last. Core's `Apps::Management::ApiEnforcement`
//! checks `declared - base.public_instance_methods(false)` and raised
//! "declared but not defined" when the emit wrote the `prepend` above
//! the methods.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const ENFORCEMENT: &str = r#"module Guarded
  class MisconfiguredError < StandardError; end

  class << self
    def prepended(base)
      declared = base.declared_methods
      missing = declared - base.public_instance_methods(false)
      raise MisconfiguredError, "declared but not defined: #{missing.join(", ")}" unless missing.empty?
    end
  end
end

module Declaring
  def declare(name)
    declared_methods << name
  end

  def declared_methods
    @declared_methods ||= []
  end
end

class Api
  extend Declaring

  declare :count
  declare :list

  def count(app_id:)
    app_id
  end

  def list
    []
  end

  prepend Guarded
end
"#;

const SCRIPT: &str = r#"
raise "prepend ran" unless Api.ancestors.first == Guarded
raise "defined" unless Api.new.count(app_id: 3) == 3
puts "prepended hook passed"
"#;

#[test]
fn a_prepended_hook_that_lists_the_bases_methods_runs_after_them() {
    emit_and_run::real_blog()
        .write("app/models/guarded.rb", ENFORCEMENT)
        .run_ruby(SCRIPT)
        .assert_passes();
}
