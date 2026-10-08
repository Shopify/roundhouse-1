# The Active Record error roots every target shares (activerecord
# errors.rb and validations.rb, Rails 8.2.0.alpha). This file is also
# transpiled for the strict targets (`runtime_loader`'s errors entries),
# so each class spells out a message-only constructor: the targets whose
# compilers check a superclass constructor call (Kotlin, Swift, C#) need
# the parent's to exist, and JS Error does not default the message to
# the class name the way Ruby's Exception does.
#
# Rails' fuller constructors on these classes (AdapterError's
# `connection_pool:`, StatementInvalid's `sql:`/`binds:`, RecordNotFound's
# model/primary_key/id, RecordInvalid's I18n message) are not
# transpilable shapes; the CRuby tree's overlay installs them
# (ruby_overlay/runtime/active_record_statement_errors.rb). The rest of
# Rails' hierarchy is Ruby-family only (error_classes.rb).
module ActiveRecord
  # Rails' generic Active Record exception, the root of every class
  # below. The default message is the class name, as Ruby's Exception
  # has it; spelled out because JS Error has none. A subclass with no
  # constructor of its own inherits this default on the trees without
  # the overlay (Rails names the subclass).
  class ActiveRecordError < StandardError
    def initialize(message = "ActiveRecord::ActiveRecordError")
      super(message)
    end
  end

  # Rails' superclass for the errors an adapter raises.
  class AdapterError < ActiveRecordError
    def initialize(message = "ActiveRecord::AdapterError")
      super(message)
    end
  end

  class RecordNotFound < ActiveRecordError
    # Bare-construct default. Ruby's StandardError sets the message to
    # the class name implicitly; JS Error doesn't, so spell the
    # contract out so transpiled targets get the same default.
    def initialize(message = "ActiveRecord::RecordNotFound")
      super(message)
    end
  end

  # Rails' error for a statement the database refused. Only the CRuby
  # tree's `Db` translates its driver's errors into it (`db_cruby.rb`),
  # and that tree's overlay adds the statement (`sql`, `binds`) and the
  # pool; `project` refuses the constant on every other target. The
  # constructor is the one its subclasses hand their message to.
  class StatementInvalid < AdapterError
    def initialize(message = "ActiveRecord::StatementInvalid")
      super(message)
    end
  end

  # Rails keeps this defunct wrapper as RecordNotUnique's and
  # InvalidForeignKey's parent, so a `rescue WrappedDatabaseException`
  # still catches them.
  class WrappedDatabaseException < StatementInvalid
    def initialize(message = "ActiveRecord::WrappedDatabaseException")
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
  # RAISED BY EACH `Db`, not from here: the emitted per-model
  # `_adapter_insert` hands its SQL straight to the per-target `Db.exec`
  # primitive, and that is the only place holding the driver's error.
  # The three ruby-family `Db`s (`db.rb`, `db_cruby.rb`, `db_jruby.rb`)
  # each map it in `exec`, keyed on SQLITE'S OWN message text — "UNIQUE
  # constraint failed: <table>.<column>" comes out of the engine, so the
  # same string appears in the cruby gem's ConstraintException, in the
  # JDBC SQLException and in `sqlite3_errmsg`. The strict targets carry
  # their own `Db` and owe their own mapping.
  class RecordNotUnique < WrappedDatabaseException
    def initialize(message = "ActiveRecord::RecordNotUnique")
      super(message)
    end
  end

  # Rails raises this from `sole` / `find_sole_by` when more than one
  # record matches ("Wanted only one Widget"), under ActiveRecordError.
  # Rails passes the relation and names its model; the shared `sole`
  # (relation.rb) builds the message itself, so the constructor here
  # takes a message only, as for the classes above. The CRuby tree's
  # overlay adds Rails' model-taking constructor and `record` reader
  # (ruby_overlay/runtime/active_record_statement_errors.rb).
  class SoleRecordExceeded < ActiveRecordError
    def initialize(message = "Wanted only one record")
      super(message)
    end
  end

  # validations.rb: raised by `save!` when validation fails.
  class RecordInvalid < ActiveRecordError
    attr_reader :record

    def initialize(record)
      @record = record
      super("Validation failed: #{record.errors.join(', ')}")
    end
  end
end
