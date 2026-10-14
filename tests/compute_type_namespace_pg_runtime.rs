//! A `class_name:` that repeats the owner's outer namespace resolves
//! the way Rails' `compute_type` does.
//!
//! `belongs_to :shopify_payments_account, class_name:
//! "ShopifyPayments::Account"` on `ShopifyPayments::Capability` first
//! tries the candidate `ShopifyPayments::Capability::ShopifyPayments::Account`,
//! whose lookup raises `NameError` named `ShopifyPayments` (a middle
//! segment, not the last). The runtime re-raised anything not named for
//! the last segment, so ~100 Core test files died at load with
//! `uninitialized constant ShopifyPayments::Capability::ShopifyPayments`.
//! Rails' `safe_constantize` swallows a `NameError` named by any segment
//! of the candidate and moves on to the next one. Reading `klass` needs
//! no database, so this runs without PostgreSQL.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE shopify_payments_accounts (
    id bigserial NOT NULL,
    name character varying,
    CONSTRAINT shopify_payments_accounts_pkey PRIMARY KEY (id)
);
CREATE TABLE shopify_payments_capabilities (
    id bigserial NOT NULL,
    shopify_payments_account_id bigint,
    shop_id bigint,
    CONSTRAINT shopify_payments_capabilities_pkey PRIMARY KEY (id)
);
CREATE TABLE shops (
    id bigserial NOT NULL,
    name character varying,
    CONSTRAINT shops_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_compute_type
"#;

const ACCOUNT: &str = r#"module ShopifyPayments
  class Account < ApplicationRecord
    self.table_name = "shopify_payments_accounts"
  end
end
"#;

const CAPABILITY: &str = r#"module ShopifyPayments
  class Capability < ApplicationRecord
    self.table_name = "shopify_payments_capabilities"
    belongs_to :shopify_payments_account, class_name: "ShopifyPayments::Account"
    belongs_to :inner, class_name: "Account", foreign_key: :shopify_payments_account_id
    belongs_to :shop
    belongs_to :missing, class_name: "ShopifyPayments::Missing", optional: true, foreign_key: :shop_id
  end
end
"#;

const PROBE: &str = r##"class ComputeTypeProbe
  def self.lines
    cap = ShopifyPayments::Capability
    out = []
    %i[shopify_payments_account inner shop].each do |name|
      out << "#{name}: #{cap.reflect_on_association(name).klass}"
    end
    begin
      cap.reflect_on_association(:missing).klass
      out << "missing: no error"
    rescue NameError => e
      out << "missing: #{e.class}"
    end
    out
  end
end
"##;

const FILES: [(&str, &str); 9] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n"),
    ("app/models/shop.rb", "class Shop < ApplicationRecord\nend\n"),
    ("app/models/shopify_payments/account.rb", ACCOUNT),
    ("app/models/shopify_payments/capability.rb", CAPABILITY),
    ("app/models/compute_type_probe.rb", PROBE),
];

const EXPECTED: [&str; 4] = [
    "shopify_payments_account: ShopifyPayments::Account",
    "inner: ShopifyPayments::Account",
    "shop: Shop",
    "missing: NameError",
];

#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn class_name_repeating_the_outer_namespace_resolves_as_rails_does() {
    let overlay = FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text));
    let (emitted, errors) = overlay.emit(roundhouse::project::BuildTarget::Ruby);
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "compute-type-namespace",
        &FILES,
        &emitted,
        "rh_e2e_compute_type",
        "ComputeTypeProbe",
        &[
            "app/models/application_record",
            "app/models/shop",
            "app/models/shopify_payments/account",
            "app/models/shopify_payments/capability",
            "app/models/compute_type_probe",
        ],
    );
    assert_eq!(native, EXPECTED, "native");
    let real: Vec<&String> = errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect();
    assert!(real.is_empty(), "{real:#?}");
    assert_eq!(emitted_lines, native, "emitted");
}
