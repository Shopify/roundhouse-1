module ActiveRecord
  class RecordNotFound < StandardError
    # Bare-construct default. Ruby's StandardError sets the message to
    # the class name implicitly; JS Error doesn't, so spell the
    # contract out so transpiled targets get the same default.
    def initialize(message = "ActiveRecord::RecordNotFound")
      super(message)
    end
  end

  # Rails' error for a statement the database refused. Rails hangs it
  # off AdapterError < ActiveRecordError; with no ActiveRecordError
  # here, StandardError is the parent. Only the CRuby tree's `Db`
  # translates its driver's errors into it (`db_cruby.rb`), and that
  # tree's overlay adds the statement (`sql`, `binds`); `project`
  # refuses the constant on every other target. The constructor is the
  # one its subclasses hand their message to, spelled out for the
  # targets whose parent constructor must exist (Kotlin, Swift, C#).
  class StatementInvalid < StandardError
    def initialize(message = "ActiveRecord::StatementInvalid")
      super(message)
    end
  end

  # Rails raises this when a value exceeds its column's declared limit;
  # apps also construct it directly to reject over-long input before it
  # reaches the DB (lobsters' Keystore.validate_input_key). A
  # StatementInvalid, as in Rails.
  class ValueTooLong < StatementInvalid
    def initialize(message = "ActiveRecord::ValueTooLong")
      super(message)
    end
  end

  # Rails raises this when an INSERT/UPDATE trips a unique index. Apps
  # rescue it to turn a lost race into a redirect rather than a 500 —
  # campfire's first-run screen does exactly that (two people opening a
  # brand-new install at once; the loser is sent to the root path).
  #
  # Rails hangs it off WrappedDatabaseException < StatementInvalid; here
  # it is a StatementInvalid directly.
  #
  # RAISED BY EACH `Db`, not from here: the emitted per-model
  # `_adapter_insert` hands its SQL straight to the per-target `Db.exec`
  # primitive, and that is the only place holding the driver's error.
  # The three ruby-family `Db`s (`db.rb`, `db_cruby.rb`, `db_jruby.rb`)
  # each map it in `exec`, keyed on SQLITE'S OWN message text — "UNIQUE
  # constraint failed: <table>.<column>" comes out of the engine, so the
  # same string appears in the cruby gem's ConstraintException, in the
  # JDBC SQLException and in `sqlite3_errmsg`. The strict targets carry
  # their own `Db` and owe their own mapping.
  class RecordNotUnique < StatementInvalid
    def initialize(message = "ActiveRecord::RecordNotUnique")
      super(message)
    end
  end

  class RecordInvalid < StandardError
    attr_reader :record

    def initialize(record)
      @record = record
      super("Validation failed: #{record.errors.join(', ')}")
    end
  end
end
