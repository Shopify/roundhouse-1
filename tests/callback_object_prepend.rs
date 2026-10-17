//! A Concern's `included do` block registers lifecycle callbacks the
//! includer runs: a callback OBJECT (`after_create Methods`, which Rails
//! sends `after_create(record)`) and `before_save :x, prepend: true`.
//!
//! Both were dropped ("after_create not supported ... model DSL call on
//! `Markets::Definition` not lowered"), so Core's
//! `Markets::ComponentCallbacks::CoreModel` never ran
//! `Markets::Platform::Service::SynchronizationCallback`: a created market
//! had no condition and `CreateMarket#save_condition!` raised
//! `Called not_nil! on nil`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const CALLBACKS: &str = r#"module Hooks
  module Methods
    extend self

    def after_create(record)
      record.log << :object_after_create
    end
  end

  module CoreModel
    extend ActiveSupport::Concern

    included do
      before_save :first_hook, prepend: true
      after_create Methods
    end

    def first_hook
      log << :first
    end
  end
end
"#;
const WIDGET: &str = "class Widget < ApplicationRecord\n  include Hooks::CoreModel\n  before_save :second_hook\n\n  def log\n    @log ||= []\n  end\n\n  def second_hook\n    log << :second\n  end\nend\n";

fn emit() -> (String, Vec<String>) {
    let (tree, errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", "database_adapter: postgresql\n")
        .write("Gemfile", "source \"https://rubygems.org\"\ngem \"rails\"\n")
        .write("config.ru", "")
        .write("config/boot.rb", "require \"bundler/setup\"\n")
        .write("config/application.rb", "require_relative \"boot\"\nrequire \"rails\"\nBundler.require(*Rails.groups)\n")
        .write("config/environments/test.rb", "Rails.application.configure do\n  config.eager_load = false\nend\n")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_callback_object_unused\n")
        .write("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table \"widgets\" do |t|\n    t.string \"name\"\n  end\nend\n")
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("app/models/hooks.rb", CALLBACKS)
        .write("app/models/widget.rb", WIDGET)
        .emit(BuildTarget::Ruby);
    (std::fs::read_to_string(tree.join("app/models/widget.rb")).expect("widget.rb"), errors)
}

#[test]
fn callback_object_and_prepend_from_a_concern_run_on_the_includer() {
    let (widget, errors) = emit();
    assert!(
        widget.contains("Hooks::Methods.after_create self") || widget.contains("Hooks::Methods.after_create(self)"),
        "the callback object is sent after_create(record), named in full:\n{widget}"
    );
    let hook = &widget[widget.find("def before_save").expect("before_save override")..];
    let first = hook.find("first_hook").expect("first_hook is called");
    let second = hook.find("second_hook").expect("second_hook is called");
    assert!(first < second, "prepend: true runs first_hook ahead of second_hook:\n{widget}");
    assert!(!errors.iter().any(|e| e.contains("not lowered")), "{errors:#?}");
}

#[test]
fn a_models_own_attributes_ivar_read_binds_the_attribute_view() {
    let (tree, _) = emit_and_run::empty_app()
        .write("roundhouse.yml", "database_adapter: postgresql\n")
        .write("Gemfile", "source \"https://rubygems.org\"\ngem \"rails\"\n")
        .write("config.ru", "")
        .write("config/boot.rb", "require \"bundler/setup\"\n")
        .write("config/application.rb", "require_relative \"boot\"\nrequire \"rails\"\nBundler.require(*Rails.groups)\n")
        .write("config/environments/test.rb", "Rails.application.configure do\n  config.eager_load = false\nend\n")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_callback_object_unused\n")
        .write("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table \"gadgets\" do |t|\n    t.string \"name\"\n  end\nend\n")
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("app/models/gadget.rb", "class Gadget < ApplicationRecord\n  def raw_name\n    @attributes[\"name\"].value\n  end\nend\n")
        .emit(BuildTarget::Ruby);
    let gadget = std::fs::read_to_string(tree.join("app/models/gadget.rb")).expect("gadget.rb");
    assert!(gadget.contains("_attribute_set[\"name\"].value"), "{gadget}");
}
