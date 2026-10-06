# PostgreSQL connection identity and transaction statements. Query and result
# behavior stays in the shared Connection and SqlAdapterQueries framework.
#
# The adapter is Rails' unit of connection: `Db` builds one per pooled
# PostgreSQL session (`Db.adapter_class`) and drives Rails' lifecycle on
# it (Db::ConnectionLifecycle: connect!, verify!, reconnect!, active?,
# raw_execute), so a module prepended to this class (semian's
# ActiveRecordPostgreSQLAdapter) wraps every connect, statement and ping
# of the tree. `Base.connection` is the adapter leased to the current
# thread/fiber, as Rails' lease_connection; before the database is
# configured it is a per-thread adapter without a session.
require_relative "active_record_statement_errors"

module ActiveRecord
  module ConnectionAdapters
    class PostgreSQLAdapter < Connection
      include Db::ConnectionLifecycle

      def adapter_name
        "PostgreSQL"
      end

      def quote(value)
        return Db.escape_bool(value) if value == true || value == false
        super(value)
      end

      def begin_db_transaction
        Db.exec("BEGIN")
        nil
      end

      def begin_isolated_db_transaction(isolation)
        level = case isolation
        when :read_uncommitted then "READ UNCOMMITTED"
        when :read_committed then "READ COMMITTED"
        when :repeatable_read then "REPEATABLE READ"
        when :serializable then "SERIALIZABLE"
        else raise TransactionIsolationError, "unknown PostgreSQL isolation level: #{isolation}"
        end
        Db.exec("BEGIN ISOLATION LEVEL #{level}")
        nil
      end

      def commit_db_transaction
        Db.exec("COMMIT")
        nil
      end

      def exec_rollback_db_transaction
        Db.exec("ROLLBACK")
        nil
      end
    end
  end

  Db.adapter_class = ConnectionAdapters::PostgreSQLAdapter

  class Base
    def self.connection
      return Db.current_adapter if Db.configured?
      Thread.current.thread_variable_get(:rh_postgresql_adapter) ||
        Thread.current.thread_variable_set(:rh_postgresql_adapter, ConnectionAdapters::PostgreSQLAdapter.new({}))
    end
  end
end
