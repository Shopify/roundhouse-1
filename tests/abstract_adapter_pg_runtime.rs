//! Rails' adapter ancestry on the PostgreSQL runtime, against native Rails
//! 8.2 on the same local PostgreSQL database.
//!
//! Shopify Core reopens `ActiveRecord::ConnectionAdapters::AbstractAdapter`
//! (components/platform/essentials/lib/podding/patches/active_record.rb:
//! `attr_writer :abstract_connection_name`, `abstract_connection_name`, and
//! a TransactionConnectionTracker prepended around `transaction`), and its
//! test helper prepends a FollowerReadWriteGuard around
//! `preventing_writes?`. The runtime's adapter was
//! `PostgreSQLAdapter < Connection`, so none of these reached the live
//! connection: every fixture set raised "undefined method
//! 'abstract_connection_name'". The adapter is now
//! `PostgreSQLAdapter < AbstractAdapter`, with the members Rails defines on
//! AbstractAdapter (`transaction`, `preventing_writes?`, `pool`, `role`,
//! `shard`, `adapter_name`, ...) defined there.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE shops (
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
  database: rh_e2e_abstract_adapter
"#;

const INITIALIZER: &str = r#"# frozen_string_literal: true

require "podding/patches/active_record"
require "follower_read_write_guard"
"#;

/// Core's podding/patches/active_record.rb, its ConnectionPool and
/// AbstractAdapter reopens, with Podding::Verifier's
/// `cross_db_transaction.with_transaction` replaced by
/// `ConnectionNameLog.with_transaction`: CrossDbTransaction#with_transaction
/// (app/utils/podding/verifier/cross_db_transaction.rb) verbatim, which
/// holds the outermost transaction's connection name for the block, plus
/// a log of the names it is given.
const PATCH: &str = r##"# typed: true
# frozen_string_literal: true

module ConnectionNameLog
  class << self
    def entries
      @entries ||= []
    end

    def current
      Thread.current[:transaction_connection_name]
    end

    def with_transaction(abstract_connection_name)
      entries << abstract_connection_name
      old_transaction_connection_name = Thread.current[:transaction_connection_name]
      Thread.current[:transaction_connection_name] ||= abstract_connection_name || :primary

      yield
    ensure
      Thread.current[:transaction_connection_name] = old_transaction_connection_name
    end
  end
end

module FollowerReads
  class << self
    attr_accessor :enabled
  end
end

module ActiveRecord
  module ConnectionAdapters
    class ConnectionPool
      def connection_name
        db_config.name.to_sym
      end
    end

    class AbstractAdapter
      attr_writer :abstract_connection_name

      def abstract_connection_name
        @abstract_connection_name || pool.connection_name
      end

      # @requires_ancestor: AbstractAdapter
      module TransactionConnectionTracker
        def transaction(*args, **kwargs)
          args_untyped = args #: as untyped
          super(*args_untyped, **kwargs) do |transaction|
            ConnectionNameLog.with_transaction(abstract_connection_name) do
              yield transaction if block_given?
            end
          end
        end
      end

      prepend AbstractAdapter::TransactionConnectionTracker
    end
  end
end
"##;

/// Core's test helper's FollowerReadWriteGuard shape, prepended to
/// AbstractAdapter after the podding patch: writes are prevented when
/// Rails prevents them or while follower reads are on. (Core prepends it
/// with a top-level `AbstractAdapter.prepend(...)`; here the prepend is
/// the reopen's, so boot loads the file as declarations.)
const GUARD: &str = r##"# frozen_string_literal: true

module FollowerReadWriteGuard
  def preventing_writes?
    return true if super

    FollowerReads.enabled ? true : false
  end
end

module ActiveRecord
  module ConnectionAdapters
    class AbstractAdapter
      prepend ::FollowerReadWriteGuard
    end
  end
end
"##;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

const SHOP: &str = "class Shop < ApplicationRecord\nend\n";

