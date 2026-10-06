# CRuby-only: `ActiveRecord::Base.connection` as Rails' SQLite3 adapter
# class, with its transaction statements.
#
# Rails hands back `ConnectionAdapters::SQLite3Adapter` from
# `Base.connection`, and code that adjusts the adapter (Core's
# sqlite_test_compat prepends a module onto that class) needs the class
# to be the one the connection really is. The shared runtime's
# `Connection` facade stays the parent, so every raw-SQL member it
# already has is unchanged.
#
# The transaction members are Rails' own (activerecord
# connection_adapters/sqlite3/database_statements.rb at 5813559f):
# BEGIN DEFERRED/IMMEDIATE TRANSACTION, COMMIT/ROLLBACK TRANSACTION, the
# isolation checks of `internal_begin_transaction`, and the
# `read_uncommitted` PRAGMA of `reset_isolation_level`. A driver error
# from these statements surfaces as `StatementInvalid` with the driver's
# exception as its `cause`, which is what Rails' `translate_exception`
# answers for anything it does not map to a narrower class.
#
# CRuby only, deliberately: `SQLite3::SQLException` (the cause) is the
# `sqlite3` gem's class, which this tree's `Db` is built on. The check
# refuses these constants on every other target.
#
# NOT modeled (ledgered): `execute` and the rest of the facade do not
# translate driver errors; the translated subclasses (NotNullViolation,
# InvalidForeignKey, CheckViolation, StatementTimeout) are not separate
# classes; `RecordNotUnique` and this `StatementInvalid` hang off
# StandardError, not Rails' `ActiveRecordError` hierarchy; there is no
# shared-cache mode, so an isolation level never reaches its PRAGMA.
module ActiveRecord
  # Rails: `StatementInvalid < AdapterError < ActiveRecordError`.
  class StatementInvalid < StandardError
    attr_reader :sql, :binds

    def initialize(message = nil, sql: nil, binds: nil)
      super(message || $!&.message)
      @sql = sql
      @binds = binds
    end
  end

  # Rails: `TransactionIsolationError < ActiveRecordError`.
  class TransactionIsolationError < StandardError
  end

  module ConnectionAdapters
    class SQLite3Adapter < Connection
      def begin_deferred_transaction(isolation = nil)
        internal_begin_transaction(:deferred, isolation)
      end

      def begin_isolated_db_transaction(isolation)
        internal_begin_transaction(:deferred, isolation)
      end

      def begin_db_transaction
        internal_begin_transaction(:immediate, nil)
      end

      def commit_db_transaction
        transaction_statement("COMMIT TRANSACTION")
      end

      def exec_rollback_db_transaction
        transaction_statement("ROLLBACK TRANSACTION")
      end

      def reset_isolation_level
        transaction_statement("PRAGMA read_uncommitted=#{@previous_read_uncommitted}")
        @previous_read_uncommitted = nil
      end

      private

      # Rails asks `shared_cache?` (the SQLITE_OPEN_SHAREDCACHE flag);
      # this tree's `Db` never opens with it, so an isolation level other
      # than none always raises, as it does on such a Rails connection.
      def internal_begin_transaction(mode, isolation)
        if isolation
          raise TransactionIsolationError, "SQLite3 only supports the `read_uncommitted` transaction isolation level" if isolation != :read_uncommitted
          raise StandardError, "You need to enable the shared-cache mode in SQLite mode before attempting to change the transaction isolation level"
        end
        transaction_statement("BEGIN #{mode} TRANSACTION")
      end

      # Rails' `translate_exception`: a RuntimeError or an Active Record
      # error passes through, anything else becomes StatementInvalid
      # named "<driver class>: <message>". Raised inside the rescue, so
      # the driver's exception is its `cause`.
      def transaction_statement(sql)
        Db.exec(sql)
        nil
      rescue RuntimeError, RecordNotUnique, StatementInvalid
        raise
      rescue => e
        raise StatementInvalid.new("#{e.class.name}: #{e.message}", sql: sql)
      end
    end
  end

  class Base
    # Rails' connection is one adapter per thread, the same object on
    # every call; `@previous_read_uncommitted` lives on it.
    def self.connection
      Thread.current[:rh_sqlite3_adapter] ||= ConnectionAdapters::SQLite3Adapter.new
    end
  end
end
