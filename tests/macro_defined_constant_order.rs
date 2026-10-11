//! A class-body constant that reads what a class-body macro defined.
//!
//! `VALID = values` after `enum_values :state, [...]` (an extended app
//! module's macro defining the singleton reader `values` with
//! `define_singleton_method`): Ruby runs the body top to bottom, so the
//! macro has defined the reader when the constant reads it. The constant
//! was emitted ahead of the macro call, and the read was a NameError.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const MACRO: &str = r#"module EnumValues
  def enum_values(column, possible)
    singleton_class.send(:define_method, column.to_s.pluralize) { possible }
  end
end
"#;

const PLAIN: &str = r#"class Switch
  extend EnumValues

  ON = :on
  OFF = :off

  enum_values :state, [ON, OFF]

  VALID_STATES = states #: Array[Symbol]
end
"#;

#[test]
fn a_constant_reading_a_macro_defined_reader_runs_after_the_macro() {
    let run = emit_and_run::empty_app()
        .write("app/models/enum_values.rb", MACRO)
        .write("app/models/switch.rb", PLAIN)
        .run_ruby("p Switch::VALID_STATES\n");
    run.assert_passes();
    assert_eq!(run.stdout, "[:on, :off]\n");
}

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    concurrent-ruby (1.3.5)\n    i18n (1.15.2)\n      concurrent-ruby (~> 1.0)\n\nDEPENDENCIES\n  i18n\n";

const STRUCTURE: &str = r#"CREATE TABLE lamps (
    id bigserial NOT NULL,
    status character varying,
    CONSTRAINT lamps_pkey PRIMARY KEY (id)
);
"#;

const APPLICATION_RECORD: &str = "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n";

const LAMP: &str = r#"class Lamp < ApplicationRecord
  extend EnumValues

  LIT = :lit
  DARK = :dark

  enum_values :status, [LIT, DARK]

  VALID_STATUSES = statuses #: Array[Symbol]
  FINAL_STATUSES = [LIT].freeze #: Array[Symbol]
end
"#;

/// A model's constant below the macro call that defines the reader it
/// calls is written below that call.
#[test]
fn a_model_constant_reading_a_macro_defined_reader_follows_the_macro() {
    let (emitted, _errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", "database_adapter: postgresql\n")
        .write("Gemfile.lock", LOCK)
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/structure.sql", STRUCTURE)
        .write("app/models/enum_values.rb", MACRO)
        .write("app/models/application_record.rb", APPLICATION_RECORD)
        .write("app/models/lamp.rb", LAMP)
        .emit(roundhouse::project::BuildTarget::Ruby);
    let text = std::fs::read_to_string(emitted.join("app/models/lamp.rb")).unwrap();
    let call = text.find("enum_values :status").expect(&text);
    let read = text.find("VALID_STATUSES = statuses").expect(&text);
    assert!(call < read, "{text}");
}
