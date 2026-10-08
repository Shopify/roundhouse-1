# frozen_string_literal: true

# Hash#stringify_keys, #symbolize_keys, #assert_valid_keys and the
# deep_* key transforms (Shopify/rails 5813559f4aed, 8.2.0.alpha
# activesupport/lib/active_support/core_ext/hash/keys.rb), verbatim;
# Rails' doc comments dropped. `require "active_support/core_ext/hash/keys"`
# from a native gem loads it (runtime/features,
# `project::FRAMEWORK_FEATURE_PORTS`). ActiveSupport::HashWithIndifferentAccess
# (active_support_core_ext.rb) keeps its own overrides.
#
# The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).

class Hash
  def stringify_keys
    transform_keys { |k| Symbol === k ? k.name : k.to_s }
  end

  def stringify_keys!
    transform_keys! { |k| Symbol === k ? k.name : k.to_s }
  end

  def symbolize_keys
    transform_keys { |key| key.to_sym rescue key }
  end
  alias_method :to_options,  :symbolize_keys

  def symbolize_keys!
    transform_keys! { |key| key.to_sym rescue key }
  end
  alias_method :to_options!, :symbolize_keys!

  def assert_valid_keys(*valid_keys)
    valid_keys.flatten!
    each_key do |k|
      unless valid_keys.include?(k)
        raise ArgumentError.new("Unknown key: #{k.inspect}. Valid keys are: #{valid_keys.map(&:inspect).join(', ')}")
      end
    end
  end

  def deep_transform_keys(&block)
    _deep_transform_keys_in_object(self, &block)
  end

  def deep_transform_keys!(&block)
    _deep_transform_keys_in_object!(self, &block)
  end

  def deep_stringify_keys
    deep_transform_keys { |k| Symbol === k ? k.name : k.to_s }
  end

  def deep_stringify_keys!
    deep_transform_keys! { |k| Symbol === k ? k.name : k.to_s }
  end

  def deep_symbolize_keys
    deep_transform_keys { |key| key.to_sym rescue key }
  end

  def deep_symbolize_keys!
    deep_transform_keys! { |key| key.to_sym rescue key }
  end

  private
    def _deep_transform_keys_in_object(object, &block)
      case object
      when Hash
        object.each_with_object(self.class.new) do |(key, value), result|
          result[yield(key)] = _deep_transform_keys_in_object(value, &block)
        end
      when Array
        object.map { |e| _deep_transform_keys_in_object(e, &block) }
      else
        object
      end
    end

    def _deep_transform_keys_in_object!(object, &block)
      case object
      when Hash
        object.keys.each do |key|
          value = object.delete(key)
          object[yield(key)] = _deep_transform_keys_in_object!(value, &block)
        end
        object
      when Array
        object.map! { |e| _deep_transform_keys_in_object!(e, &block) }
      else
        object
      end
    end
end
