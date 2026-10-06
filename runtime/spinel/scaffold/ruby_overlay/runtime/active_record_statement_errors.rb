# Shared Ruby driver exception API (SQL, binds and constraint subclasses).
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

  # Rails' connection-failure names, raised by the PostgreSQL tree's `Db`
  # (`translate` and `new_client`, runtime/spinel/db_pg_cruby.rb) as
  # Rails' PostgreSQLAdapter raises them; semian's adapter counts
  # AdapterTimeout, ConnectionFailed and ConnectionNotEstablished against
  # the circuit.
  class QueryAborted < StatementInvalid
  end

  class AdapterTimeout < QueryAborted
  end

  class ConnectionFailed < QueryAborted
  end

  # Rails: a StatementInvalid, so a missing database never marks a
  # semian circuit. Not modeled: ActiveSupport::ActionableError's
  # "Create database" action.
  class NoDatabaseError < StatementInvalid
    def initialize(message = nil, sql: nil, binds: nil)
      super(message || "Database not found", sql: sql, binds: binds)
    end

    class << self
      def db_error(db_name)
        NoDatabaseError.new(<<~MSG)
          Database not found: #{db_name}. Available database configurations can be found in config/database.yml.

          To resolve this error:

          - Create the database by running:

              bin/rails db:create

          - Verify that config/database.yml contains the correct database name.
        MSG
      end
    end
  end

  class DatabaseConnectionError < ConnectionNotEstablished
    def initialize(message = nil)
      super(message || "Database connection error")
    end

    class << self
      def hostname_error(hostname)
        DatabaseConnectionError.new(<<~MSG)
          There is an issue connecting with your hostname: #{hostname}.\n
          Please check your database configuration and ensure there is a valid connection to your database.
        MSG
      end

      def username_error(username)
        DatabaseConnectionError.new(<<~MSG)
          There is an issue connecting to your database with your username/password, username: #{username}.\n
          Please check your database configuration to ensure the username/password are valid.
        MSG
      end
    end
  end

end
