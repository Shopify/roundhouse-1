# frozen_string_literal: true

# ActiveModel::Dirty and its mutation trackers, ported from the activemodel
# native Core runs (Shopify/rails 5813559f4aed, 8.2.0.alpha:
# lib/active_model/dirty.rb and attribute_mutation_tracker.rb) for the
# PostgreSQL CRuby tree, carried where app code (a gem compiled as source
# among it) names ActiveModel::Dirty. Core's tableless classes include it
# two ways:
# - beside ActiveModel::Attributes (the checkout accounts customizations'
#   ActivatedExtension, ActivatedExtensionBehaviors and
#   ActivatedExtensionPlacement), where the attributes the class declares
#   are the ones tracked (`changed?`, `changes`, `name_changed?`,
#   `name_was`, `changes_applied`, `restore_attributes`, ...), a
#   subclass's own `changed?` chaining to it with `super`;
# - over plain readers and writers (Zendesk::Ticket's
#   `define_attribute_methods` and a writer that calls
#   `<attr>_will_change!`), where the forced changes are the ones tracked.
#
# Each method follows the native one, over the ports this tree carries:
# ActiveModel::AttributeMethods (runtime/active_model_attribute_methods.rb,
# which Dirty includes and registers its suffixes and affixes with) and,
# for a class with Attributes, ActiveModel::Attribute
# (runtime/active_model_attribute.rb).
#
# Differences, at the edges:
# - ActiveSupport::Concern is not used: the `included` block is written
#   out as Concern#append_features, with an includer that is a module (an
#   app concern) deferring to its own includers, as a Concern does.
# - ActiveModel::AttributeSet is not ported: the attributes of a class
#   with ActiveModel::Attributes are a Hash of name => ActiveModel::Attribute
#   (runtime/active_model_attributes.rb), which the tracker reads as the set
#   (an attribute the Hash lacks answering as the null attribute).
# - `as_json`'s exclusion of the two tracker ivars and `init_attributes`
#   (which only Active Record's `initialize_dup` calls) are not ported.

require "singleton"

