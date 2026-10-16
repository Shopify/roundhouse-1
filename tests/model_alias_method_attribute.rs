//! `alias_method :flat_fee, :credit_card_flat_rate` in a model body, the
//! target a column reader the body never `def`s (Core's
//! Payments::TransactionFee writes four on `unsafe_self`, Checkout::LineItem
//! and CartItem more: about a hundred sites). Ingest copies the def of a
//! target the body defines; this one is a method the compiled class
//! answers, and the statement used to be dropped with `not lowered`, so
//! `fee.flat_fee` raised NoMethodError. It is replayed in the class body
//! after the methods, guarded so a target the lowered class lacks warns
//! instead of dropping the model at load; without PostgreSQL.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const STRUCTURE: &str = r#"CREATE TABLE fees (
    id bigserial NOT NULL,
    amount numeric,
    credit_card_flat_rate numeric,
    CONSTRAINT fees_pkey PRIMARY KEY (id)
);
"#;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    concurrent-ruby (1.3.5)\n    i18n (1.15.2)\n      concurrent-ruby (~> 1.0)\n\nDEPENDENCIES\n  i18n\n";

const DATABASE_YML: &str = "default: &default\n  adapter: postgresql\n  encoding: unicode\n\ntest:\n  <<: *default\n  database: rh_e2e_alias_method_attribute\n";

const FEE: &str = r#"class Fee < ApplicationRecord
  unsafe_self = self #: as untyped
  unsafe_self.alias_method(:amount_before_tax, :amount)
  unsafe_self.alias_method(:amount_before_tax=, :amount=)
  alias_method :flat_fee, :credit_card_flat_rate
  alias_method :flat_fee=, :credit_card_flat_rate=

  def label
    "fee"
  end
  alias_method :caption, :label
end
"#;

fn emit_fee() -> String {
    let (emitted, errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", "database_adapter: postgresql\n")
        .write("Gemfile.lock", LOCK)
        .write("config/database.yml", DATABASE_YML)
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/structure.sql", STRUCTURE)
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("app/models/fee.rb", FEE)
        .emit(BuildTarget::Ruby);
    let dropped: Vec<&String> = errors.iter().filter(|e| e.contains("alias_method")).collect();
    assert!(dropped.is_empty(), "{dropped:#?}");
    std::fs::read_to_string(emitted.join("app/models/fee.rb")).unwrap()
}

#[test]
fn alias_method_of_a_column_reader_is_replayed_in_the_class_body() {
    let text = emit_fee();
    for (guard, alias) in [
        ("method_defined?(:amount)", "unsafe_self.alias_method(:amount_before_tax, :amount)"),
        ("method_defined?(:amount=)", "unsafe_self.alias_method(:amount_before_tax=, :amount=)"),
        ("method_defined?(:credit_card_flat_rate)", "alias_method :flat_fee, :credit_card_flat_rate"),
        ("method_defined?(:credit_card_flat_rate=)", "alias_method :flat_fee=, :credit_card_flat_rate="),
    ] {
        let replayed = format!("if {guard} || private_{guard}\n    {alias}\n  else\n    warn(");
        assert!(text.contains(&replayed), "`{alias}` is not replayed under its guard\n{text}");
    }
    assert!(text.contains("  def caption\n"), "a def's alias is still copied\n{text}");
}

#[test]
fn replayed_alias_method_parses_without_a_warning() {
    let (emitted, _) = emit_and_run::empty_app()
        .write("roundhouse.yml", "database_adapter: postgresql\n")
        .write("Gemfile.lock", LOCK)
        .write("config/database.yml", DATABASE_YML)
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/structure.sql", STRUCTURE)
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("app/models/fee.rb", FEE)
        .emit(BuildTarget::Ruby);
    let path = emitted.join("app/models/fee.rb");
    let out = std::process::Command::new("ruby").arg("-wc").arg(&path).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success() && !stderr.contains("warning"), "{stderr}");
}
