# PostgreSQL connection identity and transaction statements. Query and result
# behavior stays in the shared Connection and SqlAdapterQueries framework.
#
# The adapter is Rails' unit of connection: `Db` builds one per pooled
# PostgreSQL session (`Db.adapter_class`) and drives Rails' lifecycle on
# it (Db::ConnectionLifecycle: connect!, verify!, reconnect!, active?,
# raw_execute), so a module prepended to this class (semian's
# ActiveRecordPostgreSQLAdapter) wraps every connect, statement and ping
# of the tree. `Base.connection` is the adapter its class's pool leases
# to the current thread/fiber, as Rails' lease_connection
# (runtime/active_record_connection_handling.rb); before the database is
# configured it is a per-thread adapter without a session.
#
# Rails' ancestry: PostgreSQLAdapter < AbstractAdapter, so a reopen of
# AbstractAdapter (Core's podding patch: `abstract_connection_name`,
# `allow_preconnect`) reaches the live connection, and a module prepended
# to AbstractAdapter (Core's TransactionConnectionTracker around
# `transaction`, the test helper's FollowerReadWriteGuard around
# `preventing_writes?`) wraps what Rails defines there. So the members
# Rails defines on AbstractAdapter and its modules (DatabaseStatements,
# Savepoints) live on AbstractAdapter here too (this file, and the
# AbstractAdapter reopens of runtime/active_record_transactions.rb and
# runtime/active_record_connection_handling.rb); PostgreSQLAdapter keeps
# what Rails' PostgreSQLAdapter and its PostgreSQL:: modules override.
# The shared `Connection` facade stays the parent of AbstractAdapter, so
# every raw-SQL member it already has is unchanged.
#
# NOT modeled: the lifecycle module (Db::ConnectionLifecycle) mixes Rails'
# abstract members (connect!, verify!, reconnect!, disconnect!) with the
# PostgreSQL overrides (active?, reconnect, configure_connection) at
# AbstractAdapter's level, so a module prepended to AbstractAdapter wraps
# a PostgreSQL override Rails' PostgreSQLAdapter would shadow. The
# adapter's checkout/checkin callbacks (`define_callbacks :checkout,
# :checkin`) are not run by this runtime's pool; `set_callback` and
# `skip_callback` on the adapter refuse by name (below).
require_relative "active_record_statement_errors"

