# frozen_string_literal: true

# Module#silence_redefinition_of_method, #redefine_method,
# #redefine_singleton_method and #method_visibility (Shopify/rails
# 5813559f4aed, 8.2.0.alpha
# activesupport/lib/active_support/core_ext/module/redefine_method.rb),
# verbatim; Rails' doc comments dropped. Required by the class_attribute
# port; `require "active_support/core_ext/module/redefine_method"` from a
# native gem loads it (runtime/features, `project::FRAMEWORK_FEATURE_PORTS`).
#
# The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).

class Module
  def silence_redefinition_of_method(method)
    if method_defined?(method) || private_method_defined?(method)
      alias_method method, method
    end
  end

  def redefine_method(method, &block)
    visibility = method_visibility(method)
    silence_redefinition_of_method(method)
    define_method(method, &block)
    send(visibility, method)
  end

  def redefine_singleton_method(method, &block)
    singleton_class.redefine_method(method, &block)
  end

  def method_visibility(method) # :nodoc:
    case
    when private_method_defined?(method)
      :private
    when protected_method_defined?(method)
      :protected
    else
      :public
    end
  end
end
