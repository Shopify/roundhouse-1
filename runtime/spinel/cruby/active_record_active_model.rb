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
# - Rails' `_has_attribute?(inheritance_column)` is the `type` column:
#   the inheritance column the runtime knows (lower::sti_scope).
# - The CRuby overlay's Base#as_json (runtime/active_record_serialization.rb),
#   the `only:` subset the other CRuby trees answer, is removed: here
#   Base's `as_json` and `serializable_hash` are its modules', as in
#   Rails, so a module included into Base later supers into them.

module ActiveRecord # :nodoc:
  # = Active Record \Serialization
  module Serialization
    def serializable_hash(options = nil)
      if self.class.schema_columns.include?(:type)
        options = options ? options.dup : {}

        options[:except] = Array(options[:except]).map(&:to_s)
        options[:except] |= Array("type")
      end

      super(options)
    end

    private
      def attribute_names_for_serialization
        attribute_names
      end
  end

  class Base
    include ActiveModel::Conversion
    include ActiveModel::Serializers::JSON
    include Serialization

    remove_method :as_json

    self.include_root_in_json = false
    self.param_delimiter = "_"

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
  end
end
