//! A Zeitwerk `on_load` loop over literal model paths (Core's
//! components/platform/essentials/config/initializers/ledger_sharding.rb:
//! `LedgerSharding::MODELS.each_key { |model_name| Rails.autoloaders.main
//! .on_load(model_name) { |klass, _| klass.include(ModelUuidColumns);
//! klass.model_uuid_attributes(*TABLE[model_name]) … } }`), against
//! native Zeitwerk 2.8.2 and Rails 8.2 on the same local PostgreSQL
//! database.
//!
//! Run as a callback, the include of a plain module does not extend its
//! `ClassMethods` on a compiled model, so none of the models got the
//! concern's class macro. Ingest expands the loop into each model's
//! body (`ingest::autoload_loop_expansion`), where the include and the
//! macro are what a model's own are.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    concurrent-ruby (1.3.5)\n    i18n (1.15.2)\n      concurrent-ruby (~> 1.0)\n\nDEPENDENCIES\n  i18n\n";

const STRUCTURE: &str = r#"CREATE TABLE shops (
    id bigserial NOT NULL,
    name character varying,
    CONSTRAINT shops_pkey PRIMARY KEY (id)
);

CREATE TABLE orders (
    id bigserial NOT NULL,
    name character varying,
    CONSTRAINT orders_pkey PRIMARY KEY (id)
);

CREATE TABLE notes (
    id bigserial NOT NULL,
    name character varying,
    CONSTRAINT notes_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_autoload_loop
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

/// The concern the loop includes into each model: a class macro whose
/// body declares an alias (macro_replay).
const SHOUTING: &str = r#"module Shouting
  extend ActiveSupport::Concern

  class_methods do
    def shouts(new_name, column)
      alias_attribute(new_name, column)
    end
  end

  def loud_name
    name.upcase
  end
end
"#;

const SHOP: &str = "class Shop < ApplicationRecord\nend\n";
const ORDER: &str = "class Order < ApplicationRecord\nend\n";
const NOTE: &str = "class Note < ApplicationRecord\nend\n";

/// Per model: a table entry (Note has none, so its `shouts` call folds
/// away), a validation that reads the loop variable, a callback that
/// runs a block with the class's own methods.
const INITIALIZER: &str = r#"module LoopSharding
  MODELS = {
    "Shop" => -> { Shop },
    "Order" => -> { Order },
    "Note" => -> { Note },
  }.freeze #: Hash[String, untyped]

  ALIASES = {
    "Shop" => [:handle, :name],
    "Order" => [:reference, :name],
  }.freeze #: Hash[String, Array[Symbol]]
end

LoopSharding::MODELS.each_key do |model_name|
  Rails.autoloaders.main.on_load(model_name) do |klass, _abspath|
    klass.include(Shouting)
    aliases = LoopSharding::ALIASES[model_name]
    klass.shouts(*aliases) if aliases
    klass.validates(:name, presence: true)
  end
end
"#;

const PROBE: &str = r##"class AutoloadLoopProbe
  def self.lines
    out = []
    out << "ancestors: #{[Shop, Order, Note].map { |m| m.include?(Shouting) }.inspect}"
    shop = Shop.create!(handle: "abc")
    out << "shop: #{shop.name.inspect} #{Shop.find(shop.id).handle.inspect} #{shop.loud_name.inspect}"
    order = Order.create!(reference: "o-1")
    out << "order: #{order.name.inspect} #{Order.find(order.id).reference.inspect} #{order.loud_name.inspect}"
    out << "note alias: #{Note.new.respond_to?(:handle)} #{Note.new.respond_to?(:reference)}"
    out << "valid: #{Shop.new.valid?} #{Order.new.valid?} #{Note.new.valid?} #{Note.new(name: "n").valid?}"
    out << "errors: #{Note.new.tap(&:valid?).errors.full_messages.inspect}"
    out
  end
end
"##;

/// Native boot with the real Zeitwerk: the app's models and concerns as
/// the main loader's roots, then the initializers, in order. Native only.
const NATIVE_BOOT: &str = r#"require "zeitwerk"

module Rails
  def self.autoloaders
    @autoloaders ||= Struct.new(:main).new(Zeitwerk::Loader.new)
  end
end

loader = Rails.autoloaders.main
loader.push_dir(File.join(__dir__, "app/models"))
loader.push_dir(File.join(__dir__, "app/models/concerns"))
loader.setup
Dir[File.join(__dir__, "config/initializers/*.rb")].sort.each { |file| load file }
"#;

/// What native Zeitwerk 2.8.2 and Rails 8.2 answer (checked on the task
/// PostgreSQL), so two identical wrong answers still fail.
const EXPECTED: [&str; 6] = [
    r#"ancestors: [true, true, true]"#,
    r#"shop: "abc" "abc" "ABC""#,
    r#"order: "o-1" "o-1" "O-1""#,
    r#"note alias: false false"#,
    r#"valid: false false false true"#,
    r#"errors: ["Name can't be blank"]"#,
];

const FILES: [(&str, &str); 12] = [
    ("Gemfile.lock", LOCK),
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/concerns/shouting.rb", SHOUTING),
    ("app/models/shop.rb", SHOP),
    ("app/models/order.rb", ORDER),
    ("app/models/note.rb", NOTE),
    ("app/models/autoload_loop_probe.rb", PROBE),
    ("config/initializers/loop_sharding.rb", INITIALIZER),
];

fn overlay() -> emit_and_run::Overlay {
    FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn autoload_loop_matches_native_zeitwerk_on_postgresql() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    let native_files: Vec<(&str, &str)> = FILES.iter().copied().chain([("native_boot.rb", NATIVE_BOOT)]).collect();
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "autoload-loop",
        &native_files,
        &emitted,
        "rh_e2e_autoload_loop",
        "AutoloadLoopProbe",
        &["native_boot"],
    );
    assert_eq!(native, EXPECTED, "native");
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    assert_eq!(emitted_lines, native, "emitted");
}

