//! A uuid column read and written as an Integer through an app type
//! registered with ActiveRecord::Type, declared by an app class macro
//! that calls `attribute` (Core's `model_uuid_attributes` over
//! Shopify::ModelUuidAsInteger), against native Rails 8.2 on the same
//! local PostgreSQL database.
//!
//! Which columns the macro retypes is decided when it runs, so the
//! compiler gives the columns it names (`Model.retypable_attributes`) a
//! path that consults the declared type at runtime: the reader answers
//! the cast value, hydration deserializes, every INSERT/UPDATE/DELETE,
//! reload and finder bind serializes, and a relation over such a column
//! (association reads, `where`, `pluck`, `group.count`) stays on the
//! runtime Relation, which casts the same way. `products` has Core's
//! composite primary key, which the schema ingest drops; the model's
//! `self.primary_key = :id` keys the record, and a `before_create` that
//! assigns the id (Core's GenerateOid) puts it in the INSERT.
//!
//! The same model files run natively (Core's pinned Rails, over
//! `establish_connection`) and on the emitted program, each on a freshly
//! created `rh_e2e_uuid_attributes`, and every line `UuidProbe.lines`
//! prints must be equal, and equal to what Rails is known to answer.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::path::{Path, PathBuf};
use std::process::Command;

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
  database: rh_e2e_uuid_attributes
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
  uuid_attributes :id
  has_many :products
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

  def shop_number
    shop_id + 1
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

const PROBE: &str = r##"class UuidProbe
  def self.lines
    out = []
    shop = Shop.create!(id: 0x1234, name: "a")
    out << "shop id: #{shop.id.class} #{shop.id}"
    other = Shop.new(name: "b")
    other.id = "00000000-0000-0000-0000-000000005678"
    other.save!
    out << "assigned uuid string: #{other.id.class} #{other.id}"
    prod = Product.create!(shop: shop, title: "t")
    out << "generated id: #{prod.id.class} #{prod.id == Product::OID_BASE + 1}"
    out << "shop_id: #{prod.shop_id.class} #{prod.shop_id == shop.id} #{prod.shop_number}"
    second = Product.create!(shop_id: other.id, title: "s")
    out << "second id: #{second.id - prod.id}"
    found = Product.find(prod.id)
    out << "find: #{found.title} #{found.id.class} #{found.shop_id == shop.id}"
    out << "find by uuid string: #{Product.find("00000000-0000-5150-0000-000000000001").title}"
    out << "find_by: #{Product.find_by(id: prod.id)&.title}"
    out << "where shop_id: #{Product.where(shop_id: shop.id).map(&:title).inspect}"
    out << "exists? id: #{Product.exists?(prod.id)} #{Product.exists?(id: second.id)} #{Product.exists?(0x99)}"
    out << "belongs_to: #{prod.shop.name} #{Product.find(second.id).shop.name}"
    out << "has_many: #{shop.products.map(&:title).inspect} #{other.products.count}"
    prod.title = "u"
    out << "update: #{prod.save} #{Product.find(prod.id).title}"
    Product.where(id: prod.id).update_all(title: "v")
    out << "update_all where id: #{prod.reload.title}"
    Product.where(id: prod.id).update_all(shop_id: other.id)
    prod.reload
    out << "update_all shop_id: #{prod.shop_id == other.id} #{prod.shop_id.class}"
    out << "pluck id: #{Product.where(shop_id: other.id).pluck(:id).map { |i| i - Product::OID_BASE }.sort.inspect}"
    out << "ids: #{Product.ids.map { |i| i.class }.uniq.inspect}"
    out << "group count: #{Product.group(:shop_id).count.map { |k, v| [k, v] }.sort.inspect}"
    second.destroy
    out << "destroy: #{Product.exists?(second.id)} #{Product.count}"
    gone = Shop.find(0x1234)
    out << "shop find: #{gone.name} #{gone.id}"
    out
  end
end
"##;

/// What native Rails 8.2 answers (checked on the task PostgreSQL), so two
/// identical wrong answers still fail.
const EXPECTED: [&str; 20] = [
    "shop id: Integer 4660",
    "assigned uuid string: Integer 22136",
    "generated id: Integer true",
    "shop_id: Integer true 4661",
    "second id: 1",
    "find: t Integer true",
    "find by uuid string: t",
    "find_by: t",
    r#"where shop_id: ["t"]"#,
    "exists? id: true true false",
    "belongs_to: a b",
    r#"has_many: ["t"] 1"#,
    "update: true u",
    "update_all where id: v",
    "update_all shop_id: true Integer",
    "pluck id: [1, 2]",
    "ids: [Integer]",
    "group count: [[22136, 2]]",
    "destroy: false 1",
    "shop find: a 4660",
];

