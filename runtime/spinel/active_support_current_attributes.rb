# frozen_string_literal: true

# ActiveSupport::CurrentAttributes (Shopify/rails 5813559f4aed, 8.2.0.alpha
# activesupport/lib/active_support/current_attributes.rb), verbatim; Rails'
# doc comments dropped. `require "active_support/current_attributes"` from a
# native gem loads it (runtime/features, `project::FRAMEWORK_FEATURE_PORTS`),
# as does a read of ActiveSupport::CurrentAttributes (the lazy-load-hooks
# port's autoload table, as active_support.rb autoloads it).
#
# An app's own `class Current < ActiveSupport::CurrentAttributes` is
# compiled to a plain class (`ingest::current_attributes`); this base is for
# a gem's.
#
# Differences, at the edges:
# - Rails' requires name its own files; here they are the tree's ports of
#   them, required relative to this file.
# - The tree's ExecutionContext (runtime/active_support_reporters.rb, shared
#   with the targets that have no CurrentAttributes) keeps no
#   `current_attributes_instances`. This file gives its Record the per-record
#   table Rails' does, saved and restored with `push` and `pop` and cleared
#   by `flush`, as Rails' Record does.
#
# The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).

require_relative "active_support_callbacks"
require_relative "active_support_reporters"
require_relative "active_support_class_attribute"
require_relative "active_support_code_generator"
require_relative "active_support_enumerable"
require_relative "active_support_delegation"

module ActiveSupport
  module ExecutionContext
    module CurrentAttributesRecord # :nodoc:
      def current_attributes_instances
        @current_attributes_instances ||= {}
      end

      def push
        (@current_attributes_stack ||= []) << current_attributes_instances
        @current_attributes_instances = {}
        super
      end

      def pop
        @current_attributes_instances = (@current_attributes_stack ||= []).pop || {}
        super
      end

      def flush
        @current_attributes_stack = Array.new((@current_attributes_stack || []).size) { {} }
        @current_attributes_instances = {}
        super
      end
    end
    Record.prepend(CurrentAttributesRecord)

    def self.current_attributes_instances
      record.current_attributes_instances
    end
  end
end

module ActiveSupport
  class CurrentAttributes
    include ActiveSupport::Callbacks
    define_callbacks :reset

    NOT_SET = Object.new.freeze # :nodoc:

    class << self
      def instance
        current_instances[current_instances_key] ||= new
      end

      def attribute(*names, default: NOT_SET)
        invalid_attribute_names = names.map(&:to_sym) & INVALID_ATTRIBUTE_NAMES
        if invalid_attribute_names.any?
          raise ArgumentError, "Restricted attribute names: #{invalid_attribute_names.join(", ")}"
        end

        Delegation.generate(singleton_class, names, to: :instance, nilable: false, signature: "")
        Delegation.generate(singleton_class, names.map { |n| "#{n}=" }, to: :instance, nilable: false, signature: "value")

        ActiveSupport::CodeGenerator.batch(generated_attribute_methods, __FILE__, __LINE__) do |owner|
          names.each do |name|
            owner.define_cached_method(name, namespace: :current_attributes) do |batch|
              batch <<
                "def #{name}" <<
                "@attributes[:#{name}]" <<
                "end"
            end
            owner.define_cached_method("#{name}=", namespace: :current_attributes) do |batch|
              batch <<
                "def #{name}=(value)" <<
                "@attributes[:#{name}] = value" <<
                "end"
            end
          end
        end

        self.defaults = defaults.merge(names.index_with { default })
      end

      def before_reset(*methods, &block)
        set_callback :reset, :before, *methods, &block
      end

      def resets(*methods, &block)
        set_callback :reset, :after, *methods, &block
      end
      alias_method :after_reset, :resets

      delegate :set, :reset, to: :instance

      def clear_all # :nodoc:
        if instances = current_instances
          instances.values.each(&:reset)
          instances.clear
        end
      end

      private
        def generated_attribute_methods
          @generated_attribute_methods ||= Module.new.tap { |mod| include mod }
        end

        def current_instances
          ExecutionContext.current_attributes_instances
        end

        def current_instances_key
          @current_instances_key ||= name.to_sym
        end

        def method_missing(name, ...)
          instance.public_send(name, ...)
        end

        def respond_to_missing?(name, _)
          instance.respond_to?(name) || super
        end

        def method_added(name)
          super

          return if name == :initialize

          return unless public_method_defined?(name)

          return if singleton_class.method_defined?(name) || singleton_class.private_method_defined?(name)

          Delegation.generate(singleton_class, [name], to: :instance, as: self, nilable: false)
        end
    end

    class_attribute :defaults, instance_writer: false, default: {}.freeze

    attr_writer :attributes

    def initialize
      @attributes = resolve_defaults
    end

    def attributes
      @attributes.dup
    end

    def set(attributes, &block)
      with(**attributes, &block)
    end

    def reset
      run_callbacks :reset do
        self.attributes = resolve_defaults
      end
    end

    private
      def resolve_defaults
        defaults.each_with_object({}) do |(key, value), result|
          if value != NOT_SET
            result[key] = Proc === value ? value.call : value.dup
          end
        end
      end

      INVALID_ATTRIBUTE_NAMES = (methods(false) + private_methods(false) + instance_methods(false) + private_instance_methods(false)).uniq.sort.freeze # :nodoc:
  end
end
