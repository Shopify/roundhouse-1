//! A belongs_to's foreign key writer compares the new key with the
//! attribute, not with the model's own reader.
//!
//! Core's `PaymentsApi::PaymentAttemptCharge` overrides `payment_provider_id`
//! to derive it from `instruction` when the column is nil. Its
//! `payment_provider` belongs_to compiles `payment_provider_id=` with a
//! `value != payment_provider_id` guard that dropped a loaded target; the
//! guard called the override, which builds the instruction, which is not
//! assigned yet while `new(payment_provider_id: ..., instruction: {...})`
//! assigns the Hash-valued attribute last (as Rails does): "Cannot build
//! instruction from payment_attempt_charge with charge_type of payment".
//! Rails' stale_state reads the attribute, so the guard reads `@<fk>`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const STRUCTURE: &str = r#"CREATE TABLE providers (
    id bigserial NOT NULL,
    CONSTRAINT providers_pkey PRIMARY KEY (id)
);
CREATE TABLE charges (
    id bigserial NOT NULL,
    provider_id bigint,
    CONSTRAINT charges_pkey PRIMARY KEY (id)
);
"#;

const CHARGE: &str = r#"class Charge < ApplicationRecord
  belongs_to :provider, optional: true

  def provider_id
    self[:provider_id] || raise("derived from an instruction not assigned yet")
  end
end
"#;

#[test]
fn the_foreign_key_writer_guard_reads_the_attribute_not_the_overridden_reader() {
    let (emitted, errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", "database_adapter: postgresql\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/structure.sql", STRUCTURE)
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("app/models/provider.rb", "class Provider < ApplicationRecord\nend\n")
        .write("app/models/charge.rb", CHARGE)
        .emit(roundhouse::project::BuildTarget::Ruby);
    let errors: Vec<&String> = errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect();
    assert!(errors.is_empty(), "{errors:?}");
    let charge = std::fs::read_to_string(emitted.join("app/models/charge.rb")).unwrap();
    let start = charge.find("def provider_id=(").unwrap_or_else(|| panic!("no writer:\n{charge}"));
    let writer = &charge[start..];
    let writer = &writer[..writer.find("\n    end\n").map_or(writer.len(), |i| i + 9)];
    assert!(writer.contains("if value != @provider_id"), "{writer}");
    assert!(!writer.contains("!= provider_id"), "{writer}");
}
