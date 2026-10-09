# frozen_string_literal: true

# ActiveRecord::Validations, ported from the activerecord native Core runs
# (Shopify/rails 5813559f4aed, 8.2.0.alpha: lib/active_record/validations.rb
# and validations/{associated,presence,absence,length,numericality}.rb)
# for the PostgreSQL CRuby tree, over the ActiveModel::Validations port
# (runtime/active_model_validations.rb), which is loaded before it.
# Carried where app code names ActiveRecord::Validations: Core's
# Validatable concern does `base.extend(ActiveRecord::Validations::ClassMethods)`
# for a Struct that includes ActiveModel::Validations
# (Script::EmbeddedEvent), so its `validates_*_of` helpers build Active
# Record's validators.
#
# Each method follows the native one. Differences, at the edges:
# - ActiveSupport::Concern is not used: including the module extends the
#   includer with ClassMethods, as Concern#append_features does.
#   ActiveRecord::Base does not include it on this tree: a compiled
#   model's save, `valid?` and validation context are runtime/active_record/
#   connection.rb's, and its declared validations are compiled.
# - `Array.wrap` (core_ext/array/wrap.rb) is written out where the
#   validators call it.
# - The validators that read association reflection (presence, absence)
#   raise NotImplementedError naming the class for an ActiveRecord::Base
#   class, which does not answer `_reflect_on_association` here; any
#   other class raises the NoMethodError Rails raises.
# - validates_uniqueness_of and UniquenessValidator (validations/
#   uniqueness.rb, a query per validation) are not ported: declaring one
#   raises NotImplementedError naming the class and attributes.
module ActiveRecord
  module Validations
    def self.included(base) # :nodoc:
      super
      base.extend(ClassMethods)
    end

    # Array.wrap (core_ext/array/wrap.rb).
    def self.wrap(object) # :nodoc:
      if object.nil?
        []
      elsif object.respond_to?(:to_ary)
        object.to_ary || [object]
      else
        [object]
      end
    end

    # `record.class._reflect_on_association(attribute)`, which an Active
    # Record class answers from its association reflections.
    def self.reflect_on_association(record, attribute) # :nodoc:
      klass = record.class
      if !klass.respond_to?(:_reflect_on_association, true) && defined?(ActiveRecord::Base) && klass < ActiveRecord::Base
        raise NotImplementedError, "#{klass.name}: Active Record's presence and absence validators read " \
          "`_reflect_on_association(#{attribute.inspect})`, which is not ported"
      end
      klass._reflect_on_association(attribute)
    end

    # The validation process on save can be skipped by passing <tt>validate: false</tt>.
    # The validation context can be changed by passing <tt>context: context</tt>.
    def save(**options)
      perform_validations(options) ? super : false
    end

    # Attempts to save the record just like save but will raise an
    # ActiveRecord::RecordInvalid exception instead of returning +false+ if
    # the record is not valid.
    def save!(**options)
      perform_validations(options) ? super : raise_validation_error
    end

    # Runs all the validations within the specified context. Returns +true+ if
    # no errors are found, +false+ otherwise.
    def valid?(context = nil)
      context ||= default_validation_context
      output = super(context)
      errors.empty? && output
    end

    alias_method :validate, :valid?

    def custom_validation_context? # :nodoc:
      validation_context && ![:create, :update].include?(validation_context)
    end

    private
      def default_validation_context
        new_record? ? :create : :update
      end

      def raise_validation_error
        raise(RecordInvalid.new(self))
      end

      def perform_validations(options = {})
        options[:validate] == false || valid?(options[:context])
      end

    public

    # ---- validations/associated.rb
    class AssociatedValidator < ActiveModel::EachValidator # :nodoc:
      def validate_each(record, attribute, value)
        context = record_validation_context_for_association(record)

        if Array(value).reject { |association| valid_object?(association, context) }.any?
          record.errors.add(attribute, :invalid, **options, value: value)
        end
      end

      private
        def valid_object?(record, context)
          (record.respond_to?(:marked_for_destruction?) && record.marked_for_destruction?) || record.valid?(context)
        end

        def record_validation_context_for_association(record)
          record.custom_validation_context? ? record.validation_context : nil
        end
    end

    module ClassMethods
      # Validates whether the associated object or objects are all valid.
      def validates_associated(*attr_names)
        validates_with AssociatedValidator, _merge_attributes(attr_names)
      end
    end

    # ---- validations/presence.rb
    class PresenceValidator < ActiveModel::Validations::PresenceValidator # :nodoc:
      def validate_each(record, attribute, association_or_value)
        if Validations.reflect_on_association(record, attribute)
          association_or_value = Validations.wrap(association_or_value).reject(&:marked_for_destruction?)
        end
        super
      end
    end

    module ClassMethods
      # Validates that the specified attributes are not blank (as defined by
      # Object#blank?).
      def validates_presence_of(*attr_names)
        validates_with PresenceValidator, _merge_attributes(attr_names)
      end
    end

    # ---- validations/absence.rb
    class AbsenceValidator < ActiveModel::Validations::AbsenceValidator # :nodoc:
      def validate_each(record, attribute, association_or_value)
        if Validations.reflect_on_association(record, attribute)
          association_or_value = Validations.wrap(association_or_value).reject(&:marked_for_destruction?)
        end
        super
      end
    end

    module ClassMethods
      # Validates that the specified attributes are not present (as defined by
      # Object#present?).
      def validates_absence_of(*attr_names)
        validates_with AbsenceValidator, _merge_attributes(attr_names)
      end
    end

    # ---- validations/length.rb
    class LengthValidator < ActiveModel::Validations::LengthValidator # :nodoc:
      def validate_each(record, attribute, association_or_value)
        if association_or_value.respond_to?(:loaded?) && association_or_value.loaded?
          association_or_value = association_or_value.target.reject(&:marked_for_destruction?)
        end
        super
      end
    end

    module ClassMethods
      # Validates that the specified attributes match the length restrictions
      # supplied.
      def validates_length_of(*attr_names)
        validates_with LengthValidator, _merge_attributes(attr_names)
      end

      alias_method :validates_size_of, :validates_length_of
    end

    # ---- validations/numericality.rb
    class NumericalityValidator < ActiveModel::Validations::NumericalityValidator # :nodoc:
      def validate_each(record, attribute, value, precision: nil, scale: nil)
        precision = [column_precision_for(record, attribute) || Float::DIG, Float::DIG].min
        scale     = column_scale_for(record, attribute)
        super(record, attribute, value, precision: precision, scale: scale)
      end

      private
        def column_precision_for(record, attribute)
          record.class.type_for_attribute(attribute.to_s)&.precision
        end

        def column_scale_for(record, attribute)
          record.class.type_for_attribute(attribute.to_s)&.scale
        end
    end

    module ClassMethods
      # Validates whether the value of the specified attribute is numeric.
      def validates_numericality_of(*attr_names)
        validates_with NumericalityValidator, _merge_attributes(attr_names)
      end
    end

    # ---- validations/uniqueness.rb (not ported; see the header)
    class UniquenessValidator < ActiveModel::EachValidator # :nodoc:
      def initialize(options)
        raise NotImplementedError, "#{options[:class]&.name}: ActiveRecord::Validations::UniquenessValidator " \
          "(#{Array(options[:attributes]).map(&:inspect).join(", ")}) is not ported"
      end
    end

    module ClassMethods
      # Validates whether the value of the specified attributes are unique.
      def validates_uniqueness_of(*attr_names)
        validates_with UniquenessValidator, _merge_attributes(attr_names)
      end
    end
  end
end
