# CRuby-only: `ActiveRecord::Base.connection` as Rails' SQLite3 adapter
# class, with its transaction statements, and the statement errors this
# tree's `Db` raises.
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
# `read_uncommitted` PRAGMA of `reset_isolation_level`.
#
# WHERE EACH PART LIVES. The class names and their parents are portable
# and live in the shared `runtime/ruby/active_record/errors.rb`
# (`StatementInvalid`, with `RecordNotUnique` and `ValueTooLong` under
# it). Turning a driver error into one of them is the driver's
# boundary: the "<driver class>: <message>" text and the `cause` are
# the driver's, so it is `Db.translate` in `runtime/spinel/db_cruby.rb`,
# under every statement `Db` runs. Only this tree has that half, so the
# Rails statement API (`sql`, `binds`), the subclasses only it raises,
# and the adapter class are here, and `project` refuses their constants
# and the adapter's members on every other target. The per-thread
# connection needs Thread, which is why it is not in the shared
# `connection.rb` either.
#
# NOT modeled (ledgered): the Active Record error hierarchy above
# StatementInvalid (ActiveRecordError, AdapterError,
# WrappedDatabaseException, QueryAborted, StatementTimeout,
# ConnectionNotEstablished); a connection of its own per thread (every
# thread's adapter runs on `Db`'s shared handle outside a lease); a
# shared-cache mode, so an isolation level never reaches its PRAGMA;
# `transaction` options (refused at emit).
module ActiveRecord
  # Rails: `StatementInvalid < AdapterError < ActiveRecordError`, built
  # with the statement and its binds; the message defaults to the error
  # being handled.
  class StatementInvalid
    attr_reader :sql, :binds

    def initialize(message = nil, sql: nil, binds: nil)
      super(message || $!&.message)
      @sql = sql
      @binds = binds
    end
  end

  # Rails defines no constructor of their own on these.
  class RecordNotUnique
    def initialize(message = nil, sql: nil, binds: nil)
      super
    end
  end

  class ValueTooLong
    def initialize(message = nil, sql: nil, binds: nil)
      super
    end
  end

  class NotNullViolation < StatementInvalid
  end

  class InvalidForeignKey < StatementInvalid
  end

  class CheckViolation < StatementInvalid
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
        Db.exec("COMMIT TRANSACTION")
        nil
      end

      def exec_rollback_db_transaction
        Db.exec("ROLLBACK TRANSACTION")
        nil
      end

      def reset_isolation_level
        Db.exec("PRAGMA read_uncommitted=#{@previous_read_uncommitted}")
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
        Db.exec("BEGIN #{mode} TRANSACTION")
        nil
      end
    end
  end

  class Base
    # Rails' connection is one adapter per thread (ActiveSupport's
    # isolation level is the thread by default), the same object on
    # every call and in every fiber of the thread;
    # `@previous_read_uncommitted` lives on it.
    def self.connection
      Thread.current.thread_variable_get(:rh_sqlite3_adapter) ||
        Thread.current.thread_variable_set(:rh_sqlite3_adapter, ConnectionAdapters::SQLite3Adapter.new)
    end
  end
end
