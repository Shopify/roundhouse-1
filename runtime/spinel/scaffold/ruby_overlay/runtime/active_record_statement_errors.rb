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

end
