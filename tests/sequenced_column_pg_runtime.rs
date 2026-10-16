//! `sequenced_column :number, scope: :shop` (sequenced_column 1.2.1), against
//! the gem itself on native Rails 8.2 and the same local PostgreSQL database.
//!
//! The gem includes `SequencedColumn` into ActiveRecord::Base from an
//! `on_load(:active_record)` hook; the macro records the column and registers
//! `before_create :_record_sequences` and `after_rollback ..., on: :create`.
//! It is Rails-dependent, so the PostgreSQL tree carries a plain-Ruby port
//! (runtime/spinel/sequenced_column.rb, `project::GEM_RUNTIME_PORTS`) loaded
//! where the app's Bundler.require loads the gem, and the macro is replayed
//! in the model body (`macro_replay`).
//!
//! Before: the call was dropped ("sequenced_column not supported ... the
//! class does not run it"), so `returns.number` (NOT NULL) stayed NULL at
//! create and Core's fixtures and factories failed to insert.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE shops (
    id bigserial NOT NULL,
    name character varying,
    CONSTRAINT shops_pkey PRIMARY KEY (id)
);
CREATE TABLE tickets (
    id bigserial NOT NULL,
    shop_id bigint NOT NULL,
    number bigint NOT NULL,
    CONSTRAINT tickets_pkey PRIMARY KEY (id)
);
CREATE TABLE pages (
    id bigserial NOT NULL,
    shop_id bigint NOT NULL,
    prefix character varying NOT NULL,
    CONSTRAINT pages_pkey PRIMARY KEY (id)
);
CREATE TABLE slots (
    id bigserial NOT NULL,
    shop_id bigint NOT NULL,
    number bigint,
    CONSTRAINT slots_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_sequenced_column
"#;

const APPLICATION: &str = r#"require_relative "boot"
require "rails/all"
Bundler.require(*Rails.groups)

module SeqProbe
  class Application < Rails::Application
  end
end
"#;

const GEMFILE: &str = "source \"https://rubygems.org\"\ngem \"rails\"\ngem \"sequenced_column\", \"= 1.2.1\"\n";

const APPLICATION_RECORD: &str = "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n";

/// Core's initializer store (components/platform/essentials/app/utils).
const STORE: &str = r#"module SequencedColumn
  class InMemoryStore
    def initialize
      @sequences = {}
      @recycled = {}
    end

    def recycled(recycled_key)
      list = @recycled[recycled_key]
      return 0 if list.nil? || list.empty?
      list.pop.to_i
    end

    def reserve(sequence_key, db_count, _ttl)
      @sequences[sequence_key] ||= db_count.to_i
      @sequences[sequence_key] += 1
      @sequences[sequence_key]
    end

    def delete(sequence_key)
      @sequences.delete(sequence_key)
    end

    def commit(recycled_key, _sequence_key, value)
      @recycled[recycled_key] ||= []
      @recycled[recycled_key].unshift(value)
      nil
    end
  end
end
"#;

const SHOP: &str = "class Shop < ApplicationRecord\nend\n";
const TICKET: &str = "class Ticket < ApplicationRecord\n  belongs_to :shop\n  sequenced_column :number, scope: :shop\nend\n";
const PAGE: &str = "class Page < ApplicationRecord\n  belongs_to :shop\n  sequenced_column :prefix, scope: :shop, value_format: \"/t/%i\"\nend\n";
const SLOT: &str = "class Slot < ApplicationRecord\n  belongs_to :shop\n  sequenced_column :number, scope: :shop, no_overwrite: true\nend\n";

const PROBE: &str = r##"class SeqRun
  def self.lines
    # The gem logs through the record's `logger` (Rails.logger in an app);
    # the tree's base has none, and the port logs only where one answers.
    ActiveRecord::Base.logger = Logger.new(IO::NULL) if ActiveRecord::Base.respond_to?(:logger=)
    SequencedColumn.store = SequencedColumn::InMemoryStore.new
    out = []
    a = Shop.create!(name: "a")
    b = Shop.create!(name: "b")
    out << "a: #{Array.new(2) { Ticket.create!(shop: a).number }.inspect}"
    out << "b: #{Ticket.create!(shop: b).number}"
    # A fresh store seeds from the largest value in the scope.
    SequencedColumn.store = SequencedColumn::InMemoryStore.new
    out << "reseeded: #{Ticket.create!(shop: a).number}"
    rolled = nil
    Ticket.transaction do
      rolled = Ticket.create!(shop: a)
      out << "inside: #{rolled.number}"
      raise ActiveRecord::Rollback
    end
    out << "rolled back: #{rolled.number.inspect} rows=#{Ticket.where(shop_id: a.id).count}"
    out << "recycled: #{Ticket.create!(shop: a).number}"
    out << "pages: #{Array.new(2) { Page.create!(shop: a).prefix }.inspect}"
    out << "slots: #{[Slot.create!(shop: a).number, Slot.create!(shop: a, number: 41).number, Slot.create!(shop: a).number].inspect}"
    begin
      Ticket.new(shop_id: nil).save!(validate: false)
    rescue => e
      out << "nil scope: #{e.class}: #{e.message}"
    end
    out
  end
end
"##;

/// What sequenced_column 1.2.1 answers natively (checked on the task
/// PostgreSQL), so two identical wrong answers still fail.
const EXPECTED: [&str; 9] = [
    "a: [1, 2]",
    "b: 1",
    "reseeded: 3",
    "inside: 4",
    "rolled back: nil rows=3",
    "recycled: 4",
    r#"pages: ["/t/1", "/t/2"]"#,
    "slots: [1, 41, 42]",
    "nil scope: RuntimeError: The following scopes had nil values: shop_id",
];

/// What native Rails requires, in order (the first only natively: the
/// gem itself, where the emitted tree loads the port).
const LOADS: [&str; 8] = [
    "native/sequenced_column_gem.rb",
    "app/models/application_record.rb",
    "app/models/sequenced_column/in_memory_store.rb",
    "app/models/shop.rb",
    "app/models/ticket.rb",
    "app/models/page.rb",
    "app/models/slot.rb",
    "app/models/seq_run.rb",
];

/// Native only: the pinned gem's lib on the load path (its closure needs
/// only active_support), required as Bundler.require would.
const NATIVE_GEM: &str = r#"gem "railties"
lib = ENV.fetch("SEQUENCED_COLUMN_LIB", "/home/tobi/src/spinel-work/core-bundle/b3/bundle/ruby/3.4.0/gems/sequenced_column-1.2.1/lib")
$LOAD_PATH.unshift(lib)
require "rails"
require "sequenced_column"
"#;

const FILES: [(&str, &str); 13] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/application.rb", APPLICATION),
    ("Gemfile", GEMFILE),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/sequenced_column/in_memory_store.rb", STORE),
    ("app/models/shop.rb", SHOP),
    ("app/models/ticket.rb", TICKET),
    ("app/models/page.rb", PAGE),
    ("app/models/slot.rb", SLOT),
    ("app/models/seq_run.rb", PROBE),
];

