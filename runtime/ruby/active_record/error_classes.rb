# frozen_string_literal: true
# The rest of Rails' Active Record exception hierarchy (activerecord
# 8.2.0.alpha errors.rb and associations/errors.rb), on the Ruby-family
# trees: each class under the superclass Rails gives it, with Rails'
# constructor and readers. The roots every target shares
# (ActiveRecordError, AdapterError, StatementInvalid and the
# constraint classes the strict targets' `Db`s raise) are in errors.rb.
#
# Ruby-family home, off the strict-target transpile tables: apps name
# these in `rescue` clauses, and a rescue clause evaluates its class list
# whenever an exception passes through it, so an undefined name turns
# every OTHER error into a NameError that hides the real one.
#
# Constructors that hand `connection_pool:`, `sql:` or `binds:` up to
# AdapterError/StatementInvalid (ConnectionNotEstablished,
# MismatchedForeignKey, NoDatabaseError) are the CRuby overlay's
# (ruby_overlay/runtime/active_record_statement_errors.rb), with the
# parents' own: the shared parents here take a message only.
module ActiveRecord
  # Rails: `attr_reader :connection_pool` on AdapterError, set by its
  # constructor (the CRuby overlay's) or `set_pool`.
  class AdapterError
    attr_reader :connection_pool
  end

  class SubclassNotFound < ActiveRecordError
  end

  class AssociationTypeMismatch < ActiveRecordError
  end

  class SerializationTypeMismatch < ActiveRecordError
  end

  class AdapterNotSpecified < ActiveRecordError
  end

  class TableNotSpecified < ActiveRecordError
  end

  class AdapterNotFound < ActiveRecordError
  end

  # Named by `rescue_from ActiveRecord::ConnectionNotEstablished`
  # (lobsters answers it with a plain-text 500); the PostgreSQL tree's
  # `Db` raises it for a connection it cannot open (db_pg_cruby.rb).
  class ConnectionNotEstablished < AdapterError
    def set_pool(connection_pool)
      unless @connection_pool
        @connection_pool = connection_pool
      end

      self
    end
  end

  class ConnectionTimeoutError < ConnectionNotEstablished
  end

  class ConnectionNotDefined < ConnectionNotEstablished
    def initialize(message = nil, connection_name: nil, role: nil, shard: nil)
      super(message)
      @connection_name = connection_name
      @role = role
      @shard = shard
    end

    attr_reader :connection_name, :role, :shard
  end

  class DatabaseConnectionError < ConnectionNotEstablished
    def initialize(message = nil)
      super(message || "Database connection error")
    end

    def self.hostname_error(hostname)
      DatabaseConnectionError.new(<<~MSG)
        There is an issue connecting with your hostname: #{hostname}.\n
        Please check your database configuration and ensure there is a valid connection to your database.
      MSG
    end

    def self.username_error(username)
      DatabaseConnectionError.new(<<~MSG)
        There is an issue connecting to your database with your username/password, username: #{username}.\n
        Please check your database configuration to ensure the username/password are valid.
      MSG
    end
  end

  class ExclusiveConnectionTimeoutError < ConnectionTimeoutError
  end

  class ReadOnlyError < ActiveRecordError
  end

  # Rails: subclasses ArgumentError for backwards compatibility.
  class ShardSwapProhibitedError < ArgumentError
  end

  class RecordNotDestroyed < ActiveRecordError
    attr_reader :record

    def initialize(message = nil, record = nil)
      @record = record
      super(message)
    end
  end

  class InvalidForeignKey < WrappedDatabaseException
  end

  # Rails' constructor (keyword-only, building its message from the
  # column metadata) and `set_query` are the CRuby overlay's.
  class MismatchedForeignKey < StatementInvalid
  end

  class NotNullViolation < StatementInvalid
  end

  class CheckViolation < StatementInvalid
  end

  class ExclusionViolation < StatementInvalid
  end

  class RangeError < StatementInvalid
  end

  # Rails' constructor `(message = nil, code = nil, level = nil, sql =
  # nil, connection_pool = nil)` is the CRuby overlay's.
  # Rails passes the pool to AdapterError's keyword; the shared parent
  # takes a message only, so the pool is stored here, as that keyword
  # would store it.
  class SQLWarning < AdapterError
    attr_reader :code, :level
    attr_accessor :sql

    def initialize(message = nil, code = nil, level = nil, sql = nil, connection_pool = nil)
      super(message)
      @connection_pool = connection_pool
      @code = code
      @level = level
      @sql = sql
    end
  end

  class PreparedStatementInvalid < ActiveRecordError
  end

  # Rails: a StatementInvalid, so a missing database never marks a
  # semian circuit. Its constructor (default message "Database not
  # found", `connection_pool:`) is the CRuby overlay's. Not modeled:
  # ActiveSupport::ActionableError's "Create database" action.
  class NoDatabaseError < StatementInvalid
    def self.db_error(db_name)
      NoDatabaseError.new(<<~MSG)
        Database not found: #{db_name}. Available database configurations can be found in config/database.yml.

        To resolve this error:

        - Create the database by running:

            bin/rails db:create

        - Verify that config/database.yml contains the correct database name.
      MSG
    end
  end

  class DatabaseAlreadyExists < StatementInvalid
  end

  class PreparedStatementCacheExpired < StatementInvalid
  end

  class StaleObjectError < ActiveRecordError
    attr_reader :record, :attempted_action

    def initialize(record = nil, attempted_action = nil)
      if record && attempted_action
        @record = record
        @attempted_action = attempted_action
        super("Attempted to #{attempted_action} a stale object: #{record.class.name}.")
      else
        super("Stale object error.")
      end
    end
  end

  class ConfigurationError < ActiveRecordError
  end

  class StrictLoadingViolationError < ActiveRecordError
  end

  # `transaction` swallows it after rolling back (runtime/spinel/cruby/
  # active_record_transactions.rb on the PostgreSQL tree).
  class Rollback < ActiveRecordError
  end

  class DangerousAttributeError < ActiveRecordError
  end

  class AttributeAssignmentError < ActiveRecordError
    attr_reader :exception, :attribute

    def initialize(message = nil, exception = nil, attribute = nil)
      super(message)
      @exception = exception
      @attribute = attribute
    end
  end

  # Rails' constructor does not call `super`, so the message is the
  # class name.
  class MultiparameterAssignmentErrors < ActiveRecordError
    attr_reader :errors

    def initialize(errors = nil)
      @errors = errors
    end
  end

  class UnknownPrimaryKey < ActiveRecordError
    attr_reader :model

    def initialize(model = nil, description = nil)
      if model
        message = "Unknown primary key for table #{model.table_name} in model #{model}."
        message += "\n#{description}" if description
        @model = model
        super(message)
      else
        super("Unknown primary key.")
      end
    end
  end

  class UnmodifiableRelation < ActiveRecordError
  end

  class TransactionIsolationError < ActiveRecordError
  end

  class TransactionRollbackError < StatementInvalid
  end

  class AsynchronousQueryInsideTransactionError < ActiveRecordError
  end

  class SerializationFailure < TransactionRollbackError
  end

  class Deadlocked < TransactionRollbackError
  end

  class MissingRequiredOrderError < ActiveRecordError
  end

  class IrreversibleOrderError < ActiveRecordError
  end

  # Superclass for errors that have been aborted (either by client or
  # server). The PostgreSQL tree's `Db` raises the four below as Rails'
  # PostgreSQLAdapter does (db_pg_cruby.rb `translate`); semian's
  # adapter counts AdapterTimeout and ConnectionFailed against the
  # circuit.
  class QueryAborted < StatementInvalid
  end

  class LockWaitTimeout < StatementInvalid
  end

  class StatementTimeout < QueryAborted
  end

  class QueryCanceled < QueryAborted
  end

  class AdapterTimeout < QueryAborted
  end

  class ConnectionFailed < QueryAborted
  end

  class UnknownAttributeReference < ActiveRecordError
  end

  class DatabaseVersionError < ActiveRecordError
  end

  class DeprecatedAssociationError < ActiveRecordError
  end

  # associations/errors.rb. Of that file's reflection errors only this
  # one is ported: the others are raised from reflection internals this
  # runtime does not model. Not ported: the DidYouMean `corrections`.
  class AssociationNotFoundError < ConfigurationError
    attr_reader :record, :association_name

    def initialize(record = nil, association_name = nil)
      @record           = record
      @association_name = association_name
      if record && association_name
        super("Association named '#{association_name}' was not found on #{record.class.name}; perhaps you misspelled it?")
      else
        super("Association was not found.")
      end
    end
  end
end

# activemodel 8.2.0.alpha errors.rb: what a strict validation
# (`validates ..., strict: true`, `validates!`) raises with the error's
# full message, from `Errors#add`. The validations lowering writes the
# raise (`lower::model_to_library::validations`).
module ActiveModel
  class StrictValidationFailed < StandardError
  end
end
