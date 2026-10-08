//! A belongs_to target assigned through the writer is saved with its
//! owner when it is a new record, by Rails' rule (`new_record?`), not by
//! the foreign key it carries, against native Rails 8.2 on the same local
//! PostgreSQL database.
//!
//! The emitted writer used to stash the target, and `_autosave_<name>`
//! used to save it, only when the key read 0. On the uuid probe shape
//! (Core's `model_uuid_attributes`) the key is retyped: an unsaved target
//! whose id is generated at create answers nil, and one assigned an id
//! before its save answers that id. Neither is 0, so neither was saved:
//! the first INSERT hit `shop_id`'s NOT NULL, the second wrote a key with
//! no row behind it.
//!
//! The same model files run natively (Core's pinned Rails, over
//! `establish_connection`) and on the emitted program, each on a freshly
//! created `rh_e2e_belongs_to_autosave`, and every line
//! `AutosaveProbe.lines` prints must be equal, and equal to what Rails is
//! known to answer.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE shops (
    id uuid NOT NULL,
    name character varying,
    CONSTRAINT shops_pkey PRIMARY KEY (id)
);
CREATE TABLE products (
    id uuid NOT NULL,
    shop_id uuid NOT NULL,
    title character varying,
    CONSTRAINT products_pkey PRIMARY KEY (shop_id, id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_belongs_to_autosave
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

const SHOP: &str = r#"class Shop < ApplicationRecord
  OID_BASE = 0x5150

  uuid_attributes :id
  has_many :products
  before_create :generate_oid

  def generate_oid
    self.id ||= OID_BASE + Shop.count + 1
  end
end
"#;

const PRODUCT: &str = r#"class Product < ApplicationRecord
  OID_BASE = 0x51500000000000000000

  self.primary_key = :id
  uuid_attributes :id, :shop_id
  belongs_to :shop
  before_create :generate_oid

  def generate_oid
    self.id ||= OID_BASE + Product.count + 1
  end
end
"#;

/// Shaped like Core's Shopify::ModelUuidAsInteger.
const UUID_AS_INTEGER: &str = r##"module Probe
  class UuidAsInteger < ActiveModel::Type::Value
    ACCEPTABLE_UUID = /\A\h{8}-?\h{4}-?\h{4}-?\h{4}-?\h{12}\z/

    def type
      :uuid_as_integer
    end

    def deserialize(value)
      return if value.nil?
      cast_value(value)
    end

    def cast_value(value)
      if value.is_a?(Integer)
        value
      elsif value.is_a?(String) && ACCEPTABLE_UUID.match?(value)
        value.delete("-").to_i(16)
      elsif value.respond_to?(:to_i)
        value.to_i
      end
    end

    def serialize(value)
      return if value.nil?
      int = cast_value(value)
      return if int.nil?
      hex = int.to_s(16).rjust(32, "0")
      "#{hex[0, 8]}-#{hex[8, 4]}-#{hex[12, 4]}-#{hex[16, 4]}-#{hex[20, 12]}"
    end
  end
end
"##;

const INITIALIZER: &str =
    "ActiveRecord::Type.register(:uuid_as_integer, Probe::UuidAsInteger, adapter: :postgresql)\n";

const PROBE: &str = r##"class AutosaveProbe
  def self.lines
    out = []
    generated = Product.new(shop: Shop.new(name: "generated"), title: "g")
    out << "generated before: #{generated.shop_id.inspect}"
    generated.save!
    out << "generated: #{generated.shop.persisted?} #{generated.shop_id == Shop::OID_BASE + 1} #{Shop.find(generated.shop_id).name}"
    assigned = Product.new(shop: Shop.new(id: 0x77, name: "assigned"), title: "a")
    out << "assigned before: #{assigned.shop_id}"
    assigned.save!
    out << "assigned: #{assigned.shop.persisted?} #{Shop.exists?(0x77)} #{Product.find(assigned.id).shop.name}"
    saved = Shop.create!(name: "saved")
    saved.name = "renamed"
    kept = Product.create!(shop: saved, title: "k")
    out << "saved target: #{kept.shop_id == saved.id} #{Shop.find(saved.id).name} #{Shop.count}"
    out << "products: #{Product.count} #{Product.where(shop_id: 0x77).map(&:title).inspect}"
    out
  end
end
"##;

/// What native Rails 8.2 answers (checked on the task PostgreSQL), so two
/// identical wrong answers still fail.
const EXPECTED: [&str; 6] = [
    "generated before: nil",
    "generated: true true generated",
    "assigned before: 119",
    "assigned: true true assigned",
    "saved target: true saved 3",
    r#"products: 3 ["a"]"#,
];

/// What native Rails requires, in order.
const LOADS: [&str; 6] = [
    "lib/probe/uuid_as_integer",
    "config/initializers/uuid_types",
    "app/models/application_record",
    "app/models/shop",
    "app/models/product",
    "app/models/autosave_probe",
];

const FILES: [(&str, &str); 10] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/shop.rb", SHOP),
    ("app/models/product.rb", PRODUCT),
    ("app/models/autosave_probe.rb", PROBE),
    ("lib/probe/uuid_as_integer.rb", UUID_AS_INTEGER),
    ("config/initializers/uuid_types.rb", INITIALIZER),
];

fn overlay() -> emit_and_run::Overlay {
    FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

/// `check`'s errors other than the PostgreSQL adapter notice every
/// PostgreSQL probe app carries.
fn real_errors(errors: &[String]) -> Vec<&String> {
    errors
        .iter()
        .filter(|e| !e.contains("database_adapter not supported"))
        .collect()
}

/// The compiler side: the writer keeps a new record and the autosave
/// saves it by `new_record?`, whatever the key holds.
#[test]
fn belongs_to_autosave_keys_on_new_record() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let model = std::fs::read_to_string(emitted.join("app/models/product.rb")).unwrap();
    for expected in [
        "if value.new_record?",
        "if @shop_loaded && !(@shop_cache.nil?) && !(@shop_cache.destroyed?)",
        "@shop_cache.save if @shop_cache.new_record?",
        "self.shop = attrs[:shop] if !(attrs[:shop].nil?)",
        "@shop_cache.save",
        "@shop_id = @shop_cache.id",
    ] {
        assert!(model.contains(expected), "missing {expected:?} in:\n{model}");
    }
    assert!(!model.contains("value.id == 0"), "a 0-key stash survives:\n{model}");
    assert!(!model.contains("attrs[:shop].id"), "a keyword target bypasses the writer:\n{model}");
}

/// The model files natively and emitted, same PostgreSQL.
#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn belongs_to_autosave_matches_native_rails_on_postgresql() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let (native, emitted) = pg_probe::native_and_emitted(
        "belongs-to-autosave",
        &FILES,
        &emitted,
        "rh_e2e_belongs_to_autosave",
        "AutosaveProbe",
        &LOADS,
    );
    assert_eq!(native, EXPECTED, "native");
    assert_eq!(emitted, native, "emitted");
}