const PROBE: &str = r##"class AbstractAdapterProbe
  def self.lines
    out = []
    abstract = ActiveRecord::ConnectionAdapters::AbstractAdapter
    conn = ActiveRecord::Base.connection
    out << "class: #{conn.class.name}"
    out << "superclass: #{conn.class.superclass.name}"
    out << "is_a: #{conn.is_a?(abstract)}"
    out << "adapter_name: #{conn.adapter_name}"
    out << "prepended: #{abstract.ancestors.take_while { |m| m != abstract }.map(&:name).inspect}"
    out << "transaction owner: #{conn.method(:transaction).owner.name}"
    out << "preventing_writes? owner: #{conn.method(:preventing_writes?).owner.name}"
    out << "abstract_connection_name: #{conn.abstract_connection_name.inspect}"
    conn.abstract_connection_name = :pinned_name
    out << "assigned: #{conn.abstract_connection_name.inspect}"
    conn.abstract_connection_name = nil
    out << "role/shard: #{[conn.role, conn.shard].inspect}"
    out << "respond_to set_callback: #{abstract.respond_to?(:set_callback)}"

    # The names the tracker hands the verifier, and the name the
    # verifier holds while the block runs. (Not the number of calls:
    # this runtime's record save enters `transaction` twice, its `save`
    # and its `save_after_validation` seam each wrapping, where Rails'
    # save enters it once; joinable, so the statements are the same.)
    ConnectionNameLog.entries.clear
    inside = nil
    Shop.transaction do
      Shop.create!(name: "one")
      inside = ConnectionNameLog.current
    end
    out << "Base.transaction + create!: #{ConnectionNameLog.entries.uniq.inspect} #{inside.inspect} #{ConnectionNameLog.current.inspect}"
    ConnectionNameLog.entries.clear
    conn.transaction do
      conn.abstract_connection_name = :pinned_name
      conn.transaction(requires_new: true) do
        Shop.create!(name: "two")
        inside = ConnectionNameLog.current
      end
      conn.abstract_connection_name = nil
    end
    out << "nested: #{ConnectionNameLog.entries.uniq.inspect} #{inside.inspect}"
    ConnectionNameLog.entries.clear
    conn.abstract_connection_name = :pinned_name
    conn.transaction { inside = ConnectionNameLog.current }
    conn.abstract_connection_name = nil
    out << "assigned name: #{ConnectionNameLog.entries.uniq.inspect} #{inside.inspect}"
    ConnectionNameLog.entries.clear
    begin
      Shop.transaction do
        Shop.create!(name: "three")
        raise ActiveRecord::Rollback
      end
    end
    out << "rolled back: #{ConnectionNameLog.entries.uniq.inspect} #{Shop.where(name: "three").count}"

    out << "preventing_writes?: #{conn.preventing_writes?}"
    out << "while_preventing_writes: #{ActiveRecord::Base.while_preventing_writes { conn.preventing_writes? }}"
    FollowerReads.enabled = true
    out << "follower reads: #{conn.preventing_writes?}"
    begin
      Shop.create!(name: "four")
      out << "follower write: allowed"
    rescue ActiveRecord::ReadOnlyError => e
      out << "follower write: #{e.class.name}"
    ensure
      FollowerReads.enabled = false
    end
    out << "shops: #{Shop.order(:id).pluck(:name).inspect}"
    out
  end
end
"##;

/// What native Rails 8.2 answers (checked on the task PostgreSQL), so two
/// identical wrong answers still fail.
const EXPECTED: [&str; 20] = [
    "class: ActiveRecord::ConnectionAdapters::PostgreSQLAdapter",
    "superclass: ActiveRecord::ConnectionAdapters::AbstractAdapter",
    "is_a: true",
    "adapter_name: PostgreSQL",
    r#"prepended: ["FollowerReadWriteGuard", "ActiveRecord::ConnectionAdapters::AbstractAdapter::TransactionConnectionTracker"]"#,
    "transaction owner: ActiveRecord::ConnectionAdapters::AbstractAdapter::TransactionConnectionTracker",
    "preventing_writes? owner: FollowerReadWriteGuard",
    "abstract_connection_name: :primary",
    "assigned: :pinned_name",
    "role/shard: [:writing, :default]",
    "respond_to set_callback: true",
    "Base.transaction + create!: [:primary] :primary nil",
    "nested: [:primary, :pinned_name] :primary",
    "assigned name: [:pinned_name] :pinned_name",
    "rolled back: [:primary] 0",
    "preventing_writes?: false",
    "while_preventing_writes: true",
    "follower reads: true",
    "follower write: ActiveRecord::ReadOnlyError",
    r#"shops: ["one", "two"]"#,
];

