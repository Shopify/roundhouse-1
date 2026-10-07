# Active Record's transactions on the PostgreSQL adapter, ported from the
# Rails native Core runs (Shopify/rails 5813559f4aed, 8.2.0.alpha):
# connection_adapters/abstract/transaction.rb (TransactionState,
# TransactionInstrumenter, NullTransaction, Transaction,
# RestartParentTransaction, SavepointTransaction, RealTransaction,
# TransactionManager), abstract/database_statements.rb (`transaction`,
# reset_transaction, the begin/commit/rollback entry points),
# abstract/savepoints.rb, transaction.rb (ActiveRecord::Transaction),
# transactions.rb (`Base.transaction`, `#transaction`), and
# connection_pool.rb's pool transaction isolation level.
#
# Transactions are lazy, as on Rails' PostgreSQL adapter: BEGIN (or
# SAVEPOINT) is sent before the first statement that runs inside the
# transaction (with_raw_connection's `materialize_transactions`), and a
# transaction no statement used is never sent. A nested `transaction`
# joins the current one unless `requires_new:` or the current one is not
# joinable; then it is a savepoint (`active_record_N`), or, when the
# current transaction is joinable and still clean, a restart of it.
# `ActiveRecord::Rollback` rolls the innermost transaction back and is
# swallowed.
#
# Not ported: record enrollment (`add_transaction_record`: a record's
# after_commit/after_rollback callbacks and state restore). The runtime's
# record persistence does not enroll records, so a real transaction
# refuses an enrollment by name rather than drop it. Query retry
# (`allow_retry`) is not modeled, BEGIN's included (see db_pg_cruby.rb).
# `reconnect_can_restore_state?` also requires the session to hold no
# transaction libpq reports, so a materialized clean transaction is not
# restored by an implicit reconnect (Rails restores it).
require "monitor"
require "securerandom"

