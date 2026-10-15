//! A controller Concern with a nested `module ClassMethods`, as Core's
//! `ClickjackingProtection` writes it, BOOTS and its class macros run on
//! the includer:
//!
//! ```ruby
//! module GuardedActions
//!   extend ActiveSupport::Concern
//!
//!   included do
//!     class_attribute :guarded_rules, instance_writer: false, instance_predicate: false
//!     private :guarded_rules
//!     self.guarded_rules = []
//!     guard_all
//!   end
//!
//!   module ClassMethods
//!     def guard_all(**options) = self.guarded_rules += [GuardRule.new(:all, **options)]
//!   end
//!
//!   class GuardRule; end
//! end
//! ```
//!
//! The controller's lowered class writes `include GuardedActions` and
//! `extend GuardedActions::ClassMethods`, and boot loads
//! `config/routes.rb` (so the controller) before `app/models.rb` registers
//! the autoloads that would otherwise load the carrier's own file. Only the
//! emitted text was checked before (tests/controller_concern_private_class_attribute.rs),
//! so a `NameError: uninitialized constant GuardedActions::ClassMethods` at
//! boot went unseen (e2e v2-r31: every id).

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const CONCERN: &str = r#"module GuardedActions
  extend ActiveSupport::Concern

  GUARD_ALL = :all
  GUARD_NONE = :none

  included do
    class_attribute :guarded_rules, instance_writer: false, instance_predicate: false
    private :guarded_rules
    self.guarded_rules = []

    guard_all
  end

  private

  def guarded_summary
    guarded_rules.map { |rule| rule.rule.to_s }.join(",")
  end

  module ClassMethods
    def guard_all(**options)
      self.guarded_rules += [GuardRule.new(GUARD_ALL, **options)]
    end

    def guard_none(**options)
      self.guarded_rules += [GuardRule.new(GUARD_NONE, **options)]
    end
  end

  class GuardRule
    attr_reader :rule

    def initialize(rule, **options)
      @rule = rule
      @options = options
    end
  end
end
"#;

const APPLICATION_CONTROLLER: &str =
    "class ApplicationController < ActionController::Base\n  include GuardedActions\nend\n";

const WIDGETS_CONTROLLER: &str = r#"class WidgetsController < ApplicationController
  def index
    head :ok
  end
end
"#;

/// Core's `config/routes.rb` draws nothing, and the tree's carries the
/// application controller's `require_relative`: `boot.rb` loads the
/// controller there, ahead of `app/models.rb`'s autoloads.
const ROUTES: &str = "# no routes are drawn from the main application\n";

fn overlay() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "database_adapter: postgresql\n")
        .write("app/controllers/concerns/guarded_actions.rb", CONCERN)
        .write("app/controllers/application_controller.rb", APPLICATION_CONTROLLER)
        .write("app/controllers/widgets_controller.rb", WIDGETS_CONTROLLER)
        .write("config/routes.rb", ROUTES)
        .write("db/structure.sql", "CREATE TABLE widgets (\n    id bigserial NOT NULL,\n    CONSTRAINT widgets_pkey PRIMARY KEY (id)\n);\n")
}

/// Boot the tree the way the test runner does (`boot.rb`, which loads
/// `config/routes.rb` ahead of the models aggregator), and ask the
/// controllers what the class macros left behind.
#[test]
#[ignore = "requires the pg gem from Core's gem home (CORE_GEM_HOME); boot only loads it, no server is contacted"]
fn a_controller_concern_class_methods_module_boots_and_its_macros_run() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    let real: Vec<&String> = errors
        .iter()
        .filter(|e| !e.contains("database_adapter not supported"))
        .collect();
    assert!(real.is_empty(), "{}", errors.join("\n"));
    let script = r#"
require File.expand_path("boot", Dir.pwd)
rules = ->(klass) { klass.guarded_rules.map { |r| r.rule } }
puts "application: #{rules.call(ApplicationController).inspect}"
puts "widgets: #{rules.call(WidgetsController).inspect}"
ApplicationController.guard_none
puts "after guard_none: #{rules.call(ApplicationController).inspect}"
puts "private: #{ApplicationController.new.respond_to?(:guarded_rules)}"
puts "class: #{GuardedActions::GuardRule.name}"
"#;
    // The pg gem `runtime/db.rb` requires comes from Core's gem home, as
    // tests/support/pg_probe.rs loads it.
    let gem_home = std::env::var("CORE_GEM_HOME").unwrap_or_else(|_| {
        "/nix/store/d958gj4a8pmld3fxa7fhyqasz61cafch-ruby-gems-dev/lib/ruby/gems/4.0.0".into()
    });
    let output = emit_and_run::ruby()
        .env("GEM_HOME", &gem_home)
        .env("GEM_PATH", &gem_home)
        .arg("-e")
        .arg(script)
        .current_dir(&emitted)
        .output()
        .expect("spawn ruby");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "boot failed:\n{stderr}\n{stdout}");
    assert_eq!(
        stdout.lines().collect::<Vec<_>>(),
        [
            "application: [:all]",
            "widgets: [:all]",
            "after guard_none: [:all, :none]",
            "private: false",
            "class: GuardedActions::GuardRule",
        ],
        "{stderr}"
    );
}
