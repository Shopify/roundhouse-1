# frozen_string_literal: true
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
# Records enroll as Rails' do (transactions.rb `with_transaction_returning_status`,
# below `ActiveRecord::Transactions`): save, the validation-skipping write
# (`save_after_validation`), destroy, touch and update_attribute run in
# `connection.transaction`, and the record's transaction callbacks (the
# chains the compiler lowered into `_run_commit_callbacks` and its two
# siblings, runtime/ruby/active_record/base.rb) run when the outermost
# transaction commits, its rollback chain and state restore when one
# rolls back. Every enrollment is held until the transaction finishes:
# Rails holds a record without transaction callbacks in a write-time
# transaction only weakly (`ensure_finalize`), so a collected one is not
# restored there. Query retry
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

  # active_record.rb `after_all_transactions_commit` / `all_open_transactions`
  # (Rails 7.2+): run the block now when no joinable transaction is open on
  # any pool's active connection, else once every such transaction has
  # committed (dropped when one rolls back). A non-joinable transaction (the
  # one a transactional test wraps its example in) does not count.
  unless respond_to?(:after_all_transactions_commit)
    def self.after_all_transactions_commit(&block)
      open_transactions = all_open_transactions

      if open_transactions.empty?
        yield
      elsif open_transactions.size == 1
        open_transactions.first.after_commit(&block)
      else
        count = open_transactions.size
        callback = -> do
          count -= 1
          block.call if count.zero?
        end
        open_transactions.each do |t|
          t.after_commit(&callback)
        end
        open_transactions = nil # avoid holding it in the closure
      end
    end

    def self.all_open_transactions # :nodoc:
      open_transactions = []
      Base.connection_handler.each_connection_pool do |pool|
        if (active_connection = pool.active_connection)
          current_transaction = active_connection.current_transaction

          if current_transaction.open? && current_transaction.joinable?
            open_transactions << current_transaction
          end
        end
      end
      open_transactions
    end
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

      def add_record(record, _ensure_finalize = true)
        (@records ||= []) << record
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
        if records
          begin
            ite = unique_records

            instances_to_run_callbacks_on = prepare_instances_to_run_callbacks_on(ite)

            run_action_on_records(ite, instances_to_run_callbacks_on) do |record, should_run_callbacks|
              record.rolledback!(force_restore_state: full_rollback?, should_run_callbacks: should_run_callbacks)
            end
          ensure
            ite&.each do |i|
              i.rolledback!(force_restore_state: full_rollback?, should_run_callbacks: false)
            end
          end
        end

        @callbacks&.each(&:after_rollback)
      end

      def before_commit_records
        if @run_commit_callbacks
          if records
            if ActiveRecord.before_committed_on_all_records
              ite = unique_records

              instances_to_run_callbacks_on = records.each_with_object({}) do |record, candidates|
                candidates[_record_key(record)] = record
              end

              run_action_on_records(ite, instances_to_run_callbacks_on) do |record, should_run_callbacks|
                record.before_committed! if should_run_callbacks
              end
            else
              records.uniq.each(&:before_committed!)
            end
          end

          @callbacks&.each(&:before_commit)
        end
        # Note: When @run_commit_callbacks is false #commit_records takes care of appending
        # remaining callbacks to the parent transaction
      end

      def commit_records
        if records
          begin
            ite = unique_records

            if @run_commit_callbacks
              instances_to_run_callbacks_on = prepare_instances_to_run_callbacks_on(ite)

              run_action_on_records(ite, instances_to_run_callbacks_on) do |record, should_run_callbacks|
                record.committed!(should_run_callbacks: should_run_callbacks)
              end
            else
              while record = ite.shift
                # if not running callbacks, only adds the record to the parent transaction
                connection.add_transaction_record(record)
              end
            end
          ensure
            ite&.each { |i| i.committed!(should_run_callbacks: false) }
          end
        end

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

      private
        def unique_records
          records.uniq(&:__id__)
        end

        # A record handed to `add_transaction_record` need not be a model:
        # a duck with `committed!` (Platform::Callbacks' after-commit duck)
        # answers the public `trigger_transactional_callbacks?` and has no
        # identity key of its own, so it is its own candidate key (Rails'
        # `candidates[record]`).
        def _trigger_callbacks?(record)
          if record.respond_to?(:_trigger_transactional_callbacks?)
            record._trigger_transactional_callbacks?
          else
            record.trigger_transactional_callbacks?
          end
        end

        def _record_key(record)
          record.respond_to?(:_transaction_record_key) ? record._transaction_record_key : record
        end

        # A candidate is looked up by the record's identity (Rails' `==`/
        # `hash`: class and id, the object itself before it has one).
        def run_action_on_records(records, instances_to_run_callbacks_on)
          while record = records.shift
            should_run_callbacks = record.__id__ == instances_to_run_callbacks_on[_record_key(record)].__id__

            yield record, should_run_callbacks
          end
        end

        def prepare_instances_to_run_callbacks_on(records)
          records.each_with_object({}) do |record, candidates|
            next unless _trigger_callbacks?(record)

            key = _record_key(record)
            earlier_saved_candidate = candidates[key]

            next if earlier_saved_candidate && record.class.run_commit_callbacks_on_first_saved_instances_in_transaction

            # If the candidate instance destroyed itself in the database, then
            # instances which were added to the transaction afterwards, and which
            # think they updated themselves, are wrong. They should not replace
            # our candidate as an instance to run callbacks on
            next if earlier_saved_candidate&.destroyed? && !record.destroyed?

            # If the candidate instance was created inside of this transaction,
            # then instances which were subsequently loaded from the database
            # and updated need that state transferred to them so that
            # the after_create_commit callbacks are run
            record._new_record_before_last_commit = true if earlier_saved_candidate&._new_record_before_last_commit

            # The last instance to save itself is likeliest to have internal
            # state that matches what's committed to the database
            candidates[key] = record
          end
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

    # abstract_adapter.rb and its abstract/database_statements.rb and
    # abstract/savepoints.rb: on AbstractAdapter, so a module prepended to
    # it (Core's TransactionConnectionTracker) wraps `transaction` on the
    # live PostgreSQL adapter.
    class AbstractAdapter
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

      def savepoint_errors_invalidate_transactions?
        false
      end

      # Rails' transaction_manager.restorable?, with the session's own
      # transaction state as well (see the header).
      def reconnect_can_restore_state?
        super && transaction_manager.restorable?
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

    # postgresql_adapter.rb's overrides of the abstract members above.
    class PostgreSQLAdapter
      def supports_savepoints?
        true
      end

      def supports_lazy_transactions?
        true
      end

      def supports_restart_db_transaction?
        database_version >= 12_00_00 # >= 12.0
      end

      # postgresql_adapter.rb: every supported server returns inserted columns.
      def supports_insert_returning?
        true
      end

      def database_version # :nodoc:
        @database_version ||= valid_raw_connection.server_version
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

  # active_record.rb `ActiveRecord.before_committed_on_all_records` and
  # Base's `run_commit_callbacks_on_first_saved_instances_in_transaction`:
  # the value config/application.rb leaves (the compiler synthesizes the
  # reader on Rails::Application when it is not the framework default).
  class << self
    def before_committed_on_all_records
      app = Rails.application
      app.respond_to?(:active_record_before_committed_on_all_records) ? app.active_record_before_committed_on_all_records : false
    end
  end

  # transactions.rb, the record side.
  module Transactions
    def save(*, **)
      with_transaction_returning_status { super }
    end

    def save_after_validation
      with_transaction_returning_status { super }
    end

    def destroy
      with_transaction_returning_status { super }
    end

    def touch(*, **)
      with_transaction_returning_status { super }
    end

    def touch_written
      with_transaction_returning_status { super }
    end

    def update_attribute(name, value)
      with_transaction_returning_status { super }
    end

    attr_accessor :_new_record_before_last_commit

    # The enrollment does what base.rb's write-time pair does without a
    # transaction port.
    def _transaction_write_begin; end
    def _transaction_write_end; end

    # Rails keys transaction candidates by the record itself: `hash`/`eql?`
    # use class and id once the id is present (all parts for a composite key)
    # and object identity before that. The id is never compared with 0 or "",
    # which also keeps app id types such as UUID value objects out of it.
    def _transaction_record_key # :nodoc:
      key = id
      present = key.is_a?(Array) ? key.all? : key
      present ? [self.class, key] : self
    end

    def before_committed! # :nodoc:
      _run_before_commit_callbacks
    end

    def committed!(should_run_callbacks: true) # :nodoc:
      @_start_transaction_state = nil
      if should_run_callbacks
        @_committed_already_called = true
        _run_commit_hooks
      end
    ensure
      @_committed_already_called = @_trigger_update_callback = @_trigger_destroy_callback = false
    end

    def rolledback!(force_restore_state: false, should_run_callbacks: true) # :nodoc:
      if should_run_callbacks
        _transaction_rolled_back
      end
    ensure
      restore_transaction_record_state(force_restore_state)
      clear_transaction_record_state
      @_trigger_update_callback = @_trigger_destroy_callback = false if force_restore_state
    end

    def with_transaction_returning_status # :nodoc:
      _with_transaction_connection do |connection|
        status = nil
        connection.transaction do
          connection.add_transaction_record(self)
          remember_transaction_record_state

          status = yield
          raise ActiveRecord::Rollback unless status
        end
        @_last_transaction_return_status = status
        status
      end
    end

    private
      # `self.class.with_connection`, or with no pool in the handler the
      # adapter Db leases (as `Base.transaction`).
      def _with_transaction_connection
        if self.class.connection_handler.connection_pool_names.empty?
          return Db.with_connection { yield Db.current_adapter }
        end
        self.class.with_connection do |connection|
          connection.pool.with_pool_transaction_isolation_level(ActiveRecord.default_transaction_isolation_level, connection.transaction_open?) do
            yield connection
          end
        end
      end

      def remember_transaction_record_state
        @_start_transaction_state ||= {
          id: id,
          new_record: new_record?,
          persisted: @persisted,
          previously_new_record: @id_previously_changed,
          destroyed: @destroyed,
          last_saved_attributes: @__last_saved_attributes,
          level: 0
        }
        @_start_transaction_state[:level] += 1

        if @_committed_already_called
          @_new_record_before_last_commit = false
        else
          @_new_record_before_last_commit = @_start_transaction_state[:new_record]
        end
      end

      def clear_transaction_record_state
        return unless @_start_transaction_state
        @_start_transaction_state[:level] -= 1
        @_start_transaction_state = nil if @_start_transaction_state[:level] < 1
      end

      # The attribute values stay as written; what the record was before
      # the transaction comes back: new or persisted, destroyed, its id,
      # and the Dirty baseline (Rails restores the attributes' original
      # values and forgets the last save's changes).
      def restore_transaction_record_state(force_restore_state = false)
        if restore_state = @_start_transaction_state
          if force_restore_state || restore_state[:level] <= 1
            @persisted = restore_state[:persisted]
            @id_previously_changed = restore_state[:previously_new_record]
            @destroyed = restore_state[:destroyed]
            @__last_saved_attributes = restore_state[:last_saved_attributes]
            @saved_changes = nil
            self.id = restore_state[:id] if id != restore_state[:id]
          end
        end
      end
  end

  # no_touching.rb (activerecord 8.2.0.alpha): `Model.no_touching { }` makes
  # `touch` a no-op for the model and its subclasses inside the block,
  # per execution context. The class stack lives in
  # ActiveSupport::IsolatedExecutionState. `touch_written` is this
  # runtime's write-the-row form of `touch` and is held back the same way.
  module NoTouching
    module ClassMethods
      def no_touching(&block)
        NoTouching.apply_to(self, &block)
      end
    end

    class << self
      def apply_to(klass)
        klasses.push(klass)
        yield
      ensure
        klasses.pop
      end

      def applied_to?(klass)
        klasses.any? { |k| k >= klass }
      end

      private
        def klasses
          if defined?(ActiveSupport::IsolatedExecutionState) && ActiveSupport::IsolatedExecutionState.respond_to?(:[])
            ActiveSupport::IsolatedExecutionState[:active_record_no_touching_classes] ||= []
          else
            Thread.current[:active_record_no_touching_classes] ||= []
          end
        end
    end

    def no_touching(&block)
      NoTouching.apply_to(self.class, &block)
    end

    def touch(*, **)
      super unless no_touching?
    end

    def touch_written
      super unless no_touching?
    end

    def no_touching?
      NoTouching.applied_to?(self.class)
    end
  end

  class Base
    prepend Transactions
    prepend NoTouching
    extend NoTouching::ClassMethods

    class << self
      def run_commit_callbacks_on_first_saved_instances_in_transaction
        app = Rails.application
        if app.respond_to?(:active_record_run_commit_callbacks_on_first_saved_instances_in_transaction)
          app.active_record_run_commit_callbacks_on_first_saved_instances_in_transaction
        else
          true
        end
      end

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

