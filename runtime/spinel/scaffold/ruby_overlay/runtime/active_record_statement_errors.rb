# frozen_string_literal: true
# Rails' adapter-error constructors on the CRuby tree (activerecord
# 8.2.0.alpha errors.rb), loaded by the PostgreSQL and SQLite adapter
# overlays. The classes are the shared runtime's: the roots in
# runtime/ruby/active_record/errors.rb (also transpiled for the strict
# targets, so message-only there) and the rest of the hierarchy in
# error_classes.rb. This file gives them Rails' constructors, which carry
# the connection pool, the statement and its binds.
module ActiveRecord
  # Rails defines no constructor on ActiveRecordError, and none on the
  # StatementInvalid subclasses below: a nil message is the class name,
  # and the subclasses take StatementInvalid's keywords. The shared
  # file's message-only constructors are for the strict targets.
  [ActiveRecordError, WrappedDatabaseException, RecordNotUnique, ValueTooLong].each do |klass|
    klass.send(:remove_method, :initialize) if klass.private_instance_methods(false).include?(:initialize)
  end

  class AdapterError
    def initialize(message = nil, connection_pool: nil)
      @connection_pool = connection_pool
      super(message)
    end
  end

  class ConnectionNotEstablished
    def initialize(message = nil, connection_pool: nil)
      super(message, connection_pool: connection_pool)
    end
  end

  class RecordNotFound
    attr_reader :model, :primary_key, :id

    def initialize(message = nil, model = nil, primary_key = nil, id = nil)
      @primary_key = primary_key
      @model = model
      @id = id

      super(message)
    end
  end

  # Wraps the underlying database error as +cause+; the message defaults
  # to the error being handled.
  class StatementInvalid
    def initialize(message = nil, sql: nil, binds: nil, connection_pool: nil)
      super(message || $!&.message, connection_pool: connection_pool)
      @sql = sql
      @binds = binds
    end

    attr_reader :sql, :binds

    def set_query(sql, binds)
      unless @sql
        @sql = sql
        @binds = binds
      end

      self
    end
  end

  class MismatchedForeignKey
    def initialize(
      message: nil,
      sql: nil,
      binds: nil,
      table: nil,
      foreign_key: nil,
      target_table: nil,
      primary_key: nil,
      primary_key_column: nil,
      query_parser: nil,
      connection_pool: nil
    )
      @original_message = message
      @query_parser = query_parser

      if table
        type = primary_key_column.bigint? ? :bigint : primary_key_column.type
        msg = squish(<<~EOM)
          Column `#{foreign_key}` on table `#{table}` does not match column `#{primary_key}` on `#{target_table}`,
          which has type `#{primary_key_column.sql_type}`.
          To resolve this issue, change the type of the `#{foreign_key}` column on `#{table}` to be :#{type}.
          (For example `t.#{type} :#{foreign_key}`).
        EOM
      else
        msg = squish(<<~EOM)
          There is a mismatch between the foreign key and primary key column types.
          Verify that the foreign key column type and the primary key of the associated table match types.
        EOM
      end
      if message
        msg << "\nOriginal message: #{message}"
      end

      super(msg, sql: sql, binds: binds, connection_pool: connection_pool)
    end

    def set_query(sql, binds)
      if @query_parser && !@sql
        self.class.new(
          message: @original_message,
          sql: sql,
          binds: binds,
          connection_pool: @connection_pool,
          **@query_parser.call(sql)
        ).tap do |exception|
          exception.set_backtrace backtrace
        end
      else
        super
      end
    end

    private
      # ActiveSupport's String#squish, which this tree does not load.
      def squish(text)
        text.gsub(/[[:space:]]+/, " ").strip
      end
  end

  # Rails' constructor takes the model `sole` found too many rows for and
  # names it. The class and its message-only constructor are the shared
  # errors.rb's; the shared `sole` (relation.rb) hands the message it
  # built, which is kept as given.
  class SoleRecordExceeded
    attr_reader :record

    def initialize(record = nil)
      return super(record) if record.is_a?(String)

      @record = record
      super "Wanted only one #{record&.name || "record"}"
    end
  end

  # Not modeled: ActiveSupport::ActionableError's "Create database"
  # action.
  class NoDatabaseError
    def initialize(message = nil, connection_pool: nil)
      super(message || "Database not found", connection_pool: connection_pool)
    end
  end
end
