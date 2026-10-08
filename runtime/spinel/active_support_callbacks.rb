# frozen_string_literal: true

# ActiveSupport::Callbacks, ported from the activesupport native Core runs
# (Shopify/rails 5813559f4aed, 8.2.0.alpha: lib/active_support/callbacks.rb)
# for the CRuby and JRuby trees, carried where app code names the module.
# Core includes it in plain classes and in concerns' `included` blocks
# (Plus::Plan::Change, Payments::Vault::AfterRevocation and AroundRedaction)
# and uses `define_callbacks`, `set_callback` (:before / :after / :around;
# symbol, proc and block filters; `if:` / `unless:`; `prepend:`),
# `skip_callback` and `run_callbacks` with a block. Each method follows the
# native one; the chain, sequence, call templates and filters are the
# native classes.
#
# Differences, at the edges:
# - ActiveSupport::Concern is not used: the `included` block (`extend
#   ActiveSupport::DescendantsTracker` and `class_attribute :__callbacks,
#   instance_writer: false, instance_predicate: false, default: {}.freeze`)
#   is written out as Concern#append_features. `__callbacks` is a class-level
#   value a subclass inherits until it assigns its own, which is what
#   class_attribute's redefined reader answers.
# - DescendantsTracker is its own port (active_support_descendants_tracker.rb).
# - ActiveSupport::Ractors answers each of its calls with its argument,
#   as it does when `unshareable_proc_action` is unset (the default), so
#   the calls are left out. `freeze` freezes the chains it would make
#   shareable.
# - `define_callbacks` defines its five methods with define_method rather
#   than a `module_eval` string.

require_relative "active_support_descendants_tracker"

