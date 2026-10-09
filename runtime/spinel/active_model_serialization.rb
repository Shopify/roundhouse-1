# frozen_string_literal: true

# ActiveModel::Serialization and ActiveModel::Serializers::JSON, ported
# from the activemodel native Core runs (Shopify/rails 5813559f4aed,
# 8.2.0.alpha: lib/active_model/serialization.rb and serializers/json.rb)
# for the PostgreSQL CRuby tree, carried where app code (a gem compiled
# as source among it) names one of them, with the Naming port
# (runtime/active_model_naming.rb) that Serializers::JSON extends its
# includer with. frozen_record 0.27.4's serialization.rb includes
# Serializers::JSON in FrozenRecord::Base. Each method follows the native
# one.
#
# Differences, at the edges:
# - ActiveSupport::Concern is not used: Serializers::JSON's `included`
#   block (`extend ActiveModel::Naming`, `class_attribute
#   :include_root_in_json, instance_writer: false, default: false`) is
#   written out as Concern#append_features, and the class_attribute as
#   the class reader, writer and predicate and the instance reader and
#   predicate it defines.
# - `index_with` (core_ext/enumerable.rb) is written out where
#   `serializable_attributes` calls it: each name to its value, in order.
# - `as_json` answers the serializable hash through ActiveSupport::JSON's
#   `as_json` table (runtime/spinel/active_support_json.rb,
#   `Encoding.as_json`), which is what `hash.as_json` is under
#   core_ext/object/json.rb; this tree does not reopen Hash with it.
#   `from_json` decodes with that port's ActiveSupport::JSON.decode.

require_relative "active_support_json"

# ---- activemodel/lib/active_model/serialization.rb
module ActiveModel
  module Serialization
    # Returns a serialized hash of your object.
    def serializable_hash(options = nil)
      attribute_names = attribute_names_for_serialization

      return serializable_attributes(attribute_names) if options.blank?

      if only = options[:only]
        attribute_names = Array(only).map(&:to_s) & attribute_names
      elsif except = options[:except]
        attribute_names -= Array(except).map(&:to_s)
      end

      hash = serializable_attributes(attribute_names)

      Array(options[:methods]).each { |m| hash[m.to_s] = send(m) }

      serializable_add_includes(options) do |association, records, opts|
        hash[association.to_s] = if records.respond_to?(:to_ary)
          records.to_ary.map { |a| a.serializable_hash(opts) }
        else
          records.serializable_hash(opts)
        end
      end

      hash
    end

    # Hook method defining how an attribute value should be retrieved for
    # serialization. By default this is assumed to be an instance named
    # after the attribute.
    def read_attribute_for_serialization(key)
      send(key)
    end

    private
      def attribute_names_for_serialization
        attributes.keys
      end

      # `attribute_names.index_with { |n| read_attribute_for_serialization(n) }`
      def serializable_attributes(attribute_names)
        attribute_names.each_with_object({}) { |n, hash| hash[n] = read_attribute_for_serialization(n) }
      end

      # Add associations specified via the <tt>:include</tt> option.
      #
      # Expects a block that takes as arguments:
      #   +association+ - name of the association
      #   +records+     - the association record(s) to be serialized
      #   +opts+        - options for the association records
      def serializable_add_includes(options = {}) # :nodoc:
        return unless includes = options[:include]

        unless includes.is_a?(Hash)
          includes = Hash[Array(includes).flat_map { |n| n.is_a?(Hash) ? n.to_a : [[n, {}]] }]
        end

        includes.each do |association, opts|
          if records = send(association)
            yield association, records, opts
          end
        end
      end
  end
end

# ---- activemodel/lib/active_model/serializers/json.rb
module ActiveModel
  module Serializers
    module JSON
      include ActiveModel::Serialization

      # `included do ... end`, run as Concern#append_features runs it:
      # once, and not again for a class that already has the module.
      #
      #   extend ActiveModel::Naming
      #   class_attribute :include_root_in_json, instance_writer: false, default: false
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
        base.extend ActiveModel::Naming
        base.extend ClassMethods
        base.instance_variable_set(:@__class_attr_include_root_in_json, false)
      end

      # The class_attribute's instance reader and predicate.
      def include_root_in_json
        defined?(@include_root_in_json) ? @include_root_in_json : self.class.include_root_in_json
      end

      def include_root_in_json?
        !!include_root_in_json
      end

      # The class_attribute's class reader, writer and predicate.
      module ClassMethods # :nodoc:
        def include_root_in_json
          owner = self
          owner = owner.superclass until owner.instance_variable_defined?(:@__class_attr_include_root_in_json)
          owner.instance_variable_get(:@__class_attr_include_root_in_json)
        end

        def include_root_in_json=(value)
          @__class_attr_include_root_in_json = value
        end

        def include_root_in_json?
          !!include_root_in_json
        end
      end

      # Returns a hash representing the model.
      def as_json(options = nil)
        root = if options && options.key?(:root)
          options[:root]
        else
          include_root_in_json
        end

        hash = ActiveSupport::JSON::Encoding.as_json(serializable_hash(options))
        if root
          root = model_name.element if root == true
          { root => hash }
        else
          hash
        end
      end

      # Sets the model +attributes+ from a JSON string. Returns +self+.
      def from_json(json, include_root = include_root_in_json)
        hash = ActiveSupport::JSON.decode(json)
        hash = hash.values.first if include_root

        if respond_to?(:assign_attributes)
          assign_attributes(hash)
        else
          self.attributes = hash
        end

        self
      end
    end
  end
end
