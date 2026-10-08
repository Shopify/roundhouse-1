# frozen_string_literal: true

# ActiveSupport::OptionMerger and Object#with_options, ported from the
# activesupport native Core runs (Shopify/rails 5813559f4aed,
# 8.2.0.alpha: lib/active_support/option_merger.rb and
# core_ext/object/with_options.rb) for the PostgreSQL CRuby tree. It
# rides with the ActiveModel::Validations port
# (runtime/active_model_validations.rb): a replayed class body's
# `with_options unless: :reconnaissance do validates ... end`
# (api_versioning's ApiChangeRecord) runs through it.
#
# Each method follows the native one. Differences, at the edges:
# - `@options.deep_merge(...)` (core_ext/hash/deep_merge.rb) is written
#   out as OptionMerger.deep_merge, so the port does not depend on the
#   tree's core_ext overlay: a key both hashes hold merges recursively
#   when both values are Hashes, and takes the other value otherwise.

module ActiveSupport
  class OptionMerger # :nodoc:
    instance_methods.each do |method|
      undef_method(method) unless method.start_with?("__", "instance_eval", "class", "object_id")
    end

    def initialize(context, options)
      @context, @options = context, options
    end

    # Hash#deep_merge.
    def self.deep_merge(this, other)
      this.merge(other) do |_key, this_val, other_val|
        if this_val.is_a?(::Hash) && other_val.is_a?(::Hash)
          deep_merge(this_val, other_val)
        else
          other_val
        end
      end
    end

    private
      def method_missing(method, *arguments, &block)
        options = nil
        if arguments.size == 1 && arguments.first.is_a?(Proc)
          proc = arguments.shift
          arguments << lambda { |*args| ::ActiveSupport::OptionMerger.deep_merge(@options, proc.call(*args)) }
        elsif arguments.last.respond_to?(:to_hash)
          options = ::ActiveSupport::OptionMerger.deep_merge(@options, arguments.pop)
        else
          options = @options
        end

        if options
          @context.__send__(method, *arguments, **options, &block)
        else
          @context.__send__(method, *arguments, &block)
        end
      end

      def respond_to_missing?(...)
        @context.respond_to?(...)
      end
  end
end

class Object
  # An elegant way to factor duplication out of options passed to a series of
  # method calls. Each method called in the block, with the block variable as
  # the receiver, will have its options merged with the default +options+
  # Hash or Hash-like object provided.
  def with_options(options, &block)
    option_merger = ActiveSupport::OptionMerger.new(self, options)

    if block
      block.arity.zero? ? option_merger.instance_eval(&block) : block.call(option_merger)
    else
      option_merger
    end
  end
end
