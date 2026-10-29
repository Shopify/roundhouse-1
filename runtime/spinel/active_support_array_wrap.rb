# frozen_string_literal: true

# activesupport/lib/active_support/core_ext/array/wrap.rb (Shopify/rails
# 5813559f4aed, 8.2.0.alpha), verbatim; Rails' doc comments dropped.
# `require "active_support/core_ext/array/wrap"` from a native gem loads it
# (runtime/features, `project::FRAMEWORK_FEATURE_PORTS`).
#
# The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).

class Array
  def self.wrap(object)
    if object.nil?
      []
    elsif object.respond_to?(:to_ary)
      object.to_ary || [object]
    else
      [object]
    end
  end
end
