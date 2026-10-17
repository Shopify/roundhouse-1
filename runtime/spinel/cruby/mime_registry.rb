# frozen_string_literal: true
# Mime::Type.register and .register_alias (actionpack 8.2.0.alpha,
# lib/action_dispatch/http/mime_type.rb) for the PostgreSQL CRuby tree.
# runtime/ruby/mime.rb carries the frozen table actionpack's own
# mime_types.rb builds; an app extends it from an initializer (Core's
# config/initializers/006_mime_types.rb registers `:graphql`, and
# `ActionDispatch::IntegrationTest.register_encoder(:graphql)` raises for
# a type that is not registered). The additions live beside the frozen
# table and answer `lookup`, `lookup_by_extension` and `Mime[]` first.
#
# Differences, at the edges: the synonyms of `register` map to the
# registered type as upstream's LOOKUP does, but `Mime::SET`, `Mime::
# SET.symbols`, the Accept-header parser and `register_callback` are not
# carried (runtime/ruby/mime.rb leaves them out).
#
# Appended to runtime/mime.rb in the PostgreSQL CRuby tree
# (`project::apply_postgres_bootstrap`), so it loads with the port.

module Mime
  module Registry
    def registered_lookup
      @registered_lookup ||= {}
    end

    def registered_extensions
      @registered_extensions ||= {}
    end

    def register(string, symbol, mime_type_synonyms = [], extension_synonyms = [], skip_lookup = false)
      new_mime = Mime::Type.new(string, symbol)
      unless skip_lookup
        ([string] + mime_type_synonyms).each { |name| registered_lookup[name] = new_mime }
      end
      ([symbol] + extension_synonyms).each { |extension| registered_extensions[extension.to_s] = new_mime }
      new_mime
    end

    def register_alias(string, symbol, extension_synonyms = [])
      register(string, symbol, [], extension_synonyms, true)
    end

    def lookup(string)
      registered_lookup[string] || super
    end

    def lookup_by_extension(extension)
      registered_extensions[extension.to_s] || super
    end
  end

  class Type
    class << self
      prepend Mime::Registry
    end
  end
end
