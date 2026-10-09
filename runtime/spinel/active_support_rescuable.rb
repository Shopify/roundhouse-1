# frozen_string_literal: true

# ActiveSupport::Rescuable, ported from the activesupport native Core runs
# (Shopify/rails 5813559f4aed, 8.2.0.alpha: lib/active_support/rescuable.rb)
# for the CRuby and JRuby trees, carried where app code names the module or
# a job runs on the ActiveJob::Callbacks port (active_job_callbacks.rb),
# whose ActiveJob::Execution includes it in Rails. Core's jobs declare
# `rescue_from` in their bodies and in their concerns' `included` blocks
# (BackgroundQueue::Retry, Billing::PdfGenerationResiliency). Each method
# follows the native one.
#
# Differences, at the edges:
# - ActiveSupport::Concern is not used: the `included` block
#   (`class_attribute :rescue_handlers, default: [].freeze`) is written out
#   as Concern#append_features, and the class_attribute as the reader and
#   writer it defines: a class-level value a subclass inherits until it
#   assigns its own, and an instance reader, writer and predicate that fall
#   back to the class's.
# - A handler named by a String that `const_get` cannot find is looked up
#   from the top level with `Object.const_get` (nil where that fails too),
#   which is what `String#safe_constantize` answers for an absolute name.

module ActiveSupport
  module Rescuable
    # `included do ... end`, run as Concern#append_features runs it: once,
    # and not again for a class that already has the module.
    def self.append_features(base)
      # A module includer is a concern natively (an app concern is lowered
      # without its extend): Concern#append_features defers to its includers.
      unless base.is_a?(Class)
        base.extend(ActiveSupport::Concern) if defined?(ActiveSupport::Concern) && !base.instance_variable_defined?(:@_dependencies)
        return super unless base.instance_variable_defined?(:@_dependencies)
        base.instance_variable_get(:@_dependencies) << self
        return false
      end
      return false if base < self
      super
      base.extend ClassMethods
      base.instance_variable_set(:@rescue_handlers, [].freeze)
    end

    # The class_attribute instance reader, writer and predicate.
    def rescue_handlers
      defined?(@rescue_handlers) ? @rescue_handlers : self.class.rescue_handlers
    end

    def rescue_handlers=(value)
      @rescue_handlers = value
    end

    def rescue_handlers?
      !!rescue_handlers
    end

    module ClassMethods
      # The class_attribute reader: the value the nearest class that
      # assigned one holds.
      def rescue_handlers
        owner = self
        owner = owner.superclass until owner.instance_variable_defined?(:@rescue_handlers)
        owner.instance_variable_get(:@rescue_handlers)
      end

      # The class_attribute writer.
      def rescue_handlers=(value)
        @rescue_handlers = value
      end

      def rescue_handlers?
        !!rescue_handlers
      end

      # Registers exception classes with a handler to be called by
      # <tt>rescue_with_handler</tt>.
      #
      # <tt>rescue_from</tt> receives a series of exception classes or class
      # names, and an exception handler specified by a trailing <tt>:with</tt>
      # option containing the name of a method or a Proc object. Alternatively,
      # a block can be given as the handler.
      #
      # Handlers are inherited. They are searched from right to left, from
      # bottom to top, and up the hierarchy.
      def rescue_from(*klasses, with: nil, &block)
        unless with
          if block_given?
            with = block
          else
            raise ArgumentError, "Need a handler. Pass the with: keyword argument or provide a block."
          end
        end

        klasses.each do |klass|
          key = if klass.is_a?(Module) && klass.respond_to?(:===)
            klass.name
          elsif klass.is_a?(String)
            klass
          else
            raise ArgumentError, "#{klass.inspect} must be an Exception class or a String referencing an Exception class"
          end

          # Put the new handler at the end because the list is read in reverse.
          self.rescue_handlers = [*rescue_handlers, [key, with].freeze].freeze
        end
      end

      # Matches an exception to a handler based on the exception class.
      #
      # If no handler matches the exception, check for a handler matching the
      # (optional) <tt>exception.cause</tt>. If no handler matches the exception
      # or its cause, this returns +nil+, so you can deal with unhandled
      # exceptions. Be sure to re-raise unhandled exceptions if this is what
      # you expect.
      #
      # Returns the exception if it was handled and +nil+ if it was not.
      def rescue_with_handler(exception, object: self, visited_exceptions: [])
        visited_exceptions << exception

        if handler = handler_for_rescue(exception, object: object)
          handler.call exception
          exception
        elsif exception
          if visited_exceptions.include?(exception.cause)
            nil
          else
            rescue_with_handler(exception.cause, object: object, visited_exceptions: visited_exceptions)
          end
        end
      end

      def handler_for_rescue(exception, object: self) # :nodoc:
        case rescuer = find_rescue_handler(exception)
        when Symbol
          method = object.method(rescuer)
          if method.arity == 0
            -> e { method.call }
          else
            method
          end
        when Proc
          if rescuer.arity == 0
            -> e { object.instance_exec(&rescuer) }
          else
            -> e { object.instance_exec(e, &rescuer) }
          end
        end
      end

      private
        def find_rescue_handler(exception)
          if exception
            # Handlers are in order of declaration but the most recently declared
            # is the highest priority match, so we search for matching handlers
            # in reverse.
            _, handler = rescue_handlers.reverse_each.detect do |class_or_name, _|
              if klass = constantize_rescue_handler_class(class_or_name)
                klass === exception
              end
            end

            handler
          end
        end

        def constantize_rescue_handler_class(class_or_name)
          case class_or_name
          when String, Symbol
            begin
              # Try a lexical lookup first since we support
              #
              #     class Super
              #       rescue_from 'Error', with: ...
              #     end
              #
              #     class Sub
              #       class Error < StandardError; end
              #     end
              #
              # so an Error raised in Sub will hit the 'Error' handler.
              const_get class_or_name
            rescue NameError
              # `class_or_name.safe_constantize`
              begin
                Object.const_get(class_or_name.to_s)
              rescue NameError
                nil
              end
            end
          else
            class_or_name
          end
        end
    end

    # Delegates to the class method, but uses the instance as the subject for
    # rescue_from handlers (method calls, +instance_exec+ blocks).
    def rescue_with_handler(exception)
      self.class.rescue_with_handler exception, object: self
    end

    # Internal handler lookup. Delegates to class method. Some libraries call
    # this directly, so keeping it around for compatibility.
    def handler_for_rescue(exception) # :nodoc:
      self.class.handler_for_rescue exception, object: self
    end
  end
end