module ActiveSupport
  module Callbacks
    # `included do ... end`, run as Concern#append_features runs it: once,
    # and not again for a class that already has the module.
    def self.append_features(base)
      return false if base < self
      super
      base.extend ClassMethods
      base.extend ActiveSupport::DescendantsTracker
      base.instance_variable_set(:@__callbacks, {}.freeze)
    end

    CALLBACK_FILTER_TYPES = [:before, :after, :around].freeze

    # The class_attribute instance reader.
    def __callbacks
      defined?(@__callbacks) ? @__callbacks : self.class.__callbacks
    end

    # Runs the callbacks for the given event.
    #
    # Calls the before and around callbacks in the order they were set, yields
    # the block (if given one), and then runs the after callbacks in reverse
    # order.
    #
    # If the callback chain was halted, returns +false+. Otherwise returns the
    # result of the block, +nil+ if no callbacks have been set, or +true+
    # if callbacks have been set but no block is given.
    def run_callbacks(kind, type = nil)
      callbacks = __callbacks[kind.to_sym]

      if callbacks.empty?
        yield if block_given?
      else
        env = Filters::Environment.new(self, false, nil)

        next_sequence = callbacks.compile(type)

        # Common case: no 'around' callbacks defined
        if next_sequence.final?
          next_sequence.invoke_before(env)
          env.value = !env.halted && (!block_given? || yield)
          next_sequence.invoke_after(env)
          env.value
        else
          invoke_sequence = Proc.new do
            skipped = nil

            while true
              current = next_sequence
              current.invoke_before(env)
              if current.final?
                env.value = !env.halted && (!block_given? || yield)
              elsif current.skip?(env)
                (skipped ||= []) << current
                next_sequence = next_sequence.nested
                next
              else
                next_sequence = next_sequence.nested
                begin
                  target, block, method, *arguments = current.expand_call_template(env, invoke_sequence)
                  target.send(method, *arguments, &block)
                ensure
                  next_sequence = current
                end
              end
              current.invoke_after(env)
              skipped.pop.invoke_after(env) while skipped&.first
              break env.value
            end
          end

          invoke_sequence.call
        end
      end
    end

    private
      # A hook invoked every time a before callback is halted.
      # This can be overridden in ActiveSupport::Callbacks implementors in order
      # to provide better debugging/logging.
      def halted_callback_hook(filter, name)
      end

      module Conditionals # :nodoc: all
        class Value
          def initialize(&block)
            @block = block
            freeze
          end
          def call(target, value); @block.call(value); end
        end
      end

      module Filters # :nodoc: all
        Environment = Struct.new(:target, :halted, :value)

        class Before
          def initialize(user_callback, user_conditions, chain_config, filter, name)
            halted_lambda = chain_config[:terminator]
            @user_callback, @user_conditions, @halted_lambda, @filter, @name = user_callback, user_conditions, halted_lambda, filter, name
            freeze
          end
          attr_reader :user_callback, :user_conditions, :halted_lambda, :filter, :name

          def call(env)
            target = env.target
            value  = env.value
            halted = env.halted

            if !halted && user_conditions.all? { |c| c.call(target, value) }
              result_lambda = -> { user_callback.call target, value }
              env.halted = halted_lambda.call(target, result_lambda)
              if env.halted
                target.send :halted_callback_hook, filter, name
              end
            end

            env
          end

          def apply(callback_sequence)
            callback_sequence.before(self)
          end
        end

        class After
          attr_reader :user_callback, :user_conditions, :halting
          def initialize(user_callback, user_conditions, chain_config)
            halting = chain_config[:skip_after_callbacks_if_terminated]
            @user_callback, @user_conditions, @halting = user_callback, user_conditions, halting
            freeze
          end

          def call(env)
            target = env.target
            value  = env.value
            halted = env.halted

            if (!halted || !@halting) && user_conditions.all? { |c| c.call(target, value) }
              user_callback.call target, value
            end

            env
          end

          def apply(callback_sequence)
            callback_sequence.after(self)
          end
        end

        class Around
          def initialize(user_callback, user_conditions)
            @user_callback, @user_conditions = user_callback, user_conditions
            freeze
          end

          def apply(callback_sequence)
            callback_sequence.around(@user_callback, @user_conditions)
          end
        end
      end

      class Callback # :nodoc:
        def self.build(chain, filter, kind, options)
          if filter.is_a?(String)
            raise ArgumentError, "Passing string to define a callback is not supported. See the `.set_callback` documentation to see supported values."
          end

          new chain.name, filter, kind, options, chain.config
        end

        attr_reader :kind, :name
        attr_reader :chain_config, :filter

        def initialize(name, filter, kind, options, chain_config)
          @chain_config = chain_config
          @name            = name
          @kind            = kind
          @original_filter = filter.object_id
          @filter          = filter
          @if              = check_conditionals(options[:if])
          @unless          = check_conditionals(options[:unless])

          compiled
        end

        def merge_conditional_options(chain, if_option:, unless_option:)
          options = {
            if: @if.dup,
            unless: @unless.dup
          }

          options[:if].concat     Array(unless_option)
          options[:unless].concat Array(if_option)

          self.class.build chain, @filter, @kind, options
        end

        def matches?(_kind, _filter)
          @kind == _kind && (filter == _filter || (@original_filter == _filter.object_id))
        end

        def duplicates?(other)
          case @filter
          when Symbol
            matches?(other.kind, other.filter)
          else
            false
          end
        end

        # Wraps code with filter
        def compiled
          @compiled ||=
            begin
              user_conditions = conditions_lambdas
              user_callback = CallTemplate.build(@filter, self)
              lambda = user_callback.make_lambda

              case kind
              when :before
                Filters::Before.new(lambda, user_conditions, chain_config, @filter, name)
              when :after
                Filters::After.new(lambda, user_conditions, chain_config)
              when :around
                Filters::Around.new(user_callback, user_conditions)
              end
            end
        end

        def freeze # :nodoc:
          return self if frozen?

          @compiled = nil

          compiled
          super
          self
        end

        def apply(callback_sequence)
          compiled.apply(callback_sequence)
        end

        def current_scopes
          Array(chain_config[:scope]).map { |s| public_send(s) }
        end

        private
          EMPTY_ARRAY = [].freeze
          private_constant :EMPTY_ARRAY

          def check_conditionals(conditionals)
            # `conditionals.blank?`
            return EMPTY_ARRAY if conditionals.respond_to?(:empty?) ? conditionals.empty? : !conditionals

            conditionals = Array(conditionals)
            if conditionals.any?(String)
              raise ArgumentError, "Passing string to be evaluated in :if and :unless conditional options is not supported. Pass a symbol for an instance method, or a lambda, proc or block, instead."
            end

            conditionals
          end

          def conditions_lambdas
            conditions =
              @if.map { |c| CallTemplate.build(c, self).make_lambda } +
              @unless.map { |c| CallTemplate.build(c, self).inverted_lambda }
            conditions.empty? ? EMPTY_ARRAY : conditions.freeze
          end
      end

      # A future invocation of user-supplied code (either as a callback or a condition).
      module CallTemplate # :nodoc: all
        class MethodCall
          def initialize(method)
            @method_name = method
          end

          # Return the parts needed to make this call, with the given
          # input values.
          #
          # Returns an array of the form:
          #
          #   [target, block, method, *arguments]
          #
          # This array can be used as such:
          #
          #   target.send(method, *arguments, &block)
          #
          # The actual invocation is left up to the caller to minimize
          # call stack pollution.
          def expand(target, value, block)
            [target, block, @method_name]
          end

          def make_lambda
            lambda do |target, value, &block|
              target.send(@method_name, &block)
            end
          end

          def inverted_lambda
            lambda do |target, value, &block|
              !target.send(@method_name, &block)
            end
          end
        end

        class ObjectCall
          def initialize(target, method)
            @override_target = target
            @method_name = method
          end

          def expand(target, value, block)
            [@override_target || target, block, @method_name, target]
          end

          def make_lambda
            lambda do |target, value, &block|
              (@override_target || target).send(@method_name, target, &block)
            end
          end

          def inverted_lambda
            lambda do |target, value, &block|
              !(@override_target || target).send(@method_name, target, &block)
            end
          end
        end

        class InstanceExec0
          def initialize(block)
            @override_block = block
          end

          def expand(target, value, block)
            [target, @override_block, :instance_exec]
          end

          def make_lambda
            lambda do |target, value, &block|
              target.instance_exec(&@override_block)
            end
          end

          def inverted_lambda
            lambda do |target, value, &block|
              !target.instance_exec(&@override_block)
            end
          end
        end

        class InstanceExec1
          def initialize(block)
            @override_block = block
          end

          def expand(target, value, block)
            [target, @override_block, :instance_exec, target]
          end

          def make_lambda
            lambda do |target, value, &block|
              target.instance_exec(target, &@override_block)
            end
          end

          def inverted_lambda
            lambda do |target, value, &block|
              !target.instance_exec(target, &@override_block)
            end
          end
        end

        class InstanceExec2
          def initialize(block)
            @override_block = block
          end

          def expand(target, value, block)
            raise ArgumentError unless block
            [target, @override_block || block, :instance_exec, target, block]
          end

          def make_lambda
            lambda do |target, value, &block|
              raise ArgumentError unless block
              target.instance_exec(target, block, &@override_block)
            end
          end

          def inverted_lambda
            lambda do |target, value, &block|
              raise ArgumentError unless block
              !target.instance_exec(target, block, &@override_block)
            end
          end
        end

        class ProcCall
          def initialize(target)
            @override_target = target
          end

          def expand(target, value, block)
            [@override_target || target, block, :call, target, value]
          end

          def make_lambda
            lambda do |target, value, &block|
              (@override_target || target).call(target, value, &block)
            end
          end

          def inverted_lambda
            lambda do |target, value, &block|
              !(@override_target || target).call(target, value, &block)
            end
          end
        end

        # Filters support:
        #
        #   Symbols:: A method to call.
        #   Procs::   A proc to call with the object.
        #   Objects:: An object with a <tt>before_foo</tt> method on it to call.
        #
        # All of these objects are converted into a CallTemplate and handled
        # the same after this point.
        def self.build(filter, callback)
          case filter
          when Symbol
            MethodCall.new(filter)
          when Conditionals::Value
            ProcCall.new(filter)
          when ::Proc
            case filter.arity
            when 2
              InstanceExec2.new(filter)
            when 1, -2
              InstanceExec1.new(filter)
            else
              InstanceExec0.new(filter)
            end
          else
            ObjectCall.new(filter, callback.current_scopes.join("_").to_sym)
          end
        end
      end

      # Execute before and after filters in a sequence instead of
      # chaining them with nested lambda calls, see:
      # https://github.com/rails/rails/issues/18011
      class CallbackSequence # :nodoc:
        def initialize(nested = nil, call_template = nil, user_conditions = nil)
          @nested = nested
          @call_template = call_template
          @user_conditions = user_conditions

          @before = nil
          @after = nil
        end

        def before(before)
          @before ||= []
          @before.unshift(before)
          self
        end

        def after(after)
          @after ||= []
          @after.push(after)
          self
        end

        def around(call_template, user_conditions)
          CallbackSequence.new(self, call_template, user_conditions)
        end

        def skip?(arg)
          arg.halted || !@user_conditions.all? { |c| c.call(arg.target, arg.value) }
        end

        attr_reader :nested

        def final?
          !@call_template
        end

        def expand_call_template(arg, block)
          @call_template.expand(arg.target, arg.value, block)
        end

        def invoke_before(arg)
          @before&.each { |b| b.call(arg) }
        end

        def invoke_after(arg)
          @after&.each { |a| a.call(arg) }
        end

        def freeze
          @before&.freeze
          @after&.freeze
          super
        end
      end

      class CallbackChain # :nodoc:
        include Enumerable

        attr_reader :name, :config

        def initialize(name, config)
          @name = name
          @config = {
            scope: [:kind],
            terminator: DEFAULT_TERMINATOR
          }.merge!(config)
          @chain = []
          @all_callbacks = nil
          @single_callbacks = {}
          @mutex = Mutex.new
        end

        def each(&block); @chain.each(&block); end
        def index(o);     @chain.index(o); end
        def empty?;       @chain.empty?; end

        def insert(index, o)
          @all_callbacks = nil
          @single_callbacks.clear
          @chain.insert(index, o)
        end

        def delete(o)
          @all_callbacks = nil
          @single_callbacks.clear
          @chain.delete(o)
        end

        def clear
          @all_callbacks = nil
          @single_callbacks.clear
          @chain.clear
          self
        end

        def initialize_copy(other)
          @all_callbacks = nil
          @single_callbacks = {}
          @chain     = other.chain.dup
          @mutex     = Mutex.new
        end

        def compile(type)
          if frozen?
            type.nil? ? (@all_callbacks || compile_sequence(nil)) : (@single_callbacks[type] || compile_sequence(type))
          elsif type.nil?
            @all_callbacks || @mutex.synchronize do
              @all_callbacks ||= compile_sequence(nil)
            end
          else
            @single_callbacks[type] || @mutex.synchronize do
              @single_callbacks[type] ||= compile_sequence(type)
            end
          end
        end

        def append(*callbacks)
          callbacks.each { |c| append_one(c) }
        end

        def prepend(*callbacks)
          callbacks.each { |c| prepend_one(c) }
        end

        def freeze
          return self if frozen?

          @chain.each(&:freeze)
          compile(nil)
          CALLBACK_FILTER_TYPES.each { |type| compile(type) }
          @chain.freeze
          @config.freeze
          @single_callbacks.freeze
          @mutex = nil
          super
        end

        protected
          attr_reader :chain

        private
          def compile_sequence(type)
            final_sequence = CallbackSequence.new
            @chain.reverse.inject(final_sequence) do |callback_sequence, callback|
              type.nil? || type == callback.kind ? callback.apply(callback_sequence) : callback_sequence
            end
          end

          def append_one(callback)
            @all_callbacks = nil
            @single_callbacks.clear
            remove_duplicates(callback)
            @chain.push(callback)
          end

          def prepend_one(callback)
            @all_callbacks = nil
            @single_callbacks.clear
            remove_duplicates(callback)
            @chain.unshift(callback)
          end

          def remove_duplicates(callback)
            @all_callbacks = nil
            @single_callbacks.clear
            @chain.delete_if { |c| callback.duplicates?(c) }
          end

          class DefaultTerminator # :nodoc:
            def call(target, result_lambda)
              terminate = true
              catch(:abort) do
                result_lambda.call
                terminate = false
              end
              terminate
            end
          end
          DEFAULT_TERMINATOR = DefaultTerminator.new.freeze
      end

      module ClassMethods
        # The class_attribute reader: the value the nearest class that
        # assigned one holds.
        def __callbacks
          owner = self
          owner = owner.superclass until owner.instance_variable_defined?(:@__callbacks)
          owner.instance_variable_get(:@__callbacks)
        end

        # The class_attribute writer.
        def __callbacks=(value)
          @__callbacks = value
        end

        def normalize_callback_params(filters, block) # :nodoc:
          type = CALLBACK_FILTER_TYPES.include?(filters.first) ? filters.shift : :before
          # `filters.extract_options!`
          options = filters.last.is_a?(Hash) && filters.last.instance_of?(Hash) ? filters.pop : {}
          filters.unshift(block) if block
          [type, filters, options.dup]
        end

        # This is used internally to append, prepend and skip callbacks to the
        # CallbackChain.
        def __update_callbacks(name) # :nodoc:
          self.descendants.prepend(self).reverse_each do |target|
            chain = target.get_callbacks name
            yield target, chain.dup
          end
        end

        # Install a callback for the given event.
        def set_callback(name, *filter_list, &block)
          type, filters, options = normalize_callback_params(filter_list, block)

          self_chain = get_callbacks name
          mapped = filters.map do |filter|
            Callback.build(self_chain, filter, type, options)
          end

          __update_callbacks(name) do |target, chain|
            options[:prepend] ? chain.prepend(*mapped) : chain.append(*mapped)
            target.set_callbacks name, chain
          end
        end

        # Skip a previously set callback. Like #set_callback, <tt>:if</tt> or
        # <tt>:unless</tt> options may be passed in order to control when the
        # callback is skipped.
        def skip_callback(name, *filter_list, &block)
          type, filters, options = normalize_callback_params(filter_list, block)

          options[:raise] = true unless options.key?(:raise)

          __update_callbacks(name) do |target, chain|
            filters.each do |filter|
              callback = chain.find { |c| c.matches?(type, filter) }

              if !callback && options[:raise]
                raise ArgumentError, "#{type.to_s.capitalize} #{name} callback #{filter.inspect} has not been defined"
              end

              if callback && (options.key?(:if) || options.key?(:unless))
                new_callback = callback.merge_conditional_options(chain, if_option: options[:if], unless_option: options[:unless])
                chain.insert(chain.index(callback), new_callback)
              end

              chain.delete(callback)
            end
            target.set_callbacks name, chain
          end
        end

        # Remove all set callbacks for the given event.
        def reset_callbacks(name)
          callbacks = get_callbacks name

          self.descendants.each do |target|
            chain = target.get_callbacks(name).dup
            callbacks.each { |c| chain.delete(c) }
            target.set_callbacks name, chain
          end

          set_callbacks(name, callbacks.dup.clear)
        end

        # Define sets of events in the object life cycle that support callbacks.
        def define_callbacks(*names)
          # `names.extract_options!`
          options = names.last.is_a?(Hash) && names.last.instance_of?(Hash) ? names.pop : {}

          names.each do |name|
            name = name.to_sym

            define_method(:"_run_#{name}_callbacks") do |&block|
              block.call if block
            end

            define_method(:"_run_#{name}_callbacks!") do |&block|
              run_callbacks name, &block
            end

            define_singleton_method(:"_#{name}_callbacks") do
              get_callbacks(name)
            end

            define_singleton_method(:"_#{name}_callbacks=") do |value|
              set_callbacks(name, value)
            end

            define_method(:"_#{name}_callbacks") do
              __callbacks[name]
            end

            ([self] + self.descendants).each do |target|
              target.set_callbacks name, CallbackChain.new(name, options)
            end
          end
        end

        def freeze # :nodoc:
          descendants.prepend(self).each do |target|
            target.__callbacks.each_value(&:freeze)
            target.__callbacks = target.__callbacks.dup.freeze
          end
        end

        protected
          def get_callbacks(name) # :nodoc:
            __callbacks[name.to_sym]
          end

          def set_callbacks(name, callbacks) # :nodoc:
            name = name.to_sym
            callback_sets = __callbacks.dup
            callbacks_was = callback_sets[name]
            if (callbacks_was.nil? || callbacks_was.empty?) && !callbacks.empty?
              alias_method("_run_#{name}_callbacks", "_run_#{name}_callbacks!")
            end
            callback_sets[name] = callbacks

            self.__callbacks = callback_sets
          end
      end
  end
end