# The ar_transaction_changes gem (1.1.11), which Core includes into its
# models: `transaction_changed_attributes`, each attribute changed since
# the record's last transaction finished, by name, with its value before
# that first change; an attribute written back to that value drops out.
# The commit and rollback callbacks read it, and it resets once they ran
# (Rails 8.1+: the gem resets in `_run_commit_callbacks` and
# `_run_rollback_callbacks`, which `committed!`/`rolledback!` call only
# for the instance that runs the callbacks).
#
# The gem's own file is not carried: ingest refuses it (a conditional
# nested in its load-time `ActiveRecord.version` conditional), and the
# gem records changes by wrapping `_write_attribute`/`write_attribute`,
# which a compiled model's attribute writers do not call. This is the
# same contract over the runtime's Dirty snapshot instead. The value
# before an attribute's first write since the reset is its value at the
# reset, so the reset keeps the snapshot (before the first one, the
# first save keeps the snapshot it starts from: the hydrated one, nothing
# for a new record), and the reader answers each attribute whose current value differs from it. A
# create also writes the id (Rails' `_create_record`), which the
# snapshot leaves out. The values are the attribute's typecast form (the
# gem's `_read_attribute`): a timestamp's stored text comes back as the
# time the reader answers. The emitted app reopens this module for the
# gem's VERSION.
module ArTransactionChanges
  def __track_saved_changes(was_new)
    unless @_transaction_changes_kept
      @_transaction_changes_baseline = @__last_saved_attributes
      @_transaction_changes_kept = true
    end
    @_transaction_changes_created = true if was_new
    super
  end

  def committed!(should_run_callbacks: true) # :nodoc:
    super
  ensure
    _reset_transaction_changes if should_run_callbacks
  end

  def rolledback!(force_restore_state: false, should_run_callbacks: true) # :nodoc:
    super
  ensure
    _reset_transaction_changes if should_run_callbacks
  end

  private
    def _reset_transaction_changes
      @_transaction_changes_baseline = _attribute_snapshot
      @_transaction_changes_kept = true
      @_transaction_changes_created = false
    end

    # A timestamp column's snapshot is its stored text; the reader (and
    # the gem's `_read_attribute`) answers the time. A typed_store
    # column's is its serialized text; the gem's `_read_attribute`
    # answers the store's Hash (`TypedStore.snapshot`, the generated
    # `__rh_<column>_keys` marks the column as a store).
    def _transaction_change_value(name, value)
      return value unless value.is_a?(String)
      return TypedStore.snapshot(self, name, value) if respond_to?(:"__rh_#{name}_keys", true)
      type = begin
        self.class.type_for_attribute(name)
      rescue NotImplementedError
        nil
      end
      return value unless type.respond_to?(:type) && type.type == :datetime
      ActiveSupport.present_db(ActiveSupport.parse_db_time(value))
    end

  # What the gem's own source, when the app carries it (Core does: its
  # module body reopens this one), would shadow or read past: the
  # runtime's reader answers (the gem's records writes through
  # `_write_attribute`, which a compiled model's writers do not call),
  # folding in what the gem did note (`attribute_will_change!` and
  # `write_attribute` on this record, into the store its own methods see
  # through the same reader); and the gem's private readers find
  # the record's attribute, as Rails' `@attributes[name]` answers, in a
  # view bound to the record (RecordAttributeSet).
  module RuntimeSurface
    def transaction_changed_attributes
      # The gem's own bookkeeping (`attribute_will_change!`,
      # `_store_transaction_changed_attributes`) reads and writes its
      # store through this reader, as in the gem.
      return (@transaction_changed_attributes ||= {}) if @_transaction_changes_noting
      baseline = @_transaction_changes_kept ? @_transaction_changes_baseline : @__last_saved_attributes
      changed = HashWithIndifferentAccess.new
      changed["id"] = nil if @_transaction_changes_created
      _attribute_snapshot.each do |name, value|
        before = baseline.nil? ? nil : baseline[name]
        changed[name] = _transaction_change_value(name, before) unless before == value
      end
      noted = @transaction_changed_attributes
      noted.each { |name, value| changed[name] = value unless changed.key?(name) } unless noted.nil?
      changed
    end

    def attribute_will_change!(attr_name)
      _noting_transaction_changes { super }
    end

    private
      def _noting_transaction_changes
        outer = @_transaction_changes_noting
        @_transaction_changes_noting = true
        yield
      ensure
        @_transaction_changes_noting = outer
      end

      def _store_transaction_changed_attributes(attr_name, &block)
        _noting_transaction_changes { super }
      end

      def _read_attribute_for_transaction(attr_name)
        _bind_attribute_set
        super
      end

      def _deserialize_transaction_change_value(attr_name, value)
        _bind_attribute_set
        super
      end

      def _bind_attribute_set
        set = @attributes
        @attributes = ActiveRecord::RecordAttributeSet.new(self) unless set.is_a?(ActiveRecord::RecordAttributeSet) && set.record.equal?(self)
      end
  end
  prepend RuntimeSurface
end

module ActiveRecord
  # `@attributes` as the gem and Rails' dirty code read it, over a
  # compiled model that keeps its columns in slots: `[name]` answers the
  # attribute with the column's type, the value as the app last assigned
  # it (`came_from_user?`, so `value_before_type_cast` is serialized
  # through the type, as Rails does for an assigned attribute) and the
  # value cast. A name the model has no column for answers a null
  # attribute.
  class RecordAttributeSet # :nodoc:
    attr_reader :record

    def initialize(record)
      @record = record
    end

    def [](name)
      name = name.to_s
      type = begin
        @record.class.type_for_attribute(name)
      rescue NotImplementedError
        nil
      end
      return ActiveModel::Attribute.null(name) if type.nil?
      ActiveModel::Attribute.from_user(name, @record[name], type)
    end

    def key?(name)
      @record.has_attribute?(name)
    end

    def keys
      @record.attribute_names
    end
  end
end
