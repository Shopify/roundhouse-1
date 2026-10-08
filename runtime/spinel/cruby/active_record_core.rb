# frozen_string_literal: true

# ActiveRecord::Base's constructor, `reload` and mass assignment as
# methods of modules Base includes, on the PostgreSQL CRuby tree, from the
# activemodel and activerecord native Core runs (Shopify/rails
# 5813559f4aed, 8.2.0.alpha): ActiveRecord::Core#initialize and its
# private `init_internals` (core.rb), Persistence#reload (persistence.rb),
# ActiveModel::AttributeAssignment with ForbiddenAttributesProtection
# (attribute_assignment.rb, forbidden_attributes_protection.rb),
# ActiveRecord::AttributeAssignment#_assign_attributes, and
# ActiveModel::UnknownAttributeError (errors.rb). A module a gem includes
# into Base later sits above these and supers into them, as in Rails:
# shopify-money 4.1.1's MoneyColumn::ActiveRecordHooks overrides
# `init_internals`, `_assign_attributes` and `reload`.
#
# Differences, at the edges:
# - `init_internals` is the runtime base's slot setup (the body of
#   runtime/ruby/active_record/base.rb's `initialize`, kept here as the
#   private `_runtime_internals`), not Rails' @readonly/@strict_loading
#   flags.
# - Core#initialize ignores its attributes: the model's compiled
#   `initialize` calls `super()` and then writes its columns, so
#   `init_internals` runs before any attribute write, as in Rails. Those
#   writes go column by column rather than through `_assign_attributes`,
#   and records loaded from the database are built through `new`, so an
#   override of `initialize` runs for them where Rails' `instantiate`
#   skips it. lower::model_to_library::macro_replay refuses a Base
#   include's override of either (`PARTIAL_BASE_HOOKS`).
# - `reload` takes no options (Rails' `lock:`); it re-reads the row
#   through the runtime's `_adapter_reload`.
# - Multiparameter keys (`"written_on(1i)"`) raise NotImplementedError.

module ActiveModel
  # Raised when forbidden attributes are used for mass assignment.
  class ForbiddenAttributesError < StandardError
  end

  # Raised when unknown attributes are supplied via mass assignment.
  class UnknownAttributeError < NoMethodError
    attr_reader :record, :attribute

    def initialize(record, attribute)
      @record = record
      @attribute = attribute
      super("unknown attribute '#{attribute}' for #{@record.class}.")
    end
  end

  module ForbiddenAttributesProtection # :nodoc:
    private
      def sanitize_for_mass_assignment(attributes)
        if attributes.respond_to?(:permitted?)
          raise ActiveModel::ForbiddenAttributesError if !attributes.permitted?
          attributes.to_h
        else
          attributes
        end
      end
      alias :sanitize_forbidden_attributes :sanitize_for_mass_assignment
  end

  module AttributeAssignment
    include ActiveModel::ForbiddenAttributesProtection

    # Allows you to set all the attributes by passing in a hash of
    # attributes with keys matching the attribute names.
    def assign_attributes(new_attributes)
      unless new_attributes.respond_to?(:each_pair)
        raise ArgumentError, "When assigning attributes, you must pass a hash as an argument, #{new_attributes.class} passed."
      end
      return if new_attributes.empty?

      _assign_attributes(sanitize_for_mass_assignment(new_attributes))
    end

    alias attributes= assign_attributes

    # Invoked when `assign_attributes` is passed an unknown attribute name.
    def attribute_writer_missing(name, value)
      raise UnknownAttributeError.new(self, name)
    end

    private
      def _assign_attributes(attributes)
        attributes.each_pair do |k, v|
          _assign_attribute(k, v)
        end
      end

      def _assign_attribute(k, v)
        setter = :"#{k}="
        public_send(setter, v)
      rescue NoMethodError
        if respond_to?(setter)
          raise
        else
          attribute_writer_missing(k.to_s, v)
        end
      end
  end
end

module ActiveRecord
  UnknownAttributeError = ActiveModel::UnknownAttributeError

  module Core
    def initialize(_attributes = nil)
      init_internals
      initialize_internals_callback
    end

    private
      def init_internals
        _runtime_internals
      end

      def initialize_internals_callback
      end
  end

  module Persistence
    # Reloads the record from the database.
    def reload
      _adapter_reload
      self
    end
  end

  module AttributeAssignment
    private
      def _assign_attributes(attributes)
        multi_parameter_attributes = nested_parameter_attributes = nil

        attributes.each do |k, v|
          key = k.to_s

          if key.include?("(")
            (multi_parameter_attributes ||= {})[key] = v
          elsif v.is_a?(Hash)
            (nested_parameter_attributes ||= {})[key] = v
          else
            _assign_attribute(key, v)
          end
        end

        assign_nested_parameter_attributes(nested_parameter_attributes) if nested_parameter_attributes
        assign_multiparameter_attributes(multi_parameter_attributes) if multi_parameter_attributes
      end

      # Assign any deferred nested attributes after the base attributes have been set.
      def assign_nested_parameter_attributes(pairs)
        pairs.each { |k, v| _assign_attribute(k, v) }
      end

      def assign_multiparameter_attributes(pairs)
        raise NotImplementedError, "#{self.class.name}: multiparameter attribute assignment " \
          "(#{pairs.keys.join(", ")}) is not ported"
      end
  end

  class Base
    alias_method :_runtime_internals, :initialize
    private :_runtime_internals
    # Ruby warns on removing `initialize` whatever $VERBOSE says, unless
    # it is nil; Core#initialize answers in its place.
    verbose, $VERBOSE = $VERBOSE, nil
    begin
      remove_method :initialize
    ensure
      $VERBOSE = verbose
    end
    remove_method :reload

    include ActiveModel::AttributeAssignment
    include Core
    include Persistence
    include AttributeAssignment
  end
end