const FILES: [(&str, &str); 10] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("config/initializers/podding.rb", INITIALIZER),
    ("lib/podding/patches/active_record.rb", PATCH),
    ("lib/follower_read_write_guard.rb", GUARD),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/shop.rb", SHOP),
    ("app/models/abstract_adapter_probe.rb", PROBE),
];

/// What native Rails requires, in order: the initializer first, as Rails
/// runs initializers before eager loading the models.
const LOADS: [&str; 4] = [
    "config/initializers/podding",
    "app/models/application_record",
    "app/models/shop",
    "app/models/abstract_adapter_probe",
];

fn overlay() -> emit_and_run::Overlay {
    FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

/// The diagnostics besides the PostgreSQL adapter notice and the
/// tracker's `super(*args_untyped, **kwargs)` warning: the analyzer does
/// not verify a keyword splat into a `super` that leaves the app for the
/// runtime's AbstractAdapter#transaction (Core's emit warns at
/// podding/patches/active_record.rb:26 the same way). The Ruby tree writes
/// that `super` as written (asserted below), so Ruby binds it natively.
fn real_errors(errors: &[String]) -> Vec<&String> {
    errors
        .iter()
        .filter(|e| !e.contains("database_adapter not supported"))
        .filter(|e| !e.contains("keyword splat in ordinary super not supported"))
        .collect()
}

#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn core_reopens_of_abstract_adapter_reach_the_live_postgresql_connection() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "abstract-adapter",
        &FILES,
        &emitted,
        "rh_e2e_abstract_adapter",
        "AbstractAdapterProbe",
        &LOADS,
    );
    assert_eq!(native, EXPECTED, "native");
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    assert_eq!(emitted_lines, native, "emitted");
}

/// The emitted runtime declares Rails' ancestry: AbstractAdapter holds
/// the abstract surface (and refuses the checkout/checkin callbacks this
/// pool does not run, by name), PostgreSQLAdapter inherits from it.
#[test]
fn the_postgresql_adapter_inherits_from_abstract_adapter_in_the_emitted_tree() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let adapter = std::fs::read_to_string(emitted.join("runtime/active_record_postgresql_adapter.rb"))
        .expect("runtime/active_record_postgresql_adapter.rb");
    assert!(adapter.contains("class AbstractAdapter < Connection"), "{adapter}");
    assert!(adapter.contains("class PostgreSQLAdapter < AbstractAdapter"), "{adapter}");
    assert!(adapter.contains("ADAPTER_NAME = \"PostgreSQL\""), "{adapter}");
    assert!(adapter.contains("set_callback(") && adapter.contains("framework_feature_not_ported"), "{adapter}");
    let transactions = std::fs::read_to_string(emitted.join("runtime/active_record_transactions.rb"))
        .expect("runtime/active_record_transactions.rb");
    let abstract_at = transactions.find("class AbstractAdapter\n").expect("AbstractAdapter reopen");
    let transaction_at = transactions.find("def transaction(").expect("def transaction");
    let pg_at = transactions.find("class PostgreSQLAdapter\n").expect("PostgreSQLAdapter reopen");
    assert!(abstract_at < transaction_at && transaction_at < pg_at, "`transaction` is AbstractAdapter's");
    // The tracker's keyword splat into the runtime's `transaction`, which
    // the analyzer warns of (`real_errors`), is written as written.
    let reopen = std::fs::read_to_string(emitted.join("app/models/active_record/connection_adapters/abstract_adapter.rb"))
        .expect("the AbstractAdapter reopen");
    assert!(reopen.contains("prepend AbstractAdapter::TransactionConnectionTracker"), "{reopen}");
    assert!(reopen.contains("super(*args_untyped, **kwargs) do |transaction|"), "{reopen}");
}
