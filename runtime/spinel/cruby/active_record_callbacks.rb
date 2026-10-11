# frozen_string_literal: true

# ActiveRecord::Base's callback registration called at runtime, on the
# PostgreSQL CRuby tree, from the activerecord, activemodel and
# activesupport native Core runs (Shopify/rails 5813559f4aed, 8.2.0.alpha:
# active_record/callbacks.rb and transactions.rb's ClassMethods,
# active_model/callbacks.rb, active_model/validations/callbacks.rb,
# active_support/callbacks.rb). A compiled model's own `before_save :x`
# is lowered into its `before_save` hook method; a call the compiler
# cannot see through runs here. Core's
# Podding::Database::Concerns::ShardedTable::InstanceMethods registers
# `base.before_create(:shop_id_presence_constraint_check)` and
# `base.after_find(:_check_shop_lock)` from its `included` hook on
# GlobalDB::ShopRecord, whose subclasses inherit them; Auditable,
# CustomerPasswordProtection and HasSyncedActiveRecordMetafields register
# from class methods and `included` hooks the same way.
#
# The registration methods are Base's class methods: before_/around_/
# after_ save, create, update and destroy; after_initialize, after_find
# and after_touch;
# before_validation and after_validation; after_commit, after_save_commit,
# after_create_commit, after_update_commit, after_destroy_commit and
# after_rollback. Each takes Rails' filters (a method name, a block or
# proc run with `instance_exec` by its arity, or an object answering
# `<kind>_<event>(record)`) and options (`if:`, `unless:`, `prepend:`;
# `on:` / `except_on:` for validation callbacks; `on:` for commit and
# rollback callbacks), and keeps them in a chain per class and event that
# a subclass inherits, as ActiveSupport::Callbacks does: a before
# callback appends (prepends with `prepend: true`), an after model
# callback prepends so the after callbacks run in declaration order, an
# after callback does not run when the event's result is false, a
# repeated method name replaces the earlier entry.
#
# The chains run where Rails runs them:
# - save: the validation callbacks around `valid?`, then the save chain
#   around the create or update chain around the write
#   (runtime/active_record/base.rb's `save_after_validation`, of which
#   `_runtime_save_after_validation` below is a copy with the chains run
#   in place of the hook calls). `throw :abort` in a before callback
#   halts the chain: the write is skipped, `save` answers false, `save!`,
#   `create!` and `update!` raise ActiveRecord::RecordNotSaved (Rails'
#   "Failed to save the record"), and a halted validation chain makes the
#   record invalid.
# - destroy (base.rb's `destroy`): halted, `destroy` answers false and
#   `destroy!` raises ActiveRecord::RecordNotDestroyed.
# - touch: the touch chain's after callbacks.
# - commit and rollback (base.rb's `_run_commit_hooks` and
#   `_transaction_rolled_back`, which runtime/active_record_transactions.rb
#   calls once the transaction finishes).
# - find: `mark_persisted!` of a record that was not yet persisted, which
#   is what every hydration path (`instantiate`, `from_stmt`) calls once
#   the columns are read and before `after_initialize`, as Rails runs find
#   callbacks before initialize callbacks. A reload re-marks a persisted
#   record and runs none, as Rails' reload runs them on a fresh object it
#   discards.
# - initialize: the end of `new`, after the constructor assigned the
#   attributes and yielded to its block, and the end of a hydration
#   (`instantiate`, `from_stmt`), after the find chain, as Rails' `new`
#   and `init_with_attributes` run `_run_initialize_callbacks`. The first
#   runtime `after_initialize` on a class wraps those class methods, and
#   `after_initialize` itself, on it and on every subclass
#   (InitializeHooks): `new` inside a hydration factory builds the empty
#   shell the factory fills, and runs nothing; a class whose compiled
#   code already calls its declared `after_initialize` hook runs the
#   chain from that call, once. state_machines-audit_trail's
#   `audit_trail` registers its initial-transition log from a
#   `state_machine` block (Core's Sales::Subscriptions::ContractIdentity).
# A readonly record runs the save (or destroy) chain up to the write,
# which raises ActiveRecord::ReadOnlyRecord, as Rails raises it inside the
# chain (the Persistence methods of runtime/active_record_core.rb are
# redefined below for it).
#
# The model's compiled hook (`before_save`, `after_save`, ...; for commit
# and rollback base.rb's whole sequence of compiled hooks) is one entry of
# the chain, placed with the class that defines it: after the entries
# registered on that class and its superclasses, before those registered
# on its subclasses. The compiled hook is the callbacks the class body
# declares, and a concern's `included` hook runs where the class body
# includes it, usually above the declarations.
#
# Differences, at the edges:
# - The chains are applied in class order, superclass first, rather than
#   in the order the registrations ran: a callback a superclass registers
#   after its subclass was defined runs before the subclass's, where
#   Rails appends it to the subclass's chain.
# - A concern included below the class's own callback declarations still
#   runs its callbacks ahead of them (the compiled hook keeps no position
#   among the runtime registrations).
# - The validation callbacks run inside `save` and `save!`, around
#   `valid?`, as the compiled ones do (runtime/active_record/connection.rb's
#   `save`); a direct `valid?` runs neither, where Rails runs both.
# - `before_commit` registered at runtime raises NotImplementedError
#   naming the class: the transaction calls its hook directly, with no
#   chain to join.
# - `dup` and STI `becomes` build through `new`, so a class with runtime
#   `after_initialize` callbacks runs them there, where Rails' `dup` runs
#   them on the copy and `becomes` runs none.
# - The after-commit order reads `run_after_transaction_callbacks_in_order_defined`
#   from `Rails.application.active_record_run_after_transaction_callbacks_in_order_defined`
#   when the application answers it, and Rails' default (false, as Core
#   leaves it) otherwise.
# - STI `becomes` (lower::sti_scope) marks its copy persisted through
#   `mark_persisted!`, so the find callbacks run on it, where Rails'
#   `becomes` runs none.
module ActiveRecord
  module Callbacks
    # active_record/transactions.rb ACTIONS.
    TRANSACTION_ACTIONS = [:create, :destroy, :update].freeze

    # The chains, by event: their `skip_after_callbacks_if_terminated`
    # (define_model_callbacks and the validation callbacks set it; the
    # transaction callbacks do not), whether an after callback runs only
    # when the event's result is not false (define_model_callbacks'), and
    # the compiled hooks that join them.
    CHAINS = {
      validation: { skip_after_if_halted: true, before: :before_validation, after: :after_validation },
      save: { skip_after_if_halted: true, model: true, before: :before_save, after: :after_save },
      create: { skip_after_if_halted: true, model: true, before: :before_create, after: :after_create },
      update: { skip_after_if_halted: true, model: true, before: :before_update, after: :after_update },
      destroy: { skip_after_if_halted: true, model: true, before: :before_destroy, after: :after_destroy },
      touch: { skip_after_if_halted: true, model: true, after: :after_touch },
      find: { skip_after_if_halted: true, model: true },
      initialize: { skip_after_if_halted: true, model: true, after: :after_initialize },
      commit: {
        skip_after_if_halted: false,
        compiled: :_compiled_commit_hooks,
        defined_by: [:_run_commit_callbacks, :after_create_commit, :after_update_commit,
                     :after_destroy_commit, :after_save_commit, :after_commit],
      },
      rollback: {
        skip_after_if_halted: false,
        compiled: :_compiled_rollback_hooks,
        defined_by: [:_run_rollback_callbacks, :after_rollback],
      },
    }.freeze

    @generation = 0

    class << self
      attr_reader :generation

      def changed! # :nodoc:
        @generation += 1
      end

      # Wraps `klass` and its subclasses for the initialize chain, once
      # per class (Base's `inherited` wraps the subclasses defined later).
      def install_initialize_hooks(klass) # :nodoc:
        return if klass.instance_variable_get(:@__initialize_hooks)
        klass.instance_variable_set(:@__initialize_hooks, true)
        klass.prepend(InitializeHooks)
        klass.singleton_class.prepend(InitializeHooks.class_side(klass))
        klass.subclasses.each { |sub| install_initialize_hooks(sub) }
      end

      # The class whose hydration factory is running on this thread: its
      # `new` builds the shell the factory fills.
      def hydrating # :nodoc:
        Thread.current[:__roundhouse_hydrating]
      end

      def hydrate(klass) # :nodoc:
        previous = Thread.current[:__roundhouse_hydrating]
        Thread.current[:__roundhouse_hydrating] = klass
        yield
      ensure
        Thread.current[:__roundhouse_hydrating] = previous
      end

      # The chain `klass` runs for `event`, rebuilt when a registration
      # anywhere changed the chains.
      def chain(klass, event) # :nodoc:
        cache = klass.instance_variable_get(:@__callback_chains)
        cache = klass.instance_variable_set(:@__callback_chains, {}) if cache.nil?
        hit = cache[event]
        return hit[1] if hit && hit[0] == @generation
        built = build_chain(klass, event)
        cache[event] = [@generation, built]
        built
      end

      def after_transaction_callbacks_in_order? # :nodoc:
        app = defined?(::Rails) && ::Rails.respond_to?(:application) ? ::Rails.application : nil
        if app.respond_to?(:active_record_run_after_transaction_callbacks_in_order_defined)
          app.active_record_run_after_transaction_callbacks_in_order_defined
        else
          false
        end
      end

      private
        # The superclass chain from Base down to `klass`; at each class
        # its registrations in order, then the compiled hooks it defines.
        def build_chain(klass, event)
          config = CHAINS.fetch(event)
          lineage = []
          c = klass
          while c
            lineage.unshift(c)
            break if c.equal?(ActiveRecord::Base)
            c = c.superclass
          end
          compiled = compiled_entries(klass, event, config)
          chain = []
          lineage.each do |c|
            if c.equal?(ActiveRecord::Base)
              compiled.each { |entry, owner| apply(chain, entry, entry.prepend?) if owner.nil? }
            end
            ops = c.instance_variable_get(:@__callback_ops)
            ops[event]&.each { |entry| apply(chain, entry, entry.prepend?) } if ops
            compiled.each { |entry, owner| apply(chain, entry, entry.prepend?) if owner.equal?(c) }
          end
          chain.freeze
        end

        # CallbackChain#append_one / #prepend_one with remove_duplicates.
        def apply(chain, entry, prepend)
          chain.delete_if { |other| entry.duplicates?(other) }
          prepend ? chain.unshift(entry) : chain.push(entry)
        end

        # [entry, owning class] for each compiled hook of the event that a
        # class below Base defines (owner nil: a module Base includes).
        def compiled_entries(klass, event, config)
          entries = []
          if config[:compiled]
            names = config[:defined_by]
            prepend = after_transaction_callbacks_in_order?
            entries << [Compiled.new(:after, config[:compiled], prepend, false), compiled_owner(klass, names)]
          else
            [:before, :after].each do |kind|
              name = config[kind]
              next if name.nil?
              owner = compiled_owner(klass, [name])
              next if owner.nil? && klass.instance_method(name).owner.then { |o| o.equal?(ActiveRecord::Base) || o.equal?(InitializeHooks) }
              after = kind == :after
              entries << [Compiled.new(kind, name, after, after && config[:model]), owner]
            end
          end
          entries
        end

        # The class in `klass`'s superclass chain, below Base, that
        # defines one of `names` itself or through a module it includes.
        def compiled_owner(klass, names)
          c = klass
          until c.nil? || c.equal?(ActiveRecord::Base)
            parent = c.superclass
            own = parent ? c.ancestors - parent.ancestors : c.ancestors
            if own.any? { |m| !m.equal?(InitializeHooks) && names.any? { |n| m.method_defined?(n, false) || m.private_method_defined?(n, false) } }
              return c
            end
            c = parent
          end
          nil
        end
    end

    # A callback in a chain: ActiveSupport::Callbacks::Callback with its
    # CallTemplate.
    class Entry # :nodoc:
      attr_reader :kind, :filter

      def initialize(event, kind, filter, if_conditions, unless_conditions, prepend)
        @event = event
        @kind = kind
        @filter = filter
        @if = if_conditions
        @unless = unless_conditions
        @prepend = prepend
        @scope = :"#{kind}_#{event}"
      end

      def prepend?
        @prepend
      end

      # Callback#duplicates?: a method name replaces its earlier entry of
      # the same kind.
      def duplicates?(other)
        @filter.is_a?(Symbol) && other.is_a?(Entry) && other.kind == @kind && other.filter == @filter
      end

      def applies?(record, value)
        @if.all? { |c| condition(c, record, value) } && @unless.none? { |c| condition(c, record, value) }
      end

      def call(record, &block)
        case @filter
        when Symbol
          record.send(@filter, &block)
        when ::Proc
          case @filter.arity
          when 2
            raise ArgumentError unless block
            record.instance_exec(record, block, &@filter)
          when 1, -2
            record.instance_exec(record, &@filter)
          else
            record.instance_exec(&@filter)
          end
        else
          @filter.send(@scope, record, &block)
        end
      end

      private
        def condition(c, record, value)
          case c
          when Symbol
            record.send(c)
          when ValueCondition
            c.call(value)
          when ::Proc
            case c.arity
            when 2
              raise ArgumentError
            when 1, -2
              record.instance_exec(record, &c)
            else
              record.instance_exec(&c)
            end
          else
            c.send(@scope, record)
          end
        end
    end

    # Conditionals::Value: the after model callbacks' `v != false`.
    class ValueCondition # :nodoc:
      def initialize(&block)
        @block = block
      end

      def call(value)
        @block.call(value)
      end
    end

    NOT_FALSE = ValueCondition.new { |v| v != false }

    # The compiled hook's entry: the hook method, run as a method callback.
    class Compiled # :nodoc:
      attr_reader :kind

      def initialize(kind, name, prepend, after_model)
        @kind = kind
        @name = name
        @prepend = prepend
        @after_model = after_model
      end

      def filter
        nil
      end

      def prepend?
        @prepend
      end

      def duplicates?(_other)
        false
      end

      def applies?(_record, value)
        !@after_model || value != false
      end

      def call(record)
        record.send(@name)
      end
    end

    # The initialize chain's entry points on a class with runtime
    # `after_initialize` callbacks (Callbacks.install_initialize_hooks).
    # Prepended to each such class, so a subclass's compiled
    # `after_initialize` is wrapped too: the outermost runs the chain, in
    # which the compiled hook's entry calls through to the compiled method.
    module InitializeHooks # :nodoc:
      def after_initialize
        return super if @__initialize_running
        _run_initialize_chain
      end

      # The class methods for `owner`; a copy prepended to a superclass
      # answers the subclass's calls by passing them on.
      def self.class_side(owner)
        Module.new do
          define_method(:new) do |*args, **kwargs, &block|
            return super(*args, **kwargs, &block) unless equal?(owner)
            record = super(*args, **kwargs, &block)
            return record if Callbacks.hydrating.equal?(self)
            record.send(:_finish_initialize)
            record
          end

          define_method(:from_row) do |row|
            return super(row) unless equal?(owner)
            Callbacks.hydrate(self) { super(row) }
          end

          [:instantiate, :from_stmt].each do |factory|
            define_method(factory) do |source|
              return super(source) unless equal?(owner)
              record = Callbacks.hydrate(self) { super(source) }
              record.send(:_finish_initialize) if record.is_a?(ActiveRecord::Base)
              record
            end
          end
        end
      end
    end

    # Run state: ActiveSupport::Callbacks::Filters::Environment.
    Environment = Struct.new(:halted, :value) # :nodoc:

    module ClassMethods
      MODEL_CALLBACK_KEYS = [:if, :unless, :prepend].freeze

      # define_model_callbacks :save, :create, :update, :destroy
      # (active_model/callbacks.rb `_define_*_model_callback`).
      [:save, :create, :update, :destroy].each do |event|
        define_method(:"before_#{event}") do |*args, **options, &block|
          _model_callback(event, :before, args, options, block)
        end
        define_method(:"around_#{event}") do |*args, **options, &block|
          _model_callback(event, :around, args, options, block)
        end
        define_method(:"after_#{event}") do |*args, **options, &block|
          _model_callback(event, :after, args, options, block)
        end
      end

      # define_model_callbacks :initialize, :find, :touch, only: :after
      def after_find(*args, **options, &block)
        _model_callback(:find, :after, args, options, block)
      end

      def after_touch(*args, **options, &block)
        _model_callback(:touch, :after, args, options, block)
      end

      def after_initialize(*args, **options, &block)
        Callbacks.install_initialize_hooks(self)
        _model_callback(:initialize, :after, args, options, block)
      end

      # active_model/validations/callbacks.rb
      def before_validation(*args, &block)
        options = _callback_options(args).dup
        _validation_callback_options(options)
        _set_callback(:validation, :before, args, options, block)
      end

      def after_validation(*args, &block)
        options = _callback_options(args).dup
        options[:prepend] = true
        _validation_callback_options(options)
        _set_callback(:validation, :after, args, options, block)
      end

      # active_record/transactions.rb ClassMethods
      def before_commit(*args, &block)
        raise NotImplementedError, "#{name}.before_commit called at runtime: the transaction runs the " \
          "class's declared before_commit hook only"
      end

      def after_commit(*args, &block)
        _transaction_callback(:commit, args, {}, block)
      end

      def after_save_commit(*args, &block)
        _transaction_callback(:commit, args, { on: [:create, :update] }, block)
      end

      def after_create_commit(*args, &block)
        _transaction_callback(:commit, args, { on: :create }, block)
      end

      def after_update_commit(*args, &block)
        _transaction_callback(:commit, args, { on: :update }, block)
      end

      def after_destroy_commit(*args, &block)
        _transaction_callback(:commit, args, { on: :destroy }, block)
      end

      def after_rollback(*args, &block)
        _transaction_callback(:rollback, args, {}, block)
      end

      private
        def _callback_options(args)
          last = args.last
          last.is_a?(Hash) && last.instance_of?(Hash) ? args.pop : {}
        end

        def _model_callback(event, kind, args, options, block)
          options.each_key do |key|
            unless MODEL_CALLBACK_KEYS.include?(key)
              raise ArgumentError, "Unknown key: #{key.inspect}. Valid keys are: " \
                "#{MODEL_CALLBACK_KEYS.map(&:inspect).join(", ")}"
            end
          end
          options = options.dup
          if kind == :after
            options[:prepend] = true
            options[:if] = Array(options[:if]) + [NOT_FALSE]
          end
          _set_callback(event, kind, args, options, block)
        end

        def _validation_callback_options(options)
          if options.key?(:on)
            on = options[:on] = Array(options[:on])
            options[:if] = [->(o) { on.intersect?(Array(o.validation_context)) }, *options[:if]]
          end
          if options.key?(:except_on)
            except_on = options[:except_on] = Array(options[:except_on])
            options[:unless] = [->(o) { except_on.intersect?(Array(o.validation_context)) }, *options[:unless]]
          end
        end

        # set_options_for_callbacks! and its enforced prepend option.
        def _transaction_callback(event, args, enforced, block)
          options = _callback_options(args)
          if Callbacks.after_transaction_callbacks_in_order?
            enforced = enforced.merge(prepend: !options[:prepend])
          end
          options = options.merge(enforced)
          if options[:on]
            fire_on = Array(options[:on]).freeze
            if (fire_on - TRANSACTION_ACTIONS).any?
              raise ArgumentError, ":on conditions for after_commit and after_rollback callbacks have to be one of #{TRANSACTION_ACTIONS}"
            end
            options[:if] = [-> { _transaction_include_any_action?(fire_on) }, *options[:if]]
          end
          _set_callback(event, :after, args, options, block)
        end

        # ActiveSupport::Callbacks.set_callback over this class's chain.
        def _set_callback(event, kind, filters, options, block)
          filters = filters.dup
          filters.unshift(block) if block
          if_conditions = _conditionals(options[:if])
          unless_conditions = _conditionals(options[:unless])
          entries = filters.map do |filter|
            if filter.is_a?(String)
              raise ArgumentError, "Passing string to define a callback is not supported. See the `.set_callback` " \
                "documentation to see supported values."
            end
            Entry.new(event, kind, filter, if_conditions, unless_conditions, options[:prepend] ? true : false)
          end
          ops = (@__callback_ops ||= {})
          (ops[event] ||= []).concat(entries)
          Callbacks.changed!
          nil
        end

        def _conditionals(conditionals)
          return [] if conditionals.nil? || (conditionals.respond_to?(:empty?) && conditionals.empty?)
          conditionals = Array(conditionals)
          if conditionals.any?(String)
            raise ArgumentError, "Passing string to be evaluated in :if and :unless conditional options is not " \
              "supported. Pass a symbol for an instance method, or a lambda, proc or block, instead."
          end
          conditionals.freeze
        end
    end
  end

  class Base
    extend Callbacks::ClassMethods

    class << self
      prepend(Module.new do
        def inherited(subclass)
          super
          Callbacks.install_initialize_hooks(subclass) if instance_variable_get(:@__initialize_hooks)
        end
      end)
    end

    # The methods below replace base.rb's and connection.rb's (and the
    # writes active_record_core.rb captured), without a redefinition
    # warning.
    [:save, :save!, :destroy!, :mark_persisted!, :_run_commit_hooks, :_transaction_rolled_back,
     :_runtime_save_after_validation, :_runtime_destroy, :_runtime_touch, :_runtime_touch_written].each do |name|
      remove_method(name) if method_defined?(name, false) || private_method_defined?(name, false)
    end
    singleton_class.send(:remove_method, :create!) if singleton_class.method_defined?(:create!, false)

    # Rails' `create!` saves with `save!` (`object.save!`), so a halted
    # save raises RecordNotSaved.
    def self.create!(attrs = {})
      instance = new(attrs)
      instance.save!
      instance
    end

    # runtime/active_record/connection.rb's `save`, with the validation
    # chain around `valid?` in the validation context `valid?` will use.
    #
    # `validate: false` (validations.rb `perform_validations`) skips the
    # validation chain, and so its callbacks, and saves.
    def save(context: nil, validate: true)
      return save_after_validation if validate == false

      previous = @validation_context
      @validation_context = context || (new_record? ? :create : :update)
      # Validations#valid? clears the errors before the chain runs.
      @errors = _fresh_errors
      begin
        ok = _callback_chain_run(:validation) { valid?(context) }
      ensure
        @validation_context = previous
      end
      return false unless ok

      save_after_validation
    end

    # Persistence#save!: RecordInvalid for an invalid record,
    # RecordNotSaved when the save chain did not write.
    def save!(context: nil, validate: true)
      @_create_or_update_failed = false
      return self if save(context: context, validate: validate)
      raise RecordNotSaved.new("Failed to save the record", self) if @_create_or_update_failed
      raise RecordInvalid, self
    end

    # Persistence#destroy! / #_raise_record_not_destroyed.
    def destroy!
      destroy || raise(RecordNotDestroyed.new("Failed to destroy #{self.class} with #{self.class.primary_key}=#{id}", self))
    end

    # base.rb's `mark_persisted!`; a record loaded from the database runs
    # the find callbacks.
    def mark_persisted!
      loaded = !@persisted
      @persisted = true
      @destroyed = false
      _callback_chain_run(:find) { true } if loaded
      nil
    end

    # base.rb's `_run_commit_hooks` and `_transaction_rolled_back`, as the
    # compiled entries of the commit and rollback chains.
    def _run_commit_hooks
      _callback_chain_run(:commit) { true }
      nil
    end

    def _transaction_rolled_back
      _callback_chain_run(:rollback) { true }
      nil
    end

    private
      # A copy of runtime/active_record/base.rb's `save_after_validation`
      # (change both), with the save chain around the create or update
      # chain around the write (create_or_update, _create_record,
      # _update_record).
      def _runtime_save_after_validation
        _transaction_write_begin
        result = _callback_chain_run(:save) do
          was_new = new_record?
          written =
            if was_new
              _callback_chain_run(:create) do
                fill_timestamps(true)
                self.id = _adapter_insert
                @persisted = true
                __track_saved_changes(was_new)
                true
              end
            else
              _callback_chain_run(:update) do
                # Timestamp#should_record_timestamps?: with partial updates
                # (the default) a save with nothing pending stamps nothing,
                # so `saved_changes?` stays false for it.
                fill_timestamps(false) if has_changes_to_save?
                _adapter_update
                @_trigger_update_callback = true
                __track_saved_changes(was_new)
                true
              end
            end
          written != false
        end
        _transaction_write_end
        @_create_or_update_failed = true unless result
        result
      end

      # A copy of base.rb's `destroy` (change both), in the destroy chain,
      # with Callbacks#destroy's guard against a callback destroying the
      # record again.
      def _runtime_destroy
        return self unless persisted?
        @_destroy_callback_already_called ||= false
        return true if @_destroy_callback_already_called
        begin
          @_destroy_callback_already_called = true
          _transaction_write_begin
          result = _callback_chain_run(:destroy) do
            _adapter_delete
            @_trigger_destroy_callback = true
            @persisted = false
            @destroyed = true
            true
          end
          _transaction_write_end
          result ? self : result
        ensure
          @_destroy_callback_already_called = false
        end
      end

      # Copies of base.rb's `touch` and `touch_written` (change both), in
      # the touch chain: the bare form writes `updated_at` alone, the
      # written form the whole row with the call site's column.
      def _runtime_touch
        _runtime_touch_with { _adapter_touch }
      end

      def _runtime_touch_written
        _runtime_touch_with { _adapter_update }
      end

      def _runtime_touch_with
        _transaction_write_begin
        _callback_chain_run(:touch) do
          fill_timestamps(false)
          yield
          @_trigger_update_callback = true if self.class.schema_columns.include?(:updated_at)
          true
        end
        _transaction_write_end
        true
      end

      # base.rb's `_run_commit_hooks` and `_transaction_rolled_back`
      # bodies (change both).
      def _compiled_commit_hooks
        _run_commit_callbacks
        after_create_commit if _transaction_include_create?
        after_update_commit if _transaction_include_update?
        after_destroy_commit if _transaction_include_destroy?
        after_save_commit if _transaction_include_create? || _transaction_include_update?
        after_commit
      end

      def _compiled_rollback_hooks
        _run_rollback_callbacks
        after_rollback
      end

      # Transactions#transaction_include_any_action?.
      def _transaction_include_any_action?(actions)
        actions.any? do |action|
          case action
          when :create then _transaction_include_create?
          when :update then _transaction_include_update?
          when :destroy then _transaction_include_destroy?
          end
        end
      end

      # The initialize chain, from the wrapped `after_initialize` (the
      # compiled code's call of its declared hook) or from the end of
      # `new` or a hydration that made none.
      def _run_initialize_chain
        @__initialize_running = true
        _callback_chain_run(:initialize) { true }
        @__initialize_ran = true
        nil
      ensure
        remove_instance_variable(:@__initialize_running)
      end

      def _finish_initialize
        if defined?(@__initialize_ran)
          remove_instance_variable(:@__initialize_ran)
        else
          _run_initialize_chain
          remove_instance_variable(:@__initialize_ran) if defined?(@__initialize_ran)
        end
      end

      # ActiveSupport::Callbacks#run_callbacks: the chain's value, false
      # when a before callback halted it.
      def _callback_chain_run(event, &body)
        chain = Callbacks.chain(self.class, event)
        return body.call if chain.empty?
        env = Callbacks::Environment.new(false, nil)
        _callback_chain_step(chain, 0, env, Callbacks::CHAINS[event][:skip_after_if_halted], body)
        env.value
      end

      def _callback_chain_step(chain, index, env, skip_after_if_halted, body)
        if index == chain.length
          env.value = !env.halted && body.call
          return
        end
        entry = chain[index]
        case entry.kind
        when :before
          if !env.halted && entry.applies?(self, env.value)
            halted = true
            catch(:abort) do
              entry.call(self)
              halted = false
            end
            env.halted = halted
          end
          _callback_chain_step(chain, index + 1, env, skip_after_if_halted, body)
        when :after
          _callback_chain_step(chain, index + 1, env, skip_after_if_halted, body)
          if !(skip_after_if_halted && env.halted) && entry.applies?(self, env.value)
            entry.call(self)
          end
        else
          if env.halted || !entry.applies?(self, env.value)
            _callback_chain_step(chain, index + 1, env, skip_after_if_halted, body)
          else
            entry.call(self) do
              _callback_chain_step(chain, index + 1, env, skip_after_if_halted, body)
              env.value
            end
          end
        end
      end
  end

  # runtime/active_record_core.rb's Persistence: a readonly record runs
  # the save or destroy chain, whose write raises ReadOnlyRecord, as
  # Persistence#create_or_update and #destroy raise inside Rails' chain.
  module Persistence
    remove_method(:save_after_validation, :destroy)

    def save_after_validation
      return _callback_chain_run(:save) { _raise_readonly_record_error } if readonly?
      _runtime_save_after_validation
    end

    def destroy
      return _callback_chain_run(:destroy) { _raise_readonly_record_error } if readonly?
      _runtime_destroy
    end
  end

  class Relation
    remove_method(:create!) if method_defined?(:create!, false)

    # Relation#create!: `save!`, as Base.create! above.
    def create!(attrs = {})
      record = build(attrs)
      record.save!
      record
    end
  end
end
