# frozen_string_literal: true

# ActiveSupport::IsolatedExecutionState, ported from the activesupport
# native Core runs (Shopify/rails 5813559f4aed, 8.2.0.alpha,
# lib/active_support/isolated_execution_state.rb) for the CRuby and JRuby
# trees. Execution-scoped state: per thread by default, per fiber at
# `isolation_level = :fiber`. The cache port's LocalCache registry
# (active_support_cache.rb) and the logger's thread-local level
# (active_support_logger.rb) keep their state here, as native does.

module ActiveSupport
  module IsolatedExecutionState
    @isolation_level = nil

    Thread.attr_accessor :active_support_execution_state
    Fiber.attr_accessor :active_support_execution_state

    class << self
      attr_reader :isolation_level, :scope

      def isolation_level=(level)
        return if level == @isolation_level

        unless %i(thread fiber).include?(level)
          raise ArgumentError, "isolation_level must be `:thread` or `:fiber`, got: `#{level.inspect}`"
        end

        clear if @isolation_level

        @scope =
          case level
          when :thread then Thread
          when :fiber then Fiber
          end

        @isolation_level = level
      end

      def [](key)
        if (state = @scope.current.active_support_execution_state)
          state[key]
        end
      end

      def []=(key, value)
        state = (@scope.current.active_support_execution_state ||= {})
        state[key] = value
      end

      def key?(key)
        @scope.current.active_support_execution_state&.key?(key)
      end

      def delete(key)
        @scope.current.active_support_execution_state&.delete(key)
      end

      def clear
        @scope.current.active_support_execution_state&.clear
      end

      def context
        scope.current
      end

      def share_with(other, except: [], &block)
        if (state = other.active_support_execution_state)
          copied_state = state.dup
          Array(except).each { |key| copied_state.delete(key) }
        end
        old_state, context.active_support_execution_state = context.active_support_execution_state, copied_state
        block.call
      ensure
        context.active_support_execution_state = old_state
      end
    end

    self.isolation_level = :thread
  end
end
