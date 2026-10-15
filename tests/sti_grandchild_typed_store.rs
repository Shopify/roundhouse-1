//! `typed_store` in the body of an STI subclass two levels below the
//! model (Core's `ExternalProviders::Apple < Social < ExternalProvider`).
//! The typed_store gem's `typed_store :data` runs `decorate_attributes`
//! for the column, which the runtime answers only for a column the
//! model compiled as retypable, so the analysis must follow subclasses
//! of subclasses, not only the model's direct ones.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const STRUCTURE: &str = r#"CREATE TABLE providers (
    id bigserial NOT NULL,
    type character varying,
    data text,
    CONSTRAINT providers_pkey PRIMARY KEY (id)
);
"#;

#[test]
fn typed_store_in_a_grandchild_makes_its_column_retypable() {
    let (emitted, errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", "database_adapter: postgresql\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/structure.sql", STRUCTURE)
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("app/models/provider.rb", "class Provider < ApplicationRecord\nend\n")
        .write("app/models/social_provider.rb", "class SocialProvider < Provider\nend\n")
        .write(
            "app/models/apple_provider.rb",
            "class AppleProvider < SocialProvider\n  typed_store :data do |s|\n    s.string :team_id\n  end\nend\n",
        )
        .emit(roundhouse::project::BuildTarget::Ruby);
    let real: Vec<&String> = errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect();
    assert!(real.is_empty(), "{}", errors.join("\n"));
    let provider = std::fs::read_to_string(emitted.join("app/models/provider.rb")).unwrap();
    let retypable = provider.split("def self._retypable_attribute_names").nth(1).unwrap_or_else(|| panic!("provider:\n{provider}"));
    let retypable = retypable.split("\n  end").next().unwrap();
    assert!(retypable.contains("\"data\""), "data is not retypable:\n{provider}");
}
