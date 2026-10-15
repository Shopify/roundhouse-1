//! A Concern whose `included do` declares a `class_attribute` and whose
//! `class_methods do` reads it (Core's `RequestStatsMiddlewareOptions`) is
//! carried: its class methods are each includer's own and the module the
//! block would build is not written. The controller must not `extend` it.
//!
//! ```ruby
//! module Stats
//!   extend ActiveSupport::Concern
//!   included { class_attribute :options, default: Options.new }
//!   class_methods { def enable_stats(only: nil) = self.options = Options.new(only) }
//! end
//! ```
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const APPLICATION_CONTROLLER: &str = "class ApplicationController < ActionController::Base\nend\n";

const GATEWAY: &str = r#"class GatewayController < ApplicationController
  include Stats
  enable_stats(only: [:index])

  def index = head(:ok)
end
"#;

const STATS: &str = r#"module Stats
  extend ActiveSupport::Concern

  class Options
    def initialize(only: nil)
      @only = only
    end
  end

  included do
    class_attribute :stats_options, instance_accessor: false, default: Options.new
  end

  class_methods do
    def enable_stats(only: nil)
      self.stats_options = Options.new(only: only)
    end
  end
end
"#;

#[test]
fn a_carried_concern_with_a_class_methods_block_is_not_extended_by_its_includer() {
    let app = emit_and_run::empty_app()
        .write("roundhouse.yml", "database_adapter: postgresql\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\n  get \"gw\", to: \"gateway#index\"\nend\n")
        .write("app/controllers/application_controller.rb", APPLICATION_CONTROLLER)
        .write("app/controllers/gateway_controller.rb", GATEWAY)
        .write("app/controllers/concerns/stats.rb", STATS);
    let (emitted, _errors) = app.emit(roundhouse::project::BuildTarget::Ruby);
    let read = |rel: &str| std::fs::read_to_string(emitted.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
    let gateway = read("app/controllers/gateway_controller.rb");
    assert!(gateway.contains("def self.enable_stats("), "the macro is the controller's own:\n{gateway}");
    assert!(gateway.contains("enable_stats(only: [:index])"), "{gateway}");
    // Nothing writes `Stats::ClassMethods`, so nothing may extend it.
    let module = read("app/models/stats.rb");
    assert!(!module.contains("ClassMethods"), "{module}");
    assert!(!gateway.contains("Stats::ClassMethods"), "extend of an unwritten module:\n{gateway}");
}
