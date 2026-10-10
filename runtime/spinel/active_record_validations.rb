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
# - UniquenessValidator (validations/uniqueness.rb) queries through the
#   runtime's Relation (runtime/active_record/relation.rb) the way the
#   compiled `uniqueness:` rule does: `Relation.new(finder, false)` is
#   `finder_class.unscoped` (an STI subclass keeps its type condition),
#   and PostgreSQL's comparisons are written out: `case_sensitive: false`
#   on a string or text column is `LOWER(col) = LOWER(value)` (the
#   adapter's case_insensitive_comparison; citext, uuid and other columns
#   compare with `=`), anything else `=`, a nil value `IS NULL`. A
#   `belongs_to` attribute binds its foreign key (and foreign type) to
#   the associated record's key; a `belongs_to` scope binds the record's
#   own foreign key (and type) columns, where Rails reads the association
#   (the same key unless an unsaved target was assigned). `validation_needed?`'s
#   unique-index shortcut, which only spares a query whose answer the
#   index already guarantees, is not taken: a persisted record always
#   queries. Enum mapping applies where the class answers `defined_enums`.
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

    # ---- validations/uniqueness.rb
    class UniquenessValidator < ActiveModel::EachValidator # :nodoc:
      # The column types PostgreSQL's `lower()` takes, directly or through
      # a cast (can_perform_case_insensitive_comparison_for?); citext is
      # case-insensitive itself.
      LOWER_TYPES = [:string, :text].freeze

      def initialize(options)
        if options[:conditions] && !options[:conditions].respond_to?(:call)
          raise ArgumentError, "#{options[:conditions]} was passed as :conditions but is not callable. " \
                               "Pass a callable instead: `conditions: -> { where(approved: true) }`"
        end
        unless Validations.wrap(options[:scope]).all? { |scope| scope.respond_to?(:to_sym) }
          raise ArgumentError, "#{options[:scope]} is not supported format for :scope option. " \
            "Pass a symbol or an array of symbols instead: `scope: :user_id`"
        end
        super
        @klass = options[:class]
      end

      def validate_each(record, attribute, value)
        finder_class = find_finder_class_for(record)
        value = map_enum_attribute(finder_class, attribute, value)

        relation = build_relation(finder_class, attribute, value)
        if record.persisted?
          primary_key = finder_class.primary_key
          if primary_key
            id = record.respond_to?(:id_in_database) ? record.id_in_database : record.id
            relation = relation.not(primary_key.to_sym => [id])
          else
            raise UnknownPrimaryKey.new(finder_class, "Cannot validate uniqueness for persisted record without primary key.")
          end
        end
        relation = scope_relation(record, relation)

        if options[:conditions]
          conditions = options[:conditions]

          relation = if conditions.arity.zero?
            relation.instance_exec(&conditions)
          else
            relation.instance_exec(record, &conditions)
          end
        end

        if relation.exists?
          error_options = options.except(:case_sensitive, :scope, :conditions)
          error_options[:value] = value

          record.errors.add(attribute, :taken, **error_options)
        end
      end

      private
        # The class hierarchy is searched upwards to the class declaring
        # the validation, for the lowest class that is not abstract.
        def find_finder_class_for(record)
          current_class = record.class
          found_class = nil
          loop do
            found_class = current_class unless current_class.abstract_class?
            break if current_class.equal?(@klass) || current_class.superclass.nil? ||
              (defined?(ActiveRecord::Base) && current_class.equal?(ActiveRecord::Base))
            current_class = current_class.superclass
          end
          found_class
        end

        def build_relation(klass, attribute, value)
          relation = ActiveRecord::Relation.new(klass, false)
          reflection = belongs_to_reflection(klass, attribute)
          return bind_association(relation, reflection, value) if reflection

          column = attribute.to_s
          if value.nil? || !options.key?(:case_sensitive) || options[:case_sensitive] || !lower_comparable?(klass, column)
            relation.where(column.to_sym => value)
          else
            relation.where("LOWER(#{klass._table_sql}.#{quote_column(column)}) = LOWER(?)", value)
          end
        end

        def scope_relation(record, relation)
          Validations.wrap(options[:scope]).each do |scope_item|
            reflection = belongs_to_reflection(record.class, scope_item)
            if reflection
              relation = relation.where(reflection.foreign_key.to_sym => read_column(record, reflection.foreign_key))
              if reflection.polymorphic?
                relation = relation.where(reflection.foreign_type.to_sym => read_column(record, reflection.foreign_type))
              end
            else
              unless column?(record.class, scope_item.to_s) || record.class.respond_to?(:reflect_on_association)
                raise NotImplementedError, "#{record.class.name}: UniquenessValidator scope #{scope_item.inspect} names no column, " \
                  "and the class carries no association reflections to resolve it"
              end
              relation = relation.where(scope_item.to_sym => read_column(record, scope_item))
            end
          end
          relation
        end

        # PredicateBuilder's AssociationQueryValue for `where(assoc => record)`.
        def bind_association(relation, reflection, target)
          key = target.nil? ? nil : target.public_send(reflection.association_primary_key)
          relation = relation.where(reflection.foreign_key.to_sym => key)
          if reflection.polymorphic?
            type = target.nil? ? nil : target.class.polymorphic_name
            relation = relation.where(reflection.foreign_type.to_sym => type)
          end
          relation
        end

        # `record.read_attribute(name)`: a compiled model holds a column in
        # its instance variable, past any reader the class overrides.
        def read_column(record, name)
          return record.read_attribute(name) if record.respond_to?(:read_attribute)
          ivar = :"@#{name}"
          record.instance_variable_defined?(ivar) ? record.instance_variable_get(ivar) : record.send(name)
        end

        def belongs_to_reflection(klass, name)
          return nil unless klass.respond_to?(:reflect_on_association)
          reflection = klass.reflect_on_association(name)
          reflection if reflection.respond_to?(:belongs_to?) && reflection.belongs_to?
        end

        def column?(klass, column)
          !klass.respond_to?(:schema_column_types) || klass.schema_column_types.key?(column)
        end

        def lower_comparable?(klass, column)
          spec = klass.respond_to?(:schema_column_types) ? klass.schema_column_types[column] : nil
          spec.nil? || LOWER_TYPES.include?(Validations.wrap(spec).first)
        end

        def quote_column(column)
          "\"#{column.gsub('"', '""')}\""
        end

        def map_enum_attribute(klass, attribute, value)
          return value unless klass.respond_to?(:defined_enums)
          mapping = klass.defined_enums[attribute.to_s]
          value = mapping[value] if value && mapping
          value
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
