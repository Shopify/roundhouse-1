//! A model constant whose value names one of the model's STI
//! subclasses, as Core's Markets::Region does:
//!
//!     PRIORITIES = { Markets::Region::Country.name => 1, ... }.freeze
//!
//! Naming the subclass loads it, and its class body runs against the
//! model: Country's `model_uuid_attributes :definition_id` reaches
//! `attribute`, which asks the model's compiled
//! `_retypable_attribute_names`. The emitted model wrote its constants
//! above its defs, so the subclass saw the Base's empty list and raised
//! NotImplementedError at load. Such a constant now waits for the defs,
//! where the source had it, like one that calls the class itself.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const STRUCTURE: &str = r#"CREATE TABLE items (
    id bigserial NOT NULL,
    type character varying,
    ref uuid,
    CONSTRAINT items_pkey PRIMARY KEY (id)
);
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true

  def self.uuid_attributes(*names)
    names.each do |name|
      attribute name.to_sym, :uuid_as_integer
    end
  end
end
"#;

const ITEM: &str = r#"class Item < ApplicationRecord
  uuid_attributes :ref
  PRIORITIES = { SpecialItem.name => 1 }.freeze
  LIMIT = 3
end
"#;

const SPECIAL_ITEM: &str = r#"class SpecialItem < Item
  uuid_attributes :ref
end
"#;

#[test]
fn constant_naming_a_subclass_follows_the_compiled_defs() {
    let app = emit_and_run::empty_app()
        .write("roundhouse.yml", "database_adapter: postgresql\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/structure.sql", STRUCTURE)
        .write("app/models/application_record.rb", APPLICATION_RECORD)
        .write("app/models/item.rb", ITEM)
        .write("app/models/special_item.rb", SPECIAL_ITEM);
    let (emitted, _errors) = app.emit(roundhouse::project::BuildTarget::Ruby);
    let item = std::fs::read_to_string(emitted.join("app/models/item.rb")).unwrap();
    let at = |needle: &str| item.find(needle).unwrap_or_else(|| panic!("no {needle:?} in:\n{item}"));
    let retypable = at("def self._retypable_attribute_names");
    assert!(retypable < at("PRIORITIES = "), "{item}");
    // A constant that names no subclass keeps its place above the defs.
    assert!(at("LIMIT = 3") < retypable, "{item}");
}
