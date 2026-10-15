//! A controller Concern's `included do` block that makes the reader of its
//! own `class_attribute` private, as Core's `AuthorizationProtection`
//! writes it:
//!
//! ```ruby
//! included do
//!   class_attribute :implicit_api_accessible_actions_list, instance_writer: false, instance_predicate: false
//!   private :implicit_api_accessible_actions_list
//!   self.implicit_api_accessible_actions_list = []
//! end
//! ```
//!
//! Ingest refused the whole file ("visibility declarations inside a
//! `included do … end` block are not modeled"), so the controller's
//! `include AuthorizationProtection` named a constant nothing defined
//! (NameError at class load, e2e v2-r29, 8 ids). The `private` now acts
//! on the generated reader: the includer's instance reader is private,
//! the class-side reader stays public.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const CONCERN: &str = r#"module GuardedActions
  extend ActiveSupport::Concern

  included do
    class_attribute :guarded_list, instance_writer: false, instance_predicate: false
    private :guarded_list
    self.guarded_list = []
  end

  class_methods do
    def guard(*names)
      self.guarded_list += names
    end
  end

  def guarded?(name)
    guarded_list.include?(name)
  end
end
"#;

const APPLICATION_CONTROLLER: &str = "class ApplicationController < ActionController::Base\n  include GuardedActions\nend\n";

const WIDGETS_CONTROLLER: &str = r#"class WidgetsController < ApplicationController
  guard :show

  def index
    head(guarded?(:index) ? :forbidden : :ok)
  end

  def show
    head(guarded?(:show) ? :forbidden : :ok)
  end
end
"#;

const ROUTES: &str = "Rails.application.routes.draw do\n  resources :widgets, only: [:index, :show]\nend\n";


/// The concern's file is ingested (the includer no longer names an
/// undefined constant) and the includer's class body declares the
/// attribute and then makes its reader private, in the block's order.
///
/// `database_adapter: postgresql` selects the tree whose runtime carries
/// `class_attribute` and the Concern port; the replayed statements are
/// those the controller replay runs there (tests/*_pg_runtime.rs probes
/// the same shapes against native Rails).
#[test]
fn a_private_class_attribute_reader_in_a_controller_concern_is_ingested_and_replayed() {
    let overlay = emit_and_run::empty_app()
        .write("roundhouse.yml", "database_adapter: postgresql\n")
        .write("app/controllers/concerns/guarded_actions.rb", CONCERN)
        .write("app/controllers/application_controller.rb", APPLICATION_CONTROLLER)
        .write("app/controllers/widgets_controller.rb", WIDGETS_CONTROLLER)
        .write("config/routes.rb", ROUTES)
        .write("db/structure.sql", "CREATE TABLE widgets (\n    id bigserial NOT NULL,\n    CONSTRAINT widgets_pkey PRIMARY KEY (id)\n);\n");
    let (emitted, errors) = overlay.emit(roundhouse::project::BuildTarget::Ruby);
    let real: Vec<&String> = errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect();
    assert!(real.is_empty(), "{}", errors.join("\n"));
    let controller = std::fs::read_to_string(emitted.join("app/controllers/application_controller.rb"))
        .expect("emitted application_controller.rb");
    let declared = controller.find("class_attribute :guarded_list").unwrap_or_else(|| panic!("{controller}"));
    let private = controller.find("private :guarded_list").unwrap_or_else(|| panic!("no private:\n{controller}"));
    assert!(declared < private, "private must follow the declaration:\n{controller}");
    assert!(
        emitted.join("app/models/guarded_actions.rb").exists(),
        "the concern module is emitted (an `include` of it names a constant)"
    );
}