/// The loop's statements end each model's body, written for it; the
/// loop itself is not carried to run again at boot.
#[test]
fn the_loop_is_expanded_into_each_model_body() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let read = |rel: &str| std::fs::read_to_string(emitted.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
    let shop = read("app/models/shop.rb");
    assert!(shop.contains("include Shouting"), "{shop}");
    assert!(shop.contains("shouts(:handle, :name)"), "{shop}");
    assert!(shop.contains("Name can't be blank"), "{shop}");
    let order = read("app/models/order.rb");
    assert!(order.contains("shouts(:reference, :name)"), "{order}");
    let note = read("app/models/note.rb");
    assert!(note.contains("include Shouting"), "{note}");
    assert!(!note.contains("shouts "), "{note}");
    let holder = read("app/models/roundhouse_initializer_config_initializers_loop_sharding.rb");
    assert!(!holder.contains("on_load(model_name)"), "{holder}");
    assert!(holder.contains("on_load_lifted(\"Shop\""), "{holder}");
    assert!(holder.contains("on_load_lifted(\"Note\""), "{holder}");
}

/// A loop over a name that is no model of the app is not expanded: it
/// runs as written, registered with the ported loader.
#[test]
fn a_loop_naming_a_non_model_stays_a_callback() {
    let initializer = INITIALIZER.replace("\"Note\" => -> { Note },", "\"Note\" => -> { Note },\n    \"CallLog\" => -> { CallLog },");
    let (emitted, errors) =
        overlay().write("config/initializers/loop_sharding.rb", &initializer).emit(roundhouse::project::BuildTarget::Ruby);
    let _ = errors;
    let shop = std::fs::read_to_string(emitted.join("app/models/shop.rb")).unwrap();
    assert!(!shop.contains("include Shouting"), "{shop}");
}

fn overlay_base() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
}

