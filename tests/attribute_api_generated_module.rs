//! `attribute :publish_on, :time` declared in a concern's `included` block
//! writes its accessors into the model's GeneratedAttributeMethods module,
//! as Rails does, so the concern's own `publish_on=` (a module included
//! after it) overrides them. Written into the class body instead, they
//! shadowed the concern: Core's `ProductPublication#publish_on=("")` cast
//! the blank to a time and never marked the future publishable for
//! destruction.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const STRUCTURE: &str = r#"CREATE TABLE publications (
    id bigserial NOT NULL,
    published_at timestamp without time zone,
    CONSTRAINT publications_pkey PRIMARY KEY (id)
);
"#;

#[test]
fn an_attribute_api_accessor_does_not_shadow_the_concerns_writer() {
    let (emitted, _) = emit_and_run::empty_app()
        .write("roundhouse.yml", "database_adapter: postgresql\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/structure.sql", STRUCTURE)
        .write(
            "app/models/application_record.rb",
            "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n",
        )
        .write(
            "app/models/concerns/resource_publication.rb",
            "module ResourcePublication\n  extend ActiveSupport::Concern\n\n  included do\n    attribute :publish_on, :time\n  end\n\n  def publish_on=(value)\n    @seen = value\n  end\nend\n",
        )
        .write(
            "app/models/publication.rb",
            "class Publication < ApplicationRecord\n  include ResourcePublication\nend\n",
        )
        .emit(roundhouse::project::BuildTarget::Ruby);
    let src = std::fs::read_to_string(emitted.join("app/models/publication.rb")).expect("publication.rb");
    let module_end = src.find("private_constant :GeneratedAttributeMethods").expect("generated module");
    let module = &src[..module_end];
    assert!(module.contains("def publish_on="), "the attribute accessor belongs to the generated module:\n{src}");
    let after = &src[module_end..];
    assert!(!after.contains("def publish_on="), "no class-body writer may shadow the concern:\n{src}");
}
