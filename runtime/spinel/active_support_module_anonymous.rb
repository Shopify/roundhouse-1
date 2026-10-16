# frozen_string_literal: true

# Module#anonymous? (Shopify/rails 5813559f4aed, 8.2.0.alpha
# activesupport/lib/active_support/core_ext/module/anonymous.rb), verbatim;
# Rails' doc comments dropped. Native Rails loads it with active_support/all,
# which app code calls with no require (Core's
# `GraphApi::Schema.error_source_path` walks a type class up to its first
# named superclass with `anonymous?`).
#
# The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).

class Module
  def anonymous?
    name.nil?
  end
end
