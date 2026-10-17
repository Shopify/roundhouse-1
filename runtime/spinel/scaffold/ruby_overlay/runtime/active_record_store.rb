# frozen_string_literal: true
# `ActiveRecord::Store` (activerecord 8.x active_record/store.rb): the
# accessors over a serialized Hash column. `store_accessor(:settings,
# :theme)` defines `theme`, `theme=`, `theme_changed?`, `theme_change`,
# `theme_was`, `saved_change_to_theme?`, `saved_change_to_theme` and
# `theme_before_last_save` on a module the class includes, so a method
# the class defines itself wins; `store` is `serialize` with an
# indifferent-access coder, then `store_accessor`. The
# activerecord-typedstore gem's `typed_store` ends in `store_accessor`
# (Core's `redactable_typed_store`), and reads the stores a class and its
# ancestors declare back through `stored_attributes`.
#
# Reads and writes go through the column's type (`type_for_attribute(col)
# .accessor`: `Type::Serialized#accessor` is IndifferentHashAccessor,
# `Type::Json#accessor` StringKeyedHashAccessor), as Rails does, so the
# column's own coder and cast see what is stored. Dynamic Ruby
# (define_method with computed names, public_send), so ruby lane only.
module ActiveRecord
  module Store
    # A store the column's type keeps as a Hash: reads index it by `key`,
    # a write mutates it in place (dirty tracking diffs the column), and
    # a nil column is made a Hash before either.
    class HashAccessor # :nodoc:
      def self.read(object, attribute, key)
        prepare(object, attribute)
        object.public_send(attribute)[key]
      end

      def self.write(object, attribute, key, value)
        prepare(object, attribute)
        if value != read(object, attribute, key)
          object.public_send(attribute)[key] = value
        end
      end

      def self.prepare(object, attribute)
        object.public_send(:"#{attribute}=", {}) unless object.public_send(attribute)
      end
    end

    class StringKeyedHashAccessor < HashAccessor # :nodoc:
      def self.read(object, attribute, key)
        super(object, attribute, key.to_s)
      end

      def self.write(object, attribute, key, value)
        super(object, attribute, key.to_s, value)
      end
    end

    class IndifferentHashAccessor < HashAccessor # :nodoc:
      # The column's cast may hand back a plain Hash (string keys), so a
      # read looks the key up through an indifferent view of it.
      def self.read(object, attribute, key)
        store = object.public_send(attribute)
        IndifferentCoder.as_indifferent_hash(store)[key] if store
      end

      def self.write(object, attribute, key, value)
        prepare(object, attribute)
        if value != read(object, attribute, key)
          object.public_send(attribute)[key.to_s] = value
        end
      end

      def self.prepare(object, store_attribute)
        attribute = object.public_send(store_attribute)
        unless attribute.is_a?(ActiveSupport::HashWithIndifferentAccess)
          attribute = IndifferentCoder.as_indifferent_hash(attribute)
          object.public_send(:"#{store_attribute}=", attribute)
        end
        attribute
      end
    end

    # The coder `store` serializes the column with: the app's coder (any
    # object answering `load` and `dump`), else a YAML column of the
    # given class, with every Hash it loads or dumps made indifferent.
    class IndifferentCoder # :nodoc:
      def initialize(attr_name, coder_or_class_name)
        @coder =
          if coder_or_class_name.respond_to?(:load) && coder_or_class_name.respond_to?(:dump)
            coder_or_class_name
          else
            unless defined?(ActiveRecord::Coders::YAMLColumn)
              raise NotImplementedError, "ActiveRecord::Store.store(#{attr_name.inspect}): ActiveRecord::Coders::YAMLColumn is not ported; pass `coder:` (any object answering load and dump)"
            end
            ActiveRecord::Coders::YAMLColumn.new(attr_name, coder_or_class_name || Object)
          end
      end

      def dump(obj)
        @coder.dump self.class.as_indifferent_hash(obj)
      end

      def load(yaml)
        self.class.as_indifferent_hash(@coder.load(yaml || ""))
      end

      def self.as_indifferent_hash(obj)
        case obj
        when ActiveSupport::HashWithIndifferentAccess
          obj
        when Hash
          obj.with_indifferent_access
        else
          ActiveSupport::HashWithIndifferentAccess.new
        end
      end
    end

    module ClassMethods
      def store(store_attribute, options = {})
        serialize store_attribute, coder: IndifferentCoder.new(store_attribute, options[:coder])
        store_accessor(store_attribute, options[:accessors], **options.slice(:prefix, :suffix)) if options.key?(:accessors)
      end

      def store_accessor(store_attribute, *keys, prefix: nil, suffix: nil)
        keys = keys.flatten

        accessor_prefix =
          case prefix
          when String, Symbol then "#{prefix}_"
          when true then "#{store_attribute}_"
          else ""
          end
        accessor_suffix =
          case suffix
          when String, Symbol then "_#{suffix}"
          when true then "_#{store_attribute}"
          else ""
          end

        _store_accessors_module.module_eval do
          keys.each do |key|
            accessor_key = "#{accessor_prefix}#{key}#{accessor_suffix}"

            define_method("#{accessor_key}=") do |value|
              write_store_attribute(store_attribute, key, value)
            end

            define_method(accessor_key) do
              read_store_attribute(store_attribute, key)
            end

            define_method("#{accessor_key}_changed?") do
              return false unless attribute_changed?(store_attribute)
              prev_store, new_store = changes[store_attribute.to_s]
              prev_store&.dig(key) != new_store&.dig(key)
            end

            define_method("#{accessor_key}_change") do
              return unless attribute_changed?(store_attribute)
              prev_store, new_store = changes[store_attribute.to_s]
              [prev_store&.dig(key), new_store&.dig(key)]
            end

            define_method("#{accessor_key}_was") do
              return unless attribute_changed?(store_attribute)
              prev_store, _new_store = changes[store_attribute.to_s]
              prev_store&.dig(key)
            end

            define_method("saved_change_to_#{accessor_key}?") do
              return false unless saved_change_to_attribute?(store_attribute)
              prev_store, new_store = saved_change_to_attribute(store_attribute)
              prev_store&.dig(key) != new_store&.dig(key)
            end

            define_method("saved_change_to_#{accessor_key}") do
              return unless saved_change_to_attribute?(store_attribute)
              prev_store, new_store = saved_change_to_attribute(store_attribute)
              [prev_store&.dig(key), new_store&.dig(key)]
            end

            define_method("#{accessor_key}_before_last_save") do
              return unless saved_change_to_attribute?(store_attribute)
              prev_store, _new_store = saved_change_to_attribute(store_attribute)
              prev_store&.dig(key)
            end
          end
        end

        # A class's own hash, so each class in the hierarchy keeps its
        # own list (Rails' `local_stored_attributes`).
        @local_stored_attributes ||= {}
        @local_stored_attributes[store_attribute] ||= []
        @local_stored_attributes[store_attribute] |= keys
      end

      def _store_accessors_module # :nodoc:
        @_store_accessors_module ||= begin
          mod = Module.new
          include mod
          mod
        end
      end

      def local_stored_attributes
        @local_stored_attributes
      end

      # The keys each store declares, over the class and its ancestors.
      def stored_attributes
        parent = superclass.respond_to?(:stored_attributes) ? superclass.stored_attributes : {}
        if local_stored_attributes
          parent.merge!(local_stored_attributes) { |_k, a, b| a | b }
        end
        parent
      end
    end

    private
      def read_store_attribute(store_attribute, key)
        accessor = store_accessor_for(store_attribute)
        accessor.read(self, store_attribute, key)
      end

      def write_store_attribute(store_attribute, key, value)
        accessor = store_accessor_for(store_attribute)
        accessor.write(self, store_attribute, key, value)
      end

      def store_accessor_for(store_attribute)
        self.class.type_for_attribute(store_attribute).accessor
      end
  end

  class Base
    extend Store::ClassMethods
    include Store
  end
end