module ActiveModel
  class AttributeMutationTracker # :nodoc:
    OPTION_NOT_GIVEN = Object.new.freeze

    def initialize(attributes)
      @attributes = attributes
    end

    def changed_attribute_names
      attr_names.select { |attr_name| changed?(attr_name) }
    end

    def changed_values
      attr_names.each_with_object(ActiveSupport::HashWithIndifferentAccess.new) do |attr_name, result|
        if changed?(attr_name)
          result.store(attr_name, original_value(attr_name), convert_value: false)
        end
      end
    end

    def changes
      attr_names.each_with_object(ActiveSupport::HashWithIndifferentAccess.new) do |attr_name, result|
        if change = change_to_attribute(attr_name)
          result.store(attr_name, change, convert_value: false)
        end
      end
    end

    def change_to_attribute(attr_name)
      if changed?(attr_name)
        [original_value(attr_name), fetch_value(attr_name)]
      end
    end

    def any_changes?
      attr_names.any? { |attr| changed?(attr) }
    end

    def changed?(attr_name, from: OPTION_NOT_GIVEN, to: OPTION_NOT_GIVEN)
      attribute_changed?(attr_name) &&
        (OPTION_NOT_GIVEN == from || original_value(attr_name) == type_cast(attr_name, from)) &&
        (OPTION_NOT_GIVEN == to || fetch_value(attr_name) == type_cast(attr_name, to))
    end

    def changed_in_place?(attr_name)
      attribute(attr_name).changed_in_place?
    end

    def forget_change(attr_name)
      attributes[attr_name] = attribute(attr_name).forgetting_assignment
      forced_changes.delete(attr_name)
    end

    def original_value(attr_name)
      attribute(attr_name).original_value
    end

    def force_change(attr_name)
      forced_changes[attr_name] = fetch_value(attr_name)
    end

    private
      attr_reader :attributes

      # AttributeSet#[]: an attribute the set lacks is the null attribute.
      def attribute(attr_name)
        attributes[attr_name] || ActiveModel::Attribute.null(attr_name)
      end

      def forced_changes
        @forced_changes ||= {}
      end

      # AttributeSet#keys: the initialized attributes.
      def attr_names
        attributes.select { |_, attribute| attribute.initialized? }.keys
      end

      def attribute_changed?(attr_name)
        forced_changes.include?(attr_name) || !!attribute(attr_name).changed?
      end

      def fetch_value(attr_name)
        attribute(attr_name).value
      end

      def type_cast(attr_name, value)
        attribute(attr_name).type_cast(value)
      end
  end

  class ForcedMutationTracker < AttributeMutationTracker # :nodoc:
    def initialize(attributes)
      super
      @finalized_changes = nil
    end

    def changed_in_place?(attr_name)
      false
    end

    def change_to_attribute(attr_name)
      if finalized_changes&.include?(attr_name)
        finalized_changes[attr_name].dup
      else
        super
      end
    end

    def forget_change(attr_name)
      forced_changes.delete(attr_name)
    end

    def original_value(attr_name)
      if changed?(attr_name)
        forced_changes[attr_name]
      else
        fetch_value(attr_name)
      end
    end

    def force_change(attr_name)
      forced_changes[attr_name] = clone_value(attr_name) unless attribute_changed?(attr_name)
    end

    def finalize_changes
      @finalized_changes = changes
    end

    private
      attr_reader :finalized_changes

      def attr_names
        forced_changes.keys
      end

      def attribute_changed?(attr_name)
        forced_changes.include?(attr_name)
      end

      # Here the tracked object itself answers the value.
      def fetch_value(attr_name)
        attributes.send(:_read_attribute, attr_name)
      end

      def clone_value(attr_name)
        value = fetch_value(attr_name)
        value.duplicable? ? value.clone : value
      rescue TypeError, NoMethodError
        value
      end

      def type_cast(attr_name, value)
        value
      end
  end

  class NullMutationTracker # :nodoc:
    include Singleton

    def changed_attribute_names
      []
    end

    def changed_values
      {}
    end

    def changes
      {}
    end

    def change_to_attribute(attr_name)
    end

    def any_changes?
      false
    end

    def changed?(attr_name, **)
      false
    end

    def changed_in_place?(attr_name)
      false
    end

    def original_value(attr_name)
    end
  end

  module Dirty
    # `included do ... end` and `include ActiveModel::AttributeMethods`, as
    # Concern#append_features runs them: once, and not again for a class
    # that already has the module.
    def self.append_features(base)
      unless base.is_a?(Class)
        base.extend(ActiveSupport::Concern) if defined?(ActiveSupport::Concern) && !base.instance_variable_defined?(:@_dependencies)
        return super unless base.instance_variable_defined?(:@_dependencies)
        base.instance_variable_get(:@_dependencies) << self
        return false
      end
      return false if base < self
      base.include(ActiveModel::AttributeMethods)
      super
      base.class_eval do
        attribute_method_suffix "_previously_changed?", "_changed?", parameters: "**options"
        attribute_method_suffix "_change", "_will_change!", "_was", parameters: false
        attribute_method_suffix "_previous_change", "_previously_was", parameters: false
        attribute_method_affix prefix: "restore_", suffix: "!", parameters: false
        attribute_method_affix prefix: "clear_", suffix: "_change", parameters: false
      end
    end

    def initialize_dup(other) # :nodoc:
      super
      @mutations_from_database = nil
    end

    # Clears dirty data and moves +changes+ to +previous_changes+ and
    # +mutations_from_database+ to +mutations_before_last_save+
    # respectively.
    def changes_applied
      unless defined?(@attributes)
        mutations_from_database.finalize_changes
      end
      @mutations_before_last_save = mutations_from_database
      forget_attribute_assignments
      @mutations_from_database = nil
    end

    # Returns +true+ if any of the attributes has unsaved changes, +false+
    # otherwise.
    def changed?
      mutations_from_database.any_changes?
    end

    # Returns an array with the name of the attributes with unsaved
    # changes.
    def changed
      mutations_from_database.changed_attribute_names
    end

    # Dispatch target for <tt>*_changed?</tt> attribute methods.
    def attribute_changed?(attr_name, **options)
      mutations_from_database.changed?(attr_name.to_s, **options)
    end

    # Dispatch target for <tt>*_was</tt> attribute methods.
    def attribute_was(attr_name)
      mutations_from_database.original_value(attr_name.to_s)
    end

    # Dispatch target for <tt>*_previously_changed?</tt> attribute methods.
    def attribute_previously_changed?(attr_name, **options)
      mutations_before_last_save.changed?(attr_name.to_s, **options)
    end

    # Dispatch target for <tt>*_previously_was</tt> attribute methods.
    def attribute_previously_was(attr_name)
      mutations_before_last_save.original_value(attr_name.to_s)
    end

    # Restores all previous data of the provided attributes.
    def restore_attributes(attr_names = changed)
      attr_names.each { |attr_name| restore_attribute!(attr_name) }
    end

    # Clears all dirty data: current changes and previous changes.
    def clear_changes_information
      @mutations_before_last_save = nil
      forget_attribute_assignments
      @mutations_from_database = nil
    end

    def clear_attribute_changes(attr_names)
      attr_names.each do |attr_name|
        clear_attribute_change(attr_name)
      end
    end

    # Returns a hash of the attributes with unsaved changes indicating
    # their original values.
    def changed_attributes
      mutations_from_database.changed_values
    end

    # Returns a hash of changed attributes indicating their original and
    # new values: <tt>{ "name" => ["Bob", "Bill"] }</tt>.
    def changes
      mutations_from_database.changes
    end

    # Returns a hash of attributes that were changed before the model was
    # saved.
    def previous_changes
      mutations_before_last_save.changes
    end

    def attribute_changed_in_place?(attr_name) # :nodoc:
      mutations_from_database.changed_in_place?(attr_name.to_s)
    end

    private
      def clear_attribute_change(attr_name)
        mutations_from_database.forget_change(attr_name.to_s)
      end

      def mutations_from_database
        @mutations_from_database ||= if defined?(@attributes)
          ActiveModel::AttributeMutationTracker.new(@attributes)
        else
          ActiveModel::ForcedMutationTracker.new(self)
        end
      end

      def forget_attribute_assignments
        @attributes = @attributes.transform_values(&:forgetting_assignment) if defined?(@attributes)
      end

      def mutations_before_last_save
        @mutations_before_last_save ||= ActiveModel::NullMutationTracker.instance
      end

      # Dispatch target for <tt>*_change</tt> attribute methods.
      def attribute_change(attr_name)
        mutations_from_database.change_to_attribute(attr_name.to_s)
      end

      # Dispatch target for <tt>*_previous_change</tt> attribute methods.
      def attribute_previous_change(attr_name)
        mutations_before_last_save.change_to_attribute(attr_name.to_s)
      end

      # Dispatch target for <tt>*_will_change!</tt> attribute methods.
      def attribute_will_change!(attr_name)
        mutations_from_database.force_change(attr_name.to_s)
      end

      # Dispatch target for <tt>restore_*!</tt> attribute methods.
      def restore_attribute!(attr_name)
        attr_name = attr_name.to_s
        if attribute_changed?(attr_name)
          __send__("#{attr_name}=", attribute_was(attr_name))
          clear_attribute_change(attr_name)
        end
      end
  end
end
