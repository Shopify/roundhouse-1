# frozen_string_literal: true

# ActiveSupport::Concern (Shopify/rails 5813559f4aed, 8.2.0.alpha
# activesupport/lib/active_support/concern.rb), verbatim; Rails' doc
# comments dropped. `require "active_support/concern"` from a native gem
# loads it (runtime/features, `project::FRAMEWORK_FEATURE_PORTS`), as
# Rails has before any gem natively. The app's own concerns are lowered
# at ingest and never extend it.
#
# The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).

module ActiveSupport
  module Concern
    class MultipleIncludedBlocks < StandardError # :nodoc:
      def initialize
        super "Cannot define multiple 'included' blocks for a Concern"
      end
    end

    class MultiplePrependBlocks < StandardError # :nodoc:
      def initialize
        super "Cannot define multiple 'prepended' blocks for a Concern"
      end
    end

    def self.extended(base) # :nodoc:
      base.instance_variable_set(:@_dependencies, [])
    end

    def append_features(base) # :nodoc:
      if base.instance_variable_defined?(:@_dependencies)
        base.instance_variable_get(:@_dependencies) << self
        false
      else
        return false if base < self
        @_dependencies.each { |dep| base.include(dep) }
        super
        base.extend const_get(:ClassMethods) if const_defined?(:ClassMethods)
        base.class_eval(&@_included_block) if instance_variable_defined?(:@_included_block)
      end
    end

    def prepend_features(base) # :nodoc:
      if base.instance_variable_defined?(:@_dependencies)
        base.instance_variable_get(:@_dependencies).unshift self
        false
      else
        return false if base < self
        @_dependencies.each { |dep| base.prepend(dep) }
        super
        base.singleton_class.prepend const_get(:ClassMethods) if const_defined?(:ClassMethods)
        base.class_eval(&@_prepended_block) if instance_variable_defined?(:@_prepended_block)
      end
    end

    def included(base = nil, &block)
      if base.nil?
        if instance_variable_defined?(:@_included_block)
          if @_included_block.source_location != block.source_location
            raise MultipleIncludedBlocks
          end
        else
          @_included_block = block
        end
      else
        super
      end
    end

    def prepended(base = nil, &block)
      if base.nil?
        if instance_variable_defined?(:@_prepended_block)
          if @_prepended_block.source_location != block.source_location
            raise MultiplePrependBlocks
          end
        else
          @_prepended_block = block
        end
      else
        super
      end
    end

    def class_methods(&class_methods_module_definition)
      mod = const_defined?(:ClassMethods, false) ?
        const_get(:ClassMethods) :
        const_set(:ClassMethods, Module.new)

      mod.module_eval(&class_methods_module_definition)
    end
  end
end