const FILES: [(&str, &str); 10] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/shop.rb", SHOP),
    ("app/models/product.rb", PRODUCT),
    ("app/models/uuid_probe.rb", PROBE),
    ("lib/probe/uuid_as_integer.rb", UUID_AS_INTEGER),
    ("config/initializers/uuid_types.rb", INITIALIZER),
];

fn overlay() -> emit_and_run::Overlay {
    FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

/// `check`'s errors other than the PostgreSQL adapter notice every
/// PostgreSQL probe app carries and the two `uuid_attributes` body calls,
/// which run as written at class load (the runtime's `attribute`) until
/// macro replay answers them at compile time.
fn real_errors(errors: &[String]) -> Vec<&String> {
    errors
        .iter()
        .filter(|e| !e.contains("database_adapter not supported"))
        .filter(|e| !e.contains("uuid_attributes not supported"))
        .collect()
}

/// The compiler side: the named columns consult the declared type.
#[test]
fn uuid_attributes_compile_to_declared_type_calls() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let model = std::fs::read_to_string(emitted.join("app/models/product.rb")).unwrap();
    for expected in [
        r#"@id = self.class._attribute_cast("id", value)"#,
        r#"@shop_id = self.class._attribute_deserialize("shop_id", value)"#,
        r#"["id", "shop_id"]"#,
        r#"Db.escape_string_opt(self.class._attribute_serialize("id", @id))"#,
        r#"WHERE \"id\" = " + Db.escape_string_opt(self.class._attribute_serialize("id", @id))"#,
        "Shop.find_by(id: @shop_id)",
        "if @shop_id == nil",
        "  uuid_attributes :id, :shop_id\n",
    ] {
        assert!(model.contains(expected), "missing {expected:?} in:\n{model}");
    }
}

/// The model files natively and emitted, same PostgreSQL.
#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn uuid_attributes_match_native_rails_on_postgresql() {
    assert!(std::env::var_os("ROUNDHOUSE_PG_TEST_URL").is_some(), "set ROUNDHOUSE_PG_TEST_URL");
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let native_root: PathBuf = Path::new(env!("CARGO_TARGET_TMPDIR")).join("uuid-attributes-native");
    let _ = std::fs::remove_dir_all(&native_root);
    for (path, text) in FILES {
        let full = native_root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
    // The pg gem and Rails' native extensions come from Core's gem home,
    // as the oracle (tests/support/rails_oracle.rb) loads them.
    let gem_home = std::env::var("CORE_GEM_HOME")
        .unwrap_or_else(|_| "/nix/store/kr4swvh5mw01fs3kixfa9afy57zsyihg-ruby-gems-dev/lib/ruby/gems/4.0.0".into());
    let jemalloc = std::env::var("RH_JEMALLOC").unwrap_or_else(|_| {
        "/nix/store/c04j2i0fkin1bw5fab35gx896558knsd-jemalloc-5.3.0-unstable-2025-09-12/lib/libjemalloc.so.2".into()
    });
    let result = Command::new("ruby")
        .arg("tests/uuid_attributes_pg_runtime.rb")
        .arg(&native_root)
        .arg(&*emitted)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("GEM_HOME", &gem_home)
        .env("GEM_PATH", &gem_home)
        .env("LD_PRELOAD", &jemalloc)
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(result.status.success(), "stdout={stdout}\nstderr={}", String::from_utf8_lossy(&result.stderr));
    let answers: serde_json::Value = serde_json::from_str(stdout.trim()).expect("driver prints JSON");
    let lines = |side: &str| -> Vec<String> {
        let run = &answers[side];
        assert_eq!(run["ok"], true, "{side} run failed:\n{}", run["stderr"].as_str().unwrap_or(""));
        run["lines"].as_array().unwrap().iter().map(|l| l.as_str().unwrap().to_string()).collect()
    };
    let (native, emitted) = (lines("native"), lines("emitted"));
    assert_eq!(native, EXPECTED, "native");
    assert_eq!(emitted, native, "emitted");
}
