# PostgreSQL connection identity and transaction statements. Query and result
# behavior stays in the shared Connection and SqlAdapterQueries framework.
require_relative "active_record_statement_errors"

module ActiveRecord
  module ConnectionAdapters
    class PostgreSQLAdapter < Connection
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

  class Base
    def self.connection
      Thread.current.thread_variable_get(:rh_postgresql_adapter) ||
        Thread.current.thread_variable_set(:rh_postgresql_adapter, ConnectionAdapters::PostgreSQLAdapter.new)
    end
  end
end