module ActiveRecord
  # Rollback, TransactionRollbackError, SerializationFailure, Deadlocked
  # and TransactionIsolationError are runtime/ruby/active_record/error_classes.rb's.

  # active_record.rb: `singleton_class.attr_accessor
  # :default_transaction_isolation_level` (nil).
  unless respond_to?(:default_transaction_isolation_level)
    class << self
      attr_accessor :default_transaction_isolation_level
    end
    self.default_transaction_isolation_level = nil
  end

  # transaction.rb: the transaction a block of `transaction` is given.
  class Transaction
    def initialize(internal_transaction) # :nodoc:
      @internal_transaction = internal_transaction
      @uuid = nil
    end

    def after_commit(&block)
      if @internal_transaction.nil?
        yield
      else
        @internal_transaction.after_commit(&block)
      end
    end

    def after_rollback(&block)
      @internal_transaction&.after_rollback(&block)
    end

    def open?
      !closed?
    end

    def closed?
      @internal_transaction.nil? || @internal_transaction.closed?
    end

    alias_method :blank?, :closed?

    def uuid
      if @internal_transaction
        @uuid ||= SecureRandom.uuid
      end
    end

    NULL_TRANSACTION = new(nil).freeze
  end

  module ConnectionAdapters
    class TransactionState
      def initialize(state = nil)
        @state = state
        @children = nil
      end

      def add_child(state)
        @children ||= []
        @children << state
      end

      def finalized?
        @state
      end

      def committed?
        @state == :committed || @state == :fully_committed
      end

      def fully_committed?
        @state == :fully_committed
      end

      def rolledback?
        @state == :rolledback || @state == :fully_rolledback
      end

      def fully_rolledback?
        @state == :fully_rolledback
      end

      def invalidated?
        @state == :invalidated
      end

      def fully_completed?
        completed?
      end

      def completed?
        committed? || rolledback?
      end

      def rollback!
        @children&.each { |c| c.rollback! }
        @state = :rolledback
      end

      def full_rollback!
        @children&.each { |c| c.rollback! }
        @state = :fully_rolledback
      end

      def invalidate!
        @children&.each { |c| c.invalidate! }
        @state = :invalidated
      end

      def commit!
        @state = :committed
      end

      def full_commit!
        @state = :fully_committed
      end

      def nullify!
        @state = nil
      end
    end

    class TransactionInstrumenter
      def initialize(payload = {})
        @handle = nil
        @started = false
        @payload = nil
        @base_payload = payload
      end

      class InstrumentationNotStartedError < StandardError; end
      class InstrumentationAlreadyStartedError < StandardError; end

      def start
        raise InstrumentationAlreadyStartedError.new("Called start on an already started transaction") if @started
        @started = true

        ActiveSupport::Notifications.instrument("start_transaction.active_record", @base_payload)

        @payload = @base_payload.dup # the payload of an event gains its outcome later
        @handle = ActiveSupport::Notifications.instrumenter.build_handle("transaction.active_record", @payload)
        @handle.start
      end

      def finish(outcome)
        raise InstrumentationNotStartedError.new("Called finish on a transaction that hasn't started") unless @started
        @started = false

        @payload[:outcome] = outcome
        @handle.finish
      end
    end

    class NullTransaction # :nodoc:
      def state; end
      def closed?; true; end
      def open?; false; end
      def joinable?; false; end
      def isolation; nil; end
      def add_record(record, _ = true); end
      def restartable?; false; end
      def dirty?; false; end
      def dirty!; end
      def invalidated?; false; end
      def invalidate!; end
      def materialized?; false; end
      def before_commit; yield; end
      def after_commit; yield; end
      def after_rollback; end
      def user_transaction; ActiveRecord::Transaction::NULL_TRANSACTION; end
      def isolation=(_); end
    end

    class Transaction # :nodoc:
      # Rails raises ActiveRecordError.
      FinalizedTransactionError = ActiveRecord::ActiveRecordError

      class Callback # :nodoc:
        def initialize(event, callback)
          @event = event
          @callback = callback
        end

        def before_commit
          @callback.call if @event == :before_commit
        end

        def after_commit
          @callback.call if @event == :after_commit
        end

        def after_rollback
          @callback.call if @event == :after_rollback
        end
      end

      attr_reader :connection, :state, :savepoint_name, :isolation_level, :user_transaction

      def invalidate!
        @state.invalidate!
      end

      def invalidated?
        @state.invalidated?
      end

      def isolation
        @isolation_level
      end

      def isolation=(isolation) # :nodoc:
        @isolation_level = isolation
      end

      def initialize(connection, isolation: nil, joinable: true, run_commit_callbacks: false)
        super()
        @connection = connection
        @state = TransactionState.new
        @callbacks = nil
        @records = nil
        @isolation_level = isolation
        @materialized = false
        @joinable = joinable
        @run_commit_callbacks = run_commit_callbacks
        @dirty = false
        @user_transaction = joinable ? ActiveRecord::Transaction.new(self) : ActiveRecord::Transaction::NULL_TRANSACTION
        @instrumenter = TransactionInstrumenter.new(connection: connection, transaction: @user_transaction)
      end

      def dirty!
        @dirty = true
      end

      def dirty?
        @dirty
      end

      def open?
        !closed?
      end

      def closed?
        @state.finalized?
      end

      # Not ported: see the header.
      def add_record(record, ensure_finalize = true)
        raise NotImplementedError, "ActiveRecord transaction record enrollment (add_transaction_record: " \
          "after_commit/after_rollback callbacks and state restore of #{record.class.name}) is not ported"
      end

      def before_commit(&block)
        if @state.finalized?
          raise FinalizedTransactionError, "Cannot register callbacks on a finalized transaction"
        end

        (@callbacks ||= []) << Callback.new(:before_commit, block)
      end

      def after_commit(&block)
        if @state.finalized?
          raise FinalizedTransactionError, "Cannot register callbacks on a finalized transaction"
        end

        (@callbacks ||= []) << Callback.new(:after_commit, block)
      end

      def after_rollback(&block)
        if @state.finalized?
          raise FinalizedTransactionError, "Cannot register callbacks on a finalized transaction"
        end

        (@callbacks ||= []) << Callback.new(:after_rollback, block)
      end

      def records
        @records
      end

      def restartable?
        joinable? && !dirty?
      end

      def incomplete!
        @instrumenter.finish(:incomplete) if materialized?
      end

      def materialize!
        @materialized = true
        @instrumenter.start
      end

      def materialized?
        @materialized
      end

      def restore!
        if materialized?
          incomplete!
          @materialized = false
          materialize!
        end
      end

      def rollback_records
        @callbacks&.each(&:after_rollback)
      end

      def before_commit_records
        if @run_commit_callbacks
          @callbacks&.each(&:before_commit)
        end
      end

      def commit_records
        if @run_commit_callbacks
          @callbacks&.each(&:after_commit)
        elsif @callbacks
          connection.current_transaction.append_callbacks(@callbacks)
        end
      end

      def full_rollback?; true; end
      def joinable?; @joinable; end

      protected
        def append_callbacks(callbacks) # :nodoc:
          (@callbacks ||= []).concat(callbacks)
        end
    end

    class RestartParentTransaction < Transaction
      def initialize(connection, parent_transaction, **options)
        super(connection, **options)

        @parent = parent_transaction

        if isolation_level
          raise ActiveRecord::TransactionIsolationError, "cannot set transaction isolation in a nested transaction"
        end

        @parent.state.add_child(@state)
      end

      def materialize!
        @parent.materialize!
      end

      def materialized?
        @parent.materialized?
      end

      def restart
        @parent.restart
      end

      def isolation
        @parent.isolation
      end

      def rollback
        @state.rollback!
        @parent.restart
      end

      def commit
        @state.commit!
      end

      def full_rollback?; false; end
    end

    class SavepointTransaction < Transaction
      def initialize(connection, savepoint_name, parent_transaction, **options)
        super(connection, **options)

        @parent_transaction = parent_transaction
        parent_transaction.state.add_child(@state)

        if isolation_level
          raise ActiveRecord::TransactionIsolationError, "cannot set transaction isolation in a nested transaction"
        end

        @savepoint_name = savepoint_name
      end

      def isolation
        @parent_transaction.isolation
      end

      def isolation=(isolation) # :nodoc:
        @parent_transaction.isolation = isolation
      end

      def materialize!
        connection.create_savepoint(savepoint_name)
        super
      end

      def restart
        return unless materialized?

        @instrumenter.finish(:restart)
        @instrumenter.start

        connection.rollback_to_savepoint(savepoint_name)
      end

      def rollback
        unless @state.invalidated?
          connection.rollback_to_savepoint(savepoint_name) if materialized? && connection.active?
        end
        @state.rollback!
        @instrumenter.finish(:rollback) if materialized?
      end

      def commit
        connection.release_savepoint(savepoint_name) if materialized?
        @state.commit!
        @instrumenter.finish(:commit) if materialized?
      end

      def full_rollback?; false; end
    end

    class RealTransaction < Transaction
      def materialize!
        if joinable?
          if isolation_level
            connection.begin_isolated_db_transaction(isolation_level)
          else
            connection.begin_db_transaction
          end
        else
          connection.begin_deferred_transaction(isolation_level)
        end

        super
      end

      def restart
        return unless materialized?

        @instrumenter.finish(:restart)

        if connection.supports_restart_db_transaction?
          @instrumenter.start
          connection.restart_db_transaction
        else
          connection.rollback_db_transaction
          materialize!
        end
      end

      def rollback
        if materialized?
          connection.rollback_db_transaction unless @state.invalidated?
          connection.reset_isolation_level if isolation_level
        end
        @state.full_rollback!
        @instrumenter.finish(:rollback) if materialized?
      end

      def commit
        if materialized?
          connection.commit_db_transaction
          connection.reset_isolation_level if isolation_level
        end
        @state.full_commit!
        @instrumenter.finish(:commit) if materialized?
      end
    end

    class TransactionManager # :nodoc:
      def initialize(connection)
        @stack = []
        @connection = connection
        @has_unmaterialized_transactions = false
        @materializing_transactions = false
        @lazy_transactions_enabled = true
      end

      def begin_transaction(isolation: nil, joinable: true, _lazy: true)
        @connection.lock.synchronize do
          run_commit_callbacks = !current_transaction.joinable?
          transaction =
            if @stack.empty?
              RealTransaction.new(
                @connection,
                isolation: isolation,
                joinable: joinable,
                run_commit_callbacks: run_commit_callbacks
              )
            elsif current_transaction.restartable?
              RestartParentTransaction.new(
                @connection,
                current_transaction,
                isolation: isolation,
                joinable: joinable,
                run_commit_callbacks: run_commit_callbacks
              )
            else
              SavepointTransaction.new(
                @connection,
                "active_record_#{@stack.size}",
                current_transaction,
                isolation: isolation,
                joinable: joinable,
                run_commit_callbacks: run_commit_callbacks
              )
            end

          unless transaction.materialized?
            if @connection.supports_lazy_transactions? && lazy_transactions_enabled? && _lazy
              @has_unmaterialized_transactions = true
            else
              transaction.materialize!
            end
          end
          @stack.push(transaction)
          transaction
        end
      end

      def disable_lazy_transactions!
        materialize_transactions
        @lazy_transactions_enabled = false
      end

      def enable_lazy_transactions!
        @lazy_transactions_enabled = true
      end

      def lazy_transactions_enabled?
        @lazy_transactions_enabled
      end

      def dirty_current_transaction
        current_transaction.dirty!
      end

      def restore_transactions
        return false unless restorable?

        @stack.each(&:restore!)

        true
      end

      def restorable?
        @stack.none?(&:dirty?)
      end

      def materialize_transactions
        return if @materializing_transactions

        if @has_unmaterialized_transactions
          @connection.lock.synchronize do
            begin
              @materializing_transactions = true
              @stack.each { |t| t.materialize! unless t.materialized? }
            ensure
              @materializing_transactions = false
            end
            @has_unmaterialized_transactions = false
          end
        end
      end

      def commit_transaction
        @connection.lock.synchronize do
          transaction = @stack.last

          begin
            transaction.before_commit_records
          ensure
            @stack.pop
          end

          dirty_current_transaction if transaction.dirty?

          transaction.commit
          transaction.commit_records
        end
      end

      def rollback_transaction(transaction = nil)
        @connection.lock.synchronize do
          transaction ||= @stack.last
          begin
            transaction.rollback
          ensure
            @stack.pop if @stack.last == transaction
          end
          transaction.rollback_records
        end
      end

      def within_new_transaction(isolation: nil, joinable: true)
        isolation ||= @connection.pool&.pool_transaction_isolation_level
        @connection.lock.synchronize do
          transaction = begin_transaction(isolation: isolation, joinable: joinable)
          begin
            yield transaction.user_transaction
          rescue Exception => error
            rollback_transaction
            after_failure_actions(transaction, error)

            raise
          ensure
            unless error
              if Thread.current.status == "aborting"
                rollback_transaction
              else
                begin
                  commit_transaction
                rescue ActiveRecord::ConnectionFailed
                  transaction.invalidate! unless transaction.state.completed?
                  raise
                rescue ActiveRecord::TransactionRollbackError
                  unless transaction.state.completed?
                    transaction.invalidate!
                    rollback_transaction(transaction)
                  end
                  raise
                rescue Exception
                  rollback_transaction(transaction) unless transaction.state.completed?
                  raise
                end
              end
            end
          end
        ensure
          unless transaction&.state&.completed?
            @connection.throw_away!
            transaction&.incomplete!
          end
        end
      end

      def open_transactions
        @stack.size
      end

      def current_transaction
        @stack.last || NULL_TRANSACTION
      end

      private
        NULL_TRANSACTION = NullTransaction.new.freeze

        def after_failure_actions(transaction, error)
          return unless transaction.is_a?(RealTransaction)
          return unless defined?(ActiveRecord::PreparedStatementCacheExpired) && error.is_a?(ActiveRecord::PreparedStatementCacheExpired)
          @connection.clear_cache!
        end
    end

    class PostgreSQLAdapter
      # Rails 8.2 AbstractAdapter#pinned: held by ConnectionPool#pin_connection!.
      attr_accessor :pinned # :nodoc:

      # abstract/database_statements.rb.
      def transaction(requires_new: nil, isolation: nil, joinable: true, &block)
        isolation_override = false
        if isolation && open_transactions == 1 && !current_transaction.joinable?
          iso = isolation.to_sym

          unless transaction_isolation_levels.include?(iso)
            raise ActiveRecord::TransactionIsolationError,
                  "invalid transaction isolation level: #{iso.inspect}"
          end

          isolation_override = true
          old_isolation = current_transaction.isolation
          current_transaction.isolation = iso
          isolation = nil
        end

        if !requires_new && current_transaction.joinable?
          if isolation && current_transaction.isolation != isolation
            raise ActiveRecord::TransactionIsolationError, "cannot set isolation when joining a transaction"
          end
          yield current_transaction.user_transaction
        else
          within_new_transaction(isolation: isolation, joinable: joinable, &block)
        end
      rescue ActiveRecord::Rollback
        # rollbacks are silently swallowed
      ensure
        current_transaction.isolation = old_isolation if isolation_override
      end

      def transaction_manager # :nodoc:
        @transaction_manager ||= TransactionManager.new(self)
      end

      def within_new_transaction(isolation: nil, joinable: true, &block) # :nodoc:
        transaction_manager.within_new_transaction(isolation: isolation, joinable: joinable, &block)
      end

      def open_transactions
        transaction_manager.open_transactions
      end

      def current_transaction
        transaction_manager.current_transaction
      end

      def begin_transaction(isolation: nil, joinable: true, _lazy: true) # :nodoc:
        transaction_manager.begin_transaction(isolation: isolation, joinable: joinable, _lazy: _lazy)
      end

      def commit_transaction # :nodoc:
        transaction_manager.commit_transaction
      end

      def rollback_transaction(transaction = nil) # :nodoc:
        transaction_manager.rollback_transaction(transaction)
      end

      def materialize_transactions # :nodoc:
        transaction_manager.materialize_transactions
      end

      def disable_lazy_transactions! # :nodoc:
        transaction_manager.disable_lazy_transactions!
      end

      def enable_lazy_transactions! # :nodoc:
        transaction_manager.enable_lazy_transactions!
      end

      def dirty_current_transaction # :nodoc:
        transaction_manager.dirty_current_transaction
      end

      def transaction_open?
        current_transaction.open?
      end

      def reset_transaction(restore: false) # :nodoc:
        # Store the existing transaction state to the side
        old_state = @transaction_manager if restore && @transaction_manager&.restorable?

        @transaction_manager = ConnectionAdapters::TransactionManager.new(self)

        if block_given?
          # Reconfigure the connection without any transaction state in the way
          result = yield

          # Now the connection's fully established, we can swap back
          if old_state
            @transaction_manager = old_state
            @transaction_manager.restore_transactions
          end

          result
        end
      end

      def add_transaction_record(record, ensure_finalize = true)
        current_transaction.add_record(record, ensure_finalize)
      end

      def begin_deferred_transaction(isolation_level = nil) # :nodoc:
        if isolation_level
          begin_isolated_db_transaction(isolation_level)
        else
          begin_db_transaction
        end
      end

      TRANSACTION_ISOLATION_LEVELS = {
        read_uncommitted: "READ UNCOMMITTED",
        read_committed:   "READ COMMITTED",
        repeatable_read:  "REPEATABLE READ",
        serializable:     "SERIALIZABLE"
      }.freeze
      private_constant :TRANSACTION_ISOLATION_LEVELS

      def transaction_isolation_levels
        TRANSACTION_ISOLATION_LEVELS
      end

      def reset_isolation_level # :nodoc:
      end

      def rollback_db_transaction
        exec_rollback_db_transaction
      rescue ActiveRecord::ConnectionNotEstablished, ActiveRecord::ConnectionFailed
        # Connection's gone; that counts as a rollback
      end

      def restart_db_transaction
        exec_restart_db_transaction
      end

      def rollback_to_savepoint(name = nil)
        exec_rollback_to_savepoint(name)
      end

      # abstract/savepoints.rb.
      def current_savepoint_name
        current_transaction.savepoint_name
      end

      def create_savepoint(name = current_savepoint_name)
        query_command("SAVEPOINT #{name}", "TRANSACTION")
      end

      def exec_rollback_to_savepoint(name = current_savepoint_name)
        query_command("ROLLBACK TO SAVEPOINT #{name}", "TRANSACTION")
      end

      def release_savepoint(name = current_savepoint_name)
        query_command("RELEASE SAVEPOINT #{name}", "TRANSACTION")
      end

      def supports_savepoints?
        true
      end

      def supports_lazy_transactions?
        true
      end

      def savepoint_errors_invalidate_transactions?
        false
      end

      def supports_restart_db_transaction?
        database_version >= 12_00_00 # >= 12.0
      end

      def database_version # :nodoc:
        @database_version ||= valid_raw_connection.server_version
      end

      # Rails' transaction_manager.restorable?, with the session's own
      # transaction state as well (see the header).
      def reconnect_can_restore_state?
        super && transaction_manager.restorable?
      end

      # Rails 8.2 PostgreSQLAdapter#reset! over AbstractAdapter#reset!.
      def reset!
        @lock.synchronize do
          return connect! unless @raw_connection

          unless @raw_connection.transaction_status == ::PG::PQTRANS_IDLE
            @raw_connection.query "ROLLBACK"
          end
          @raw_connection.query "DISCARD ALL"

          attempt_configure_connection do
            reset_transaction
            configure_connection
          end
        end
      end

      # AbstractAdapter#throw_away!: the session is closed; the adapter
      # stays in the runtime's pool and connects anew at its next use (the
      # runtime's pools do not remove adapters).
      def throw_away!
        disconnect!
      end

      def clear_cache!(new_connection: false)
        nil
      end
    end

    class ConnectionPool
      # connection_pool.rb: the isolation level `Base.transaction` gives a
      # new transaction, per pool and execution context.
      def pool_transaction_isolation_level
        (Thread.current[:roundhouse_pool_transaction_isolation_levels] ||= {})[db_config.name]
      end

      def pool_transaction_isolation_level=(isolation_level)
        (Thread.current[:roundhouse_pool_transaction_isolation_levels] ||= {})[db_config.name] = isolation_level
      end

      def with_pool_transaction_isolation_level(isolation_level, transaction_open, &block) # :nodoc:
        if !ActiveRecord.default_transaction_isolation_level.nil?
          begin
            if transaction_open && self.pool_transaction_isolation_level != ActiveRecord.default_transaction_isolation_level
              raise ActiveRecord::TransactionIsolationError, "cannot set default isolation level while transaction is open"
            end

            old_level = self.pool_transaction_isolation_level
            self.pool_transaction_isolation_level = isolation_level
            yield
          ensure
            self.pool_transaction_isolation_level = old_level
          end
        else
          yield
        end
      end
    end
  end

  class Base
    class << self
      # transactions.rb ClassMethods#transaction. With no pool in the
      # handler (a program driving Db's databases directly), the adapter
      # Db leases for the current database.
      def transaction(**options, &block)
        if connection_handler.connection_pool_names.empty?
          return Db.with_connection { Db.current_adapter.transaction(**options, &block) }
        end
        with_connection do |connection|
          connection.pool.with_pool_transaction_isolation_level(ActiveRecord.default_transaction_isolation_level, connection.transaction_open?) do
            connection.transaction(**options, &block)
          end
        end
      end

      # transactions.rb ClassMethods#current_transaction.
      def current_transaction
        connection_pool.active_connection&.current_transaction&.user_transaction || Transaction::NULL_TRANSACTION
      end
    end

    def transaction(**options, &block)
      self.class.transaction(**options, &block)
    end
  end
end
