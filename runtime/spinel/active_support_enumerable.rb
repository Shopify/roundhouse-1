# frozen_string_literal: true

# activesupport/lib/active_support/core_ext/enumerable.rb (Shopify/rails
# 5813559f4aed, 8.2.0.alpha), verbatim; Rails' doc comments dropped.
# `require "active_support/core_ext/enumerable"` from a native gem loads it
# (runtime/features, `project::FRAMEWORK_FEATURE_PORTS`).
#
# The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).

module ActiveSupport
  module EnumerableCoreExt # :nodoc:
    module Constants
      private
        def const_missing(name)
          if name == :SoleItemExpectedError
            ::ActiveSupport::EnumerableCoreExt::SoleItemExpectedError
          else
            super
          end
        end
    end
  end
end

module Enumerable
  class SoleItemExpectedError < StandardError; end

  ActiveSupport::EnumerableCoreExt::SoleItemExpectedError = remove_const(:SoleItemExpectedError)
  singleton_class.prepend(ActiveSupport::EnumerableCoreExt::Constants)

  def minimum(key)
    map(&key).min
  end

  def maximum(key)
    map(&key).max
  end

  def index_by
    if block_given?
      result = {}
      each { |elem| result[yield(elem)] = elem }
      result
    else
      to_enum(:index_by) { size if respond_to?(:size) }
    end
  end

  def index_with(default = (no_default = true))
    if block_given?
      to_h { |elem| [elem, yield(elem)] } # rubocop:disable Rails/IndexWith
    elsif no_default
      to_enum(:index_with) { size if respond_to?(:size) }
    else
      to_h { |elem| [elem, default] } # rubocop:disable Rails/IndexWith
    end
  end

  def many?
    cnt = 0
    if block_given?
      any? do |*args|
        cnt += 1 if yield(*args)
        cnt > 1
      end
    else
      any? { (cnt += 1) > 1 }
    end
  end

  def including(*elements)
    to_a.including(*elements)
  end

  def exclude?(object)
    !include?(object)
  end

  def excluding(*elements)
    elements.flatten!(1)
    reject { |element| elements.include?(element) }
  end
  alias :without :excluding

  def pluck(*keys)
    if keys.many?
      map { |element| keys.map { |key| element[key] } }
    else
      key = keys.first
      map { |element| element[key] }
    end
  end

  def pick(*keys)
    return if none?

    if keys.many?
      keys.map { |key| first[key] }
    else
      first[keys.first]
    end
  end

  def compact_blank
    reject(&:blank?)
  end

  def in_order_of(key, series, filter: true)
    if filter
      group_by(&key).values_at(*series).compact.flatten(1)
    else
      sort_by { |v| series.index(v.public_send(key)) || series.size }
    end
  end

  def sole
    result = nil
    found = false

    each do |*element|
      if found
        raise SoleItemExpectedError, "multiple items found"
      end

      result = element.size == 1 ? element[0] : element
      found = true
    end

    if found
      result
    else
      raise SoleItemExpectedError, "no item found"
    end
  end
end

class Hash
  def compact_blank # :nodoc:
    reject { |_k, v| v.blank? }
  end

  def compact_blank!
    delete_if { |_k, v| v.blank? }
  end
end

class Range # :nodoc:
  def sum(initial_value = 0)
    if block_given? || !(first.is_a?(Integer) && last.is_a?(Integer))
      super
    else
      actual_last = exclude_end? ? (last - 1) : last
      if actual_last >= first
        initial_value + (actual_last - first + 1) * (actual_last + first) / 2
      else
        initial_value
      end
    end
  end
end

class Array # :nodoc:
  def compact_blank!
    delete_if(&:blank?)
  end
end
