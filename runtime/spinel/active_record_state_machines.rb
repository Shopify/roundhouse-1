# frozen_string_literal: true

# What the state_machines integration (state_machines-activemodel /
# -activerecord, loaded natively from the gem home) expects of
# ActiveRecord::Base that the runtime's base does not otherwise answer,
# for a model whose `state_machine` the lowered class body runs
# (`lower::state_machines`).
#
# The `:validation` chain, the around kind: `set_callback(:validation,
# :around, filter, prepend: true)`, which state_machines-activemodel's
# `define_validation_hook` registers for a machine whose action is
# `:save` (the machine is the filter; Rails calls its
# `around_validation(record) { ... }`). Rails' ActiveRecord::Base
# includes ActiveSupport::Callbacks and defines the chain
# (activemodel/lib/active_model/validations/callbacks.rb); the runtime
# base runs `before_validation` / `after_validation` from `save` and the
# validate chain from `valid?`, so the around callbacks wrap the latter:
# `valid?` clears the errors, then runs each around callback, outermost
# first, the validations innermost. A callback that does not yield
# halts the chain and the record is invalid with the errors the callback
# added, as Rails' halted `run_validations!` answers false. Only the
# `:validation` `:around` kind with object or Symbol filters and no
# `if:` / `unless:` is the runtime's; any other `set_callback` on a model
# that does not include ActiveSupport::Callbacks itself raises by name.
#
# The initial states. The integration's `initialize(attributes = nil, *)`
# helper runs `initialize_states` in the block it passes to Rails'
# `Base#initialize`, which yields once the attributes are assigned. An
# emitted model defines its own `initialize` (column defaults, then the
# attributes), which comes before the gem's helper module and does not
# yield, so a class that calls `state_machine`, and each subclass, is
# given StateInitializer, which runs `initialize_states` after the
# model's own `initialize` with the attributes it was given, as Rails
# does. A record a hydration factory loads (`HYDRATE_ATTRS`) is left
# alone: Rails does not run `initialize` for it.

module ActiveRecord
  class Base
    def self.set_callback(name, kind, *filters, **options, &block)
      prepend = options.delete(:prepend)
      if name != :validation || kind != :around || !block.nil? || !options.empty? || filters.empty? ||
          !filters.all? { |f| f.is_a?(Symbol) || f.respond_to?(:around_validation) }
        raise NotImplementedError,
          "roundhouse: `#{self.name}.set_callback(#{name.inspect}, #{kind.inspect}, ...)` is not supported: " \
          "an emitted model without ActiveSupport::Callbacks runs only :validation :around callbacks " \
          "given as objects or Symbols (runtime/active_record_state_machines.rb)"
      end
      chain = _validation_around_callbacks.dup
      prepend ? chain.unshift(*filters) : chain.concat(filters)
      @_validation_around_callbacks = chain
      nil
    end

    # The class's around-validation filters, outermost first; a subclass
    # without its own reads its superclass's, so a filter set on the
    # parent later still runs there.
    def self._validation_around_callbacks
      own = @_validation_around_callbacks
      return own unless own.nil?
      superclass.respond_to?(:_validation_around_callbacks) ? superclass._validation_around_callbacks : []
    end
  end

  module ValidationAroundCallbacks
    def valid?(context = nil)
      chain = self.class._validation_around_callbacks
      return super if chain.empty?
      @errors = _fresh_errors
      ran = false
      result = false
      inner = lambda do
        ran = true
        result = super(context)
      end
      run = chain.reverse.inject(inner) do |nested, filter|
        if filter.is_a?(Symbol)
          -> { send(filter) { nested.call } }
        else
          -> { filter.around_validation(self) { nested.call } }
        end
      end
      run.call
      ran && result
    end
  end

  Base.prepend(ValidationAroundCallbacks)

  module StateInitializer
    def initialize(attrs = Base::EMPTY_ATTRS, *rest)
      super
      return if attrs.equal?(Base::HYDRATE_ATTRS)
      self.class.state_machines.initialize_states(self, {}, attrs.nil? ? {} : attrs.to_h)
    end
  end

  module StateMachineClassHooks
    def state_machine(*args, **options, &block)
      machine = super
      prepend(StateInitializer) unless ancestors.include?(StateInitializer)
      machine
    end

    def inherited(subclass)
      super
      subclass.prepend(StateInitializer) if ancestors.include?(StateInitializer)
    end
  end

  Base.singleton_class.prepend(StateMachineClassHooks)
end