fn overlay() -> emit_and_run::Overlay {
    FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

/// The macro replays in the model body, and the tree loads the port where
/// Bundler.require loads the gem.
#[test]
fn sequenced_column_replays_through_the_gem_port() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let ticket = std::fs::read_to_string(emitted.join("app/models/ticket.rb")).unwrap();
    assert!(ticket.contains("sequenced_column(:number, scope: :shop)") || ticket.contains("sequenced_column :number, scope: :shop"), "{ticket}");
    let facades = std::fs::read_to_string(emitted.join("runtime/gem_facades.rb")).unwrap();
    assert!(facades.contains("require_relative \"sequenced_column\""), "{facades}");
    assert!(emitted.join("runtime/sequenced_column.rb").exists());
    let feature = std::fs::read_to_string(emitted.join("runtime/features/sequenced_column.rb")).unwrap();
    assert!(feature.contains("require_relative \"../sequenced_column\""), "{feature}");
}

/// Without the gem in the Gemfile the macro stays refused by name.
#[test]
fn sequenced_column_stays_dropped_without_the_gem() {
    let (emitted, errors) = overlay()
        .write("Gemfile", "source \"https://rubygems.org\"\ngem \"rails\"\n")
        .emit(roundhouse::project::BuildTarget::Ruby);
    let ticket = std::fs::read_to_string(emitted.join("app/models/ticket.rb")).unwrap();
    assert!(!ticket.contains("sequenced_column"), "{ticket}");
    assert!(errors.iter().any(|e| e.contains("sequenced_column")), "{}", errors.join("\n"));
    let facades = std::fs::read_to_string(emitted.join("runtime/gem_facades.rb")).unwrap();
    assert!(!facades.contains("require_relative \"sequenced_column\""), "{facades}");
}

/// The same models natively, on the gem, and emitted, on the port.
#[test]
#[ignore = "requires pg gem, Core's pinned Rails and gem home, and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn sequenced_column_matches_the_gem_on_postgresql() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let native_files: Vec<(&str, &str)> =
        FILES.iter().copied().chain([("native/sequenced_column_gem.rb", NATIVE_GEM)]).collect();
    let (native, emitted) = pg_probe::native_and_emitted(
        "sequenced-column",
        &native_files,
        &emitted,
        "rh_e2e_sequenced_column",
        "SeqRun",
        &LOADS,
    );
    assert_eq!(native, EXPECTED, "native");
    assert_eq!(emitted, native, "emitted");
}