module ActiveRecord
  module ConnectionAdapters
    class AbstractAdapter < Connection
      ADAPTER_NAME = "Abstract"

      include Db::ConnectionLifecycle

      # abstract_adapter.rb:46, `include ActiveSupport::Callbacks` and
      # `define_callbacks :checkout, :checkin`: Rails runs the chains when
      # its pool checks a connection out (`clean!`) and in (`expire`). This
      # runtime's pool (Db.checkout / Db.checkin_to) runs neither, so a
      # callback set on them would never run: refused by name instead.
      class << self
        def set_callback(name, *filter_list, &block)
          raise NotImplementedError, "roundhouse: error[framework_feature_not_ported]: " \
            "#{self.name}.set_callback(#{name.inspect}, ...): the adapter's checkout/checkin callbacks " \
            "(AbstractAdapter `define_callbacks :checkout, :checkin`) are not run by this runtime's connection pool"
        end

        def skip_callback(name, *filter_list, &block)
          raise NotImplementedError, "roundhouse: error[framework_feature_not_ported]: " \
            "#{self.name}.skip_callback(#{name.inspect}, ...): the adapter's checkout/checkin callbacks " \
            "(AbstractAdapter `define_callbacks :checkout, :checkin`) are not run by this runtime's connection pool"
        end
      end

      # Returns the human-readable name of the adapter (Rails:
      # `self.class::ADAPTER_NAME`).
      def adapter_name
        self.class::ADAPTER_NAME
      end

      # AbstractAdapter's feature defaults the runtime reads; the
      # PostgreSQL adapter overrides them as Rails' does.
      def supports_savepoints?
        false
      end

      def supports_lazy_transactions?
        false
      end

      def supports_restart_db_transaction?
        false
      end

      # abstract/database_statements.rb query_command: a statement without
      # rows on this adapter's session, recorded and instrumented as every
      # statement is.
      def query_command(sql, name = nil, materialize_transactions: true) # :nodoc:
        Db.record_query(sql)
        Db.context[:cache]&.clear
        result = raw_execute(sql, name, [], materialize_transactions: materialize_transactions) { |conn| perform_query(conn, sql) }
        nil
      ensure
        result&.clear
      end
    end

    class PostgreSQLAdapter < AbstractAdapter
      ADAPTER_NAME = "PostgreSQL"

      # activerecord 8.1.4 postgresql_adapter.rb:105,
      # `class_attribute :create_unlogged_tables, default: false`: class
      # reader/writer/predicate inherited by subclasses until one assigns,
      # and the instance reader/predicate. Rails reads it in
      # PostgreSQL::TableDefinition#initialize (schema_definitions.rb:241)
      # for `create_table`. This runtime creates tables only from the
      # compiled schema (Schema.statements), never through create_table,
      # so the value is held and readable but changes no DDL. The
      # per-instance writer is not ported.
      class << self
        attr_writer :create_unlogged_tables

        def create_unlogged_tables
          return @create_unlogged_tables if instance_variable_defined?(:@create_unlogged_tables)
          superclass.respond_to?(:create_unlogged_tables) ? superclass.create_unlogged_tables : false
        end

        def create_unlogged_tables?
          !!create_unlogged_tables
        end
      end

      def create_unlogged_tables
        self.class.create_unlogged_tables
      end

      def create_unlogged_tables?
        !!create_unlogged_tables
      end

      def quote(value)
        return Db.escape_bool(value) if value == true || value == false
        super(value)
      end

      # postgresql/database_statements.rb: the transaction statements, on
      # this adapter's own session, named "TRANSACTION". BEGIN does not
      # materialize the lazy transactions it is materializing; COMMIT and
      # ROLLBACK do (Rails' `materialize_transactions: true`).
      def begin_db_transaction # :nodoc:
        query_command("BEGIN", "TRANSACTION", materialize_transactions: false)
      end

      def begin_isolated_db_transaction(isolation) # :nodoc:
        query_command("BEGIN ISOLATION LEVEL #{transaction_isolation_levels.fetch(isolation)}", "TRANSACTION", materialize_transactions: false)
      end

      def commit_db_transaction # :nodoc:
        query_command("COMMIT", "TRANSACTION", materialize_transactions: true)
      end

      def exec_rollback_db_transaction # :nodoc:
        cancel_any_running_query
        query_command("ROLLBACK", "TRANSACTION", materialize_transactions: true)
      end

      def exec_restart_db_transaction # :nodoc:
        cancel_any_running_query
        query_command("ROLLBACK AND CHAIN", "TRANSACTION", materialize_transactions: true)
      end

      private
        IDLE_TRANSACTION_STATUSES = [PG::PQTRANS_IDLE, PG::PQTRANS_INTRANS, PG::PQTRANS_INERROR].freeze
        private_constant :IDLE_TRANSACTION_STATUSES

        def cancel_any_running_query
          return if @raw_connection.nil? || IDLE_TRANSACTION_STATUSES.include?(@raw_connection.transaction_status)

          # pg < 1.6.0 cannot obtain the backend key under libpq >= 18.
          if !(PG.library_version >= 18_00_00 && Gem::Version.new(PG::VERSION) < Gem::Version.new("1.6.0"))
            @raw_connection.cancel
          end
          @raw_connection.block
        rescue PG::Error
        end
      public
    end
  end

  Db.adapter_class = ConnectionAdapters::PostgreSQLAdapter
end

# Base.connection, connection_pool, connects_to, connected_to, ...:
# Rails' connection handling, one pool per configuration.
require_relative "active_record_connection_handling"
