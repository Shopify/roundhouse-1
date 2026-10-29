# frozen_string_literal: true

# activesupport/lib/active_support/core_ext/array/access.rb (Shopify/rails
# 5813559f4aed, 8.2.0.alpha), verbatim; Rails' doc comments dropped.
# `require "active_support/core_ext/array/access"` from a native gem loads it
# (runtime/features, `project::FRAMEWORK_FEATURE_PORTS`).
#
# The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).

class Array
  def from(position)
    self[position, length] || []
  end

  def to(position)
    if position >= 0
      take position + 1
    else
      self[0..position]
    end
  end

  def including(*elements)
    self + elements.flatten(1)
  end

  def excluding(*elements)
    self - elements.flatten(1)
  end
  alias :without :excluding

  def second
    self[1]
  end

  def third
    self[2]
  end

  def fourth
    self[3]
  end

  def fifth
    self[4]
  end

  def forty_two
    self[41]
  end

  def third_to_last
    self[-3]
  end

  def second_to_last
    self[-2]
  end
end
