//! A has_many reader answers the target class's class methods, run under
//! the association's scope, as Rails' CollectionProxy does: it delegates
//! to its scope, and `Relation::Delegation#method_missing` sends
//! `klass.respond_to?(method)` calls to the class inside `scoping`
//! (activerecord relation/delegation.rb, associations/collection_proxy.rb).
//! `shop.events.log(...)` ran `Event.log` with `Event.all` narrowed to
//! the shop's rows; the emitted proxy raised `undefined method 'log'`.
//!
//! The control is native Rails on Core's activerecord over the same local
//! PostgreSQL; the SQLite tree rewrites an association chain at compile
//! time and never builds the proxy.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

use roundhouse::project::BuildTarget;

const APPLICATION_RECORD: &str = "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n";
const SHOP: &str = "class Shop < ApplicationRecord\n  has_many :entries\nend\n";
const ENTRY: &str = r##"class Entry < ApplicationRecord
  belongs_to :shop

  # Class methods written for the model, read through the association: the
  # relation `scoping` makes current is the association's.
  def self.titles
    (current_scope || all).to_a.map(&:title).sort
  end

  def self.summary(prefix, suffix: "!")
    "#{prefix}#{(current_scope || all).to_a.size}#{suffix}"
  end
end
"##;
const PROBE: &str = r##"class ProxyProbe
  def self.lines
    out = []
    one = Shop.create!(name: "one")
    two = Shop.create!(name: "two")
    Entry.create!(shop: one, title: "b")
    Entry.create!(shop: one, title: "a")
    Entry.create!(shop: two, title: "other")
    shop = Shop.find(one.id)
    entries = shop.entries #: as untyped
    out << "class method on the proxy: #{entries.titles.inspect}"
    out << "with arguments: #{entries.summary("n=", suffix: "?")}"
    out << "unscoped class: #{Entry.titles.inspect}"
    out << "responds: #{entries.respond_to?(:titles)} #{entries.respond_to?(:nope)}"
    begin
      entries.nope
    rescue NoMethodError => e
      out << "unknown: #{e.class}"
    end
    out
  end
end
"##;
const EXPECTED: &[&str] = &[
    "class method on the proxy: [\"a\", \"b\"]",
    "with arguments: n=2?",
    "unscoped class: [\"a\", \"b\", \"other\"]",
    "responds: true false",
    "unknown: NoMethodError",
];
fn files() -> Vec<(&'static str, &'static str)> {
    vec![
        ("app/models/application_record.rb", APPLICATION_RECORD),
        ("app/models/shop.rb", SHOP),
        ("app/models/entry.rb", ENTRY),
        ("app/models/proxy_probe.rb", PROBE),
    ]
}

const STRUCTURE: &str = r#"CREATE TABLE shops (
    id bigserial NOT NULL,
    name character varying,
    CONSTRAINT shops_pkey PRIMARY KEY (id)
);
CREATE TABLE entries (
    id bigserial NOT NULL,
    shop_id bigint,
    title character varying,
    CONSTRAINT entries_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str =
    "default: &default\n  adapter: postgresql\n  encoding: unicode\n\ntest:\n  <<: *default\n  database: rh_e2e_collection_proxy\n";

#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn a_has_many_reader_answers_the_target_class_methods_under_its_scope_as_native() {
    let mut files = files();
    files.extend([
        ("roundhouse.yml", "database_adapter: postgresql\n"),
        ("config/database.yml", DATABASE_YML),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("db/structure.sql", STRUCTURE),
    ]);
    let (emitted, errors) =
        files.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text)).emit(BuildTarget::Ruby);
    let real: Vec<_> = errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect();
    assert!(real.is_empty(), "{}", errors.join("\n"));
    let loads = ["app/models/application_record", "app/models/shop", "app/models/entry", "app/models/proxy_probe"];
    let (native, emitted) = pg_probe::native_and_emitted(
        "collection-proxy",
        &files,
        &emitted,
        "rh_e2e_collection_proxy",
        "ProxyProbe",
        &loads,
    );
    assert_eq!(native, EXPECTED, "native");
    assert_eq!(emitted, native, "emitted");
}
