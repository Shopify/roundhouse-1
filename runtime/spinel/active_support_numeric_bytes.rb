# frozen_string_literal: true

# Numeric#bytes, #kilobytes, ... #zettabytes and the KILOBYTE... constants
# (Shopify/rails 5813559f4aed, 8.2.0.alpha
# activesupport/lib/active_support/core_ext/numeric/bytes.rb), verbatim;
# Rails' doc comments dropped. `require "active_support/core_ext/numeric/bytes"`
# (or core_ext/numeric) from a native gem (polaris' `12.kilobytes`) loads it
# (runtime/features, `project::FRAMEWORK_FEATURE_PORTS`).
#
# The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).

class Numeric
  KILOBYTE = 1024
  MEGABYTE = KILOBYTE * 1024
  GIGABYTE = MEGABYTE * 1024
  TERABYTE = GIGABYTE * 1024
  PETABYTE = TERABYTE * 1024
  EXABYTE  = PETABYTE * 1024
  ZETTABYTE = EXABYTE * 1024

  def bytes
    self
  end
  alias :byte :bytes

  def kilobytes
    self * KILOBYTE
  end
  alias :kilobyte :kilobytes

  def megabytes
    self * MEGABYTE
  end
  alias :megabyte :megabytes

  def gigabytes
    self * GIGABYTE
  end
  alias :gigabyte :gigabytes

  def terabytes
    self * TERABYTE
  end
  alias :terabyte :terabytes

  def petabytes
    self * PETABYTE
  end
  alias :petabyte :petabytes

  def exabytes
    self * EXABYTE
  end
  alias :exabyte :exabytes

  def zettabytes
    self * ZETTABYTE
  end
  alias :zettabyte :zettabytes
end
