//! A type declared with `attribute :name, :time` under
//! `time_zone_aware_attributes`, against native Rails 8.2 on the same local
//! PostgreSQL database.
//!
//! Core's Publishable concerns declare `attribute :publish_on, :time` on
//! Product, Collection and others, with no column under it, and the app
//! is time zone aware. attribute_registration.rb's `attribute` passes the
//! declared type through `hook_attribute_type`, so it is a
//! TimeZoneConverter over Type::Time, and every assignment casts through
//! it. The emitted writer stored the assigned String as is; the runtime's
//! `attribute` (`_declared_attribute_types`, for a type object over a
//! column) did not hook the type. A `time` column's writer stored its
//! text uncast, so "11:00:00" read back as 11:00 UTC in the zone.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE products (
    id bigserial NOT NULL,
    title character varying,
    opens_at time without time zone,
    closes_at time without time zone,
    starts_at time without time zone,
    CONSTRAINT products_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_declared_time_attr
"#;

const APPLICATION: &str = r#"Bundler.require(*Rails.groups)

module DeclaredTimeProbe
  class Application < Rails::Application
    config.time_zone = "Eastern Time (US & Canada)"
  end
end
"#;

/// What railties and Active Record's railtie do natively at boot for
/// that config (the driver loads no Rails application).
const NATIVE_BOOT: &str = r#"require "active_support/all"
Time.zone_default = Time.find_zone!("Eastern Time (US & Canada)")
ActiveRecord::Base.time_zone_aware_attributes = true
ActiveRecord.default_timezone = :utc
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

const PRODUCT: &str = r#"class Product < ApplicationRecord
  attribute :publish_on, :time
  attribute :closes_at, :time
  attribute :starts_at, ActiveRecord::Type::Time.new
end
"#;

const PROBE: &str = r##"class DeclaredTimeAttrProbe
  def self.lines
    out = ["zone: #{Time.zone.name}"]
    %w[publish_on closes_at opens_at starts_at].each do |name|
      type = Product.type_for_attribute(name)
      cast = type.cast("10:30:15")
      out << "#{name}: #{type.type.inspect} #{type.is_a?(ActiveRecord::AttributeMethods::TimeZoneConversion::TimeZoneConverter)} #{cast.class} #{cast.utc?} #{[cast.year, cast.month, cast.day, cast.hour, cast.min, cast.sec].inspect}"
    end
    product = Product.new(title: "t", publish_on: "10:30:15", closes_at: "11:00:00", opens_at: "09:00:00", starts_at: "08:15:00")
    %w[publish_on closes_at opens_at starts_at].each do |name|
      value = product.public_send(name)
      out << "#{name} written: #{value.class} #{value.utc?} #{[value.hour, value.min, value.sec].inspect}"
    end
    product.save!
    reread = Product.find(product.id)
    %w[closes_at opens_at starts_at].each do |name|
      value = reread.public_send(name)
      out << "#{name} read: #{value.class} #{[value.year, value.month, value.day, value.hour, value.min, value.sec].inspect}"
    end
    out
  end
end
"##;

/// What native Rails 8.2 answers (checked on the task PostgreSQL), so two
/// identical wrong answers still fail.
const EXPECTED: [&str; 12] = [
    "zone: Eastern Time (US & Canada)",
    "publish_on: :time true ActiveSupport::TimeWithZone false [2000, 1, 1, 10, 30, 15]",
    "closes_at: :time true ActiveSupport::TimeWithZone false [2000, 1, 1, 10, 30, 15]",
    "opens_at: :time true ActiveSupport::TimeWithZone false [2000, 1, 1, 10, 30, 15]",
    "starts_at: :time true ActiveSupport::TimeWithZone false [2000, 1, 1, 10, 30, 15]",
    "publish_on written: ActiveSupport::TimeWithZone false [10, 30, 15]",
    "closes_at written: ActiveSupport::TimeWithZone false [11, 0, 0]",
    "opens_at written: ActiveSupport::TimeWithZone false [9, 0, 0]",
    "starts_at written: ActiveSupport::TimeWithZone false [8, 15, 0]",
    "closes_at read: ActiveSupport::TimeWithZone [2000, 1, 1, 11, 0, 0]",
    "opens_at read: ActiveSupport::TimeWithZone [2000, 1, 1, 9, 0, 0]",
    "starts_at read: ActiveSupport::TimeWithZone [2000, 1, 1, 8, 15, 0]",
];

const FILES: [(&str, &str); 9] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/application.rb", APPLICATION),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/product.rb", PRODUCT),
    ("app/models/declared_time_attr_probe.rb", PROBE),
    ("native_boot.rb", NATIVE_BOOT),
];

fn overlay() -> emit_and_run::Overlay {
    FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

/// The compiler side: the writer of a declared type over no column casts
/// through it.
#[test]
fn declared_time_writer_casts() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let product = std::fs::read_to_string(emitted.join("app/models/product.rb")).unwrap();
    let expected = r#"@publish_on = self.class._cast_column("publish_on", value)"#;
    assert!(product.contains(expected), "missing {expected:?} in:\n{product}");
}

/// The models natively and emitted, same PostgreSQL.
#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn declared_time_attr_matches_native_rails_on_postgresql() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "declared-time-attr",
        &FILES,
        &emitted,
        "rh_e2e_declared_time_attr",
        "DeclaredTimeAttrProbe",
        &["native_boot", "app/models/application_record", "app/models/product", "app/models/declared_time_attr_probe"],
    );
    assert_eq!(native, EXPECTED, "native");
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    assert_eq!(emitted_lines, native, "emitted");
}
