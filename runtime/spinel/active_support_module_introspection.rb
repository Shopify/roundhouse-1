# frozen_string_literal: true

# Module#module_parent_name, #module_parent and #module_parents (Shopify/rails
# 5813559f4aed, 8.2.0.alpha
# activesupport/lib/active_support/core_ext/module/introspection.rb),
# verbatim; Rails' doc comments dropped. Native Rails loads it with
# active_support/all, which app code calls with no require
# (`project::BOOT_LOADED_CORE_EXTENSIONS`).
#
# The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).

class Module
  def module_parent_name
    if defined?(@parent_name)
      @parent_name
    else
      parent_name = name =~ /::[^:]+\z/ ? -$` : nil
      @parent_name = parent_name unless frozen?
      parent_name
    end
  end

  def module_parent
    module_parent_name ? ActiveSupport::Inflector.constantize(module_parent_name) : Object
  end

  def module_parents
    parents = []
    if module_parent_name
      parts = module_parent_name.split("::")
      until parts.empty?
        parents << ActiveSupport::Inflector.constantize(parts * "::")
        parts.pop
      end
    end
    parents << Object unless parents.include? Object
    parents
  end
end