/// Core's own ledger_sharding.rb (the world worktree), over stand-ins
/// for the concerns it includes: every Ledger model gets the bundle and
/// the `model_uuid_attributes` its table lists, written in its body.
#[test]
fn core_ledger_sharding_gives_each_model_its_concern_and_uuid_columns() {
    let path = "/home/tobi/world/trees/allunit-core/src/areas/core/shopify/components/platform/essentials/config/initializers/ledger_sharding.rb";
    let Ok(real) = std::fs::read_to_string(&path) else { return };
    let mut app = overlay_base()
        .write("Gemfile.lock", "GEM\n  remote: https://rubygems.org/\n  specs:\n    concurrent-ruby (1.3.5)\n    i18n (1.15.2)\n      concurrent-ruby (~> 1.0)\n\nDEPENDENCIES\n  i18n\n")
        .write("roundhouse.yml", "database_adapter: postgresql\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("app/models/concerns/model_uuid_columns.rb", "module ModelUuidColumns\n  extend ActiveSupport::Concern\n  class_methods do\n    def model_uuid_attributes(*names)\n      names.each { |n| attribute n.to_sym, :string }\n    end\n  end\nend\n")
        .write("app/models/concerns/generate_oid.rb", "module GenerateOid\n  extend ActiveSupport::Concern\nend\n")
        .write("app/models/concerns/yugabyte_associations.rb", "module YugabyteAssociations\n  extend ActiveSupport::Concern\n  included do\n    class_attribute :declared_yugabyte_query_constraints, instance_accessor: false, default: nil\n  end\n  module ClassMethods\n    def yugabyte_query_constraints(*columns)\n      cols = columns.map(&:to_sym)\n      self.declared_yugabyte_query_constraints = cols\n    end\n  end\nend\n")
        .write("app/models/concerns/ledger_yugabyte_associations.rb", "module LedgerYugabyteAssociations\nend\n")
        .write("app/models/concerns/global_db/shop_island_scoped.rb", "module GlobalDb\n  module ShopIslandScoped\n    extend ActiveSupport::Concern\n  end\nend\n")
        .write("app/models/concerns/podding/database/concerns/sharded_table.rb", "module Podding\n  module Database\n    module Concerns\n      module ShardedTable\n        extend ActiveSupport::Concern\n      end\n    end\n  end\nend\n")
        .write("app/models/concerns/active_record_connection_adapter/record_extension.rb", "module ActiveRecordConnectionAdapter\n  module RecordExtension\n  end\nend\n")
        .write("app/models/ledger/application_record.rb", "module Ledger\n  class ApplicationRecord < ::ApplicationRecord\n    self.abstract_class = true\n  end\nend\n")
        .write("app/models/ledger.rb", "module Ledger\n  def self.table_name_prefix\n    \"ledger_\"\n  end\nend\n")
        .write("config/initializers/ledger_sharding.rb", &real);
    for m in ["Account","Transaction","Entry","Reference","Batch","AccountGroup","AccountGroupsAccount","Report","ReportEntry","FinancialEvent","FinancialTransaction","FinancialTransfer","TransactionGroup","TransactionGroupEntry"] {
        app = app.write(&format!("app/models/ledger/{}.rb", m.to_lowercase()), &format!("module Ledger\n  class {m} < Ledger::ApplicationRecord\n  end\nend\n"));
    }
    let cols = "id bigint, shop_id bigint, parent_id bigint, batch_id bigint, financial_event_id bigint, reversed_by_id bigint, reverses_id bigint, account_id bigint, ledger_transaction_id bigint, entity_id bigint, ledger_entity_id bigint, account_group_id bigint, entry_id bigint, report_id bigint, financial_account_id bigint, financial_transfer_id bigint, financial_transaction_id bigint, transaction_group_id bigint, date date, kind varchar";
    let tables = ["accounts","transactions","entries","references","batches","account_groups","account_groups_accounts","reports","report_entries","financial_events","financial_transactions","financial_transfers","transaction_groups","transaction_group_entries"];
    let sql: String = tables.iter().map(|t| format!("CREATE TABLE ledger_{t} (\n    {}\n);\n", cols.split(", ").collect::<Vec<_>>().join(",\n    "))).collect();
    app = app.write("db/structure.sql", &sql);
    let (emitted, errors) = app.emit(roundhouse::project::BuildTarget::Ruby);
    // The probe's schema has no primary keys, which the PostgreSQL insert
    // identity check names; anything else is a failure.
    let others: Vec<&String> = errors
        .iter()
        .filter(|e| !e.contains("database_adapter not supported") && !e.contains("postgresql_insert_identity"))
        .collect();
    assert!(others.is_empty(), "{}", errors.join("\n"));
    let uuid_columns = [
        ("account", "model_uuid_attributes(:id, :parent_id, :shop_id)"),
        ("transaction", "model_uuid_attributes(:batch_id, :financial_event_id, :id, :reversed_by_id, :reverses_id, :shop_id)"),
        ("report_entry", "model_uuid_attributes(:entry_id, :id, :report_id, :shop_id)"),
        ("batch", "model_uuid_attributes(:id, :shop_id)"),
    ];
    for (file, call) in uuid_columns {
        let text = std::fs::read_to_string(emitted.join(format!("app/models/ledger/{file}.rb"))).unwrap();
        for line in ["include ModelUuidColumns", "include GenerateOid", call] {
            assert!(text.contains(line), "{file}: {line}");
        }
    }
    // The loop was consumed, not registered a second time.
    let holder = std::fs::read_to_string(emitted.join("app/models/roundhouse_initializer_config_initializers_ledger_sharding.rb")).unwrap();
    assert!(!holder.contains("on_load(model_name)"), "{holder}");
}
