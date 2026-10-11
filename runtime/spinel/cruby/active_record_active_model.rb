# frozen_string_literal: true

# ActiveRecord::Base's ActiveModel ancestry on the PostgreSQL CRuby tree,
# from the activerecord native Core runs (Shopify/rails 5813559f4aed,
# 8.2.0.alpha): base.rb's `include ActiveModel::API` (its Conversion) and
# `include Serialization` (lib/active_record/serialization.rb, which
# includes ActiveModel::Serializers::JSON), with the primary key's
# `to_key` (attribute_methods/primary_key.rb) and `attribute_names`
# (attribute_methods.rb) the serialization reads. Over the ActiveModel
# ports (runtime/active_model_naming.rb, runtime/active_model_serialization.rb),
# which runtime/gem_facades.rb requires just before this file.
#
# Differences, at the edges:
# - ActiveSupport::Concern is not used. Serialization's `included do
#   self.include_root_in_json = false end` runs where Base includes it,
#   and Serializers::JSON is included into Base before Serialization
#   rather than recorded as a Concern dependency: the ancestors are
#   Rails' (Serialization, then Serializers::JSON, then Serialization's
#   ActiveModel half), but Serialization itself does not include it.
# - `to_key` and `attribute_names` are Base's own methods, not those of
#   ActiveRecord::AttributeMethods and its PrimaryKey module. Rails'
#   PrimaryKey#to_key overrides Conversion#to_key; Base's own method
#   overrides the Conversion port included here the same way.
# - `attribute_names` is the record's loaded columns: `attributes`' keys,
#   less the ones a partial `select` left out (`_note_unloaded`), where
#   Rails reads `@attributes.keys`.
# - `inheritance_column` (model_schema.rb's class attribute, default
#   "type", its writer storing `value.to_s`) is a class-side reader and
#   writer over a per-class value with the superclass's as default, as
#   `class_attribute` reads. Rails' `_has_attribute?(inheritance_column)`
#   is a schema column of that name. The runtime's STI scoping and
#   instantiation read the `type` column whatever it is set to
#   (lower::sti_scope).
# - A column a partial `select` left out (`_note_unloaded`) raises
#   ActiveModel::MissingAttributeError when read, as Rails' reader does
#   (attribute_methods/read.rb), until `reload` reads the whole row.
#   Rails then saves only the changed columns; the emitted UPDATE writes
#   every column, so saving such a record raises NotImplementedError
#   rather than writing NULL over the columns it never read.
# - The CRuby overlay's Base#as_json (runtime/active_record_serialization.rb),
#   the `only:` subset the other CRuby trees answer, is removed: here
#   Base's `as_json` and `serializable_hash` are its modules', as in
#   Rails, so a module included into Base later supers into them.

module ActiveModel
  class MissingAttributeError < NoMethodError
  end
end

module ActiveRecord # :nodoc:
  # Readers of the columns a partial `select` left out, and the UPDATE
  # that would overwrite them (see the header).
  module UnloadedAttributes
    def _note_unloaded(row)
      super
      unloaded = @__unloaded_columns
      return nil if unloaded.nil?
      unloaded.each do |col|
        define_singleton_method(col) do
          if @__unloaded_columns&.include?(col)
            raise ActiveModel::MissingAttributeError, "missing attribute '#{col}' for #{self.class.name}"
          end
          super()
        end
      end
      define_singleton_method(:_adapter_update) do
        missing = @__unloaded_columns
        if missing
          raise NotImplementedError, "#{self.class.name}: saving a record loaded by a partial select " \
            "(#{missing.join(", ")} not loaded) is not ported; the UPDATE writes every column"
        end
        super()
      end
      nil
    end

    def reload
      @__unloaded_columns = nil
      super
    end
  end

  # = Active Record \Serialization
  module Serialization
    def serializable_hash(options = nil)
      inheritance_column = self.class.inheritance_column
      if !inheritance_column.empty? && self.class.schema_columns.include?(inheritance_column.to_sym)
        options = options ? options.dup : {}

        options[:except] = Array(options[:except]).map(&:to_s)
        options[:except] |= Array(inheritance_column)
      end

      super(options)
    end

    private
      def attribute_names_for_serialization
        attribute_names
      end
  end

  class Base
    prepend UnloadedAttributes
    include ActiveModel::Conversion
    include ActiveModel::Serializers::JSON
    include Serialization

    remove_method :as_json

    self.include_root_in_json = false
    self.param_delimiter = "_"

    class << self
      # The name of the table column which stores the class name on
      # single-table inheritance situations. "type" unless this class or
      # an ancestor set it.
      def inheritance_column
        if instance_variable_defined?(:@__inheritance_column)
          @__inheritance_column
        elsif superclass.respond_to?(:inheritance_column)
          superclass.inheritance_column
        else
          "type"
        end
      end

      def inheritance_column=(value)
        @__inheritance_column = value.to_s
      end
    end

    # Returns this record's primary key value wrapped in an array if one is
    # available.
    def to_key
      key = id
      Array(key) if key
    end

    # Returns an array of names for the attributes available on this object.
    def attribute_names
      names = attributes.keys
      unloaded = @__unloaded_columns
      unloaded ? names - unloaded.map(&:to_s) : names
    end

    # activerecord/lib/active_record/attribute_methods.rb: the column
    # object for `name` (its precision and scale for a writer's rounding),
    # a NullColumn for a name the table lacks.
    def column_for_attribute(name)
      self.class.column_for_attribute(name)
    end

    # activerecord/lib/active_record/attribute_methods/write.rb: a gem's
    # unit-name writer (`write_attribute(unit_field_name, name)`) reaches the
    # column through the model's generated `[]=`, which casts as the
    # attribute writers do.
    def write_attribute(attr_name, value)
      self[attr_name] = value
    end

    # attribute_methods/read.rb: the column's value through the model's
    # generated `[]`, past any reader the class overrides, as an app
    # module calls it on a record it holds (a validation's condition
    # reading a foreign key column). The validations ports ask
    # `respond_to?(:read_attribute)` and read the same value.
    def read_attribute(attr_name)
      self[attr_name]
    end
  end
end
