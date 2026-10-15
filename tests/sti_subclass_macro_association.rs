//! An STI subclass's `readonly_belongs_to :location, ...` declares the
//! association on the subclass, as Rails does: Core's
//! `Shipping::ReturnLabel < Shipping::Label` does, and `ReturnDelivery
//! .returnLabel`'s type build raised `No association location on
//! Shipping::ReturnLabel` (the call was dropped as not modelled).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const STRUCTURE: &str = r#"CREATE TABLE labels (
    id bigserial NOT NULL,
    type character varying,
    location_id bigint,
    CONSTRAINT labels_pkey PRIMARY KEY (id)
);

CREATE TABLE locations (
    id bigserial NOT NULL,
    CONSTRAINT locations_pkey PRIMARY KEY (id)
);
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true

  def self.readonly_belongs_to(name, scope = nil, **options)
    belongs_to(name, scope, **options.merge(autosave: false))
  end
end
"#;

#[test]
fn an_sti_subclasss_readonly_belongs_to_declares_the_association_on_the_subclass() {
    let (emitted, _) = emit_and_run::empty_app()
        .write("roundhouse.yml", "database_adapter: postgresql\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/structure.sql", STRUCTURE)
        .write("app/models/application_record.rb", APPLICATION_RECORD)
        .write("app/models/shipping/label.rb", "module Shipping\n  class Label < ApplicationRecord\n  end\nend\n")
        .write(
            "app/models/shipping/return_label.rb",
            "module Shipping\n  class ReturnLabel < Label\n    readonly_belongs_to :location, optional: true, inverse_of: false\n  end\nend\n",
        )
        .write(
            "app/models/location.rb",
            "class Location < ApplicationRecord\n  def self.label_reflection\n    Shipping::ReturnLabel.reflect_on_association(:location)\n  end\nend\n",
        )
        .emit(roundhouse::project::BuildTarget::Ruby);
    let label = std::fs::read_to_string(emitted.join("app/models/shipping/return_label.rb")).expect("return_label.rb");
    assert!(label.contains("def location"), "the subclass must answer `location`:\n{label}");
    assert!(
        label.contains("BelongsToReflection.new(:location"),
        "the subclass must reflect on `location`:\n{label}"
    );
}
