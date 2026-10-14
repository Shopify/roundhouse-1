//! A concern's `included do with_options … do readonly_has_one … end end`
//! declares the association on each includer. Core's `SellerOfRecord`
//! (included into `ShopLegalEntity`) declares `payment_gateways` and
//! `shopify_payments_account` there; `has_many`/`has_one` reached the
//! model but the `readonly_has_one` did not, so
//! `ShopifyPayments::Resolvers::ShopifyPaymentsAccount` raised
//! `undefined method 'shopify_payments_account' for an instance of
//! ShopLegalEntity`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const STRUCTURE: &str = r#"CREATE TABLE entities (
    id bigserial NOT NULL,
    shop_id bigint,
    CONSTRAINT entities_pkey PRIMARY KEY (id)
);

CREATE TABLE accounts (
    id bigserial NOT NULL,
    entity_id bigint,
    status character varying,
    CONSTRAINT accounts_pkey PRIMARY KEY (id)
);
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true

  def self.readonly_has_one(name, scope = nil, **options)
    has_one(name, scope, **options.merge(autosave: false))
  end
end
"#;

const CONCERN: &str = r#"module Ownable
  extend ActiveSupport::Concern

  included do
    with_options foreign_key: :entity_id, inverse_of: :entity do
      has_one :plain_account, class_name: "Account"
      readonly_has_one :enabled_account,
        -> { where(status: ["enabled", "configuring"]) },
        class_name: "Account"
    end
  end
end
"#;

#[test]
fn a_concerns_readonly_has_one_under_with_options_reaches_the_includer() {
    let (emitted, _) = emit_and_run::empty_app()
        .write("roundhouse.yml", "database_adapter: postgresql\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/structure.sql", STRUCTURE)
        .write("app/models/application_record.rb", APPLICATION_RECORD)
        .write("app/models/concerns/ownable.rb", CONCERN)
        .write("app/models/entity.rb", "class Entity < ApplicationRecord\n  include Ownable\nend\n")
        .write("app/models/account.rb", "class Account < ApplicationRecord\n  belongs_to :entity\nend\n")
        .emit(roundhouse::project::BuildTarget::Ruby);
    let entity = std::fs::read_to_string(emitted.join("app/models/entity.rb")).expect("entity.rb");
    assert!(entity.contains("def plain_account"), "{entity}");
    assert!(entity.contains("def enabled_account"), "the includer must answer `enabled_account`:\n{entity}");
    // The block's `foreign_key:` reached the macro call it wraps.
    assert!(entity.contains("entity_id"), "{entity}");
}
