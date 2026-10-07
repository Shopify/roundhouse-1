# `Rails.configuration` and `Rails.application.config` — railties'
# application configuration (railties 8.1.4 rails.rb: `def
# configuration; application.config; end`).
#
# CRuby/JRuby only, pushed at the ruby-family fork in `project.rs` like
# module_delegate.rb: an object that answers the names the app's config
# writes defined is a computed-name dispatch, which the strict targets
# cannot compile. Where this file is absent, `check` refuses each
# `Rails.configuration` read as a runtime constant
# (`analyze::runtime_reads`).
#
# WHAT IT ANSWERS. Rails' config object takes arbitrary keys: `config.k
# = v` in config/application.rb, an initializer or an in-repo engine's
# class body IS the definition of `k`. Ingest lifts each such write to
# three methods on the app's `Rails::Application` reopen (`k__build`,
# `k`, `k=`; see `extract_config_assignments`), and `lower::
# config_reader` rewrites every read it can see to `Rails.application.k`.
# This object is for the reads it cannot see: a configuration passed
# around as a value, or a read in code the lowering never walks. It
# answers exactly the lifted keys, through the same readers, so both
# spellings share the one built-once value. Any other name — a framework
# key the emitted app does not model, or a key nothing carried writes —
# is a NoMethodError, as it would be on a config object that has no such
# key, rather than a nil nobody set.
#
# `Rails.application.config` is added with `class_eval` on the runtime's
# own class (rails.rb, required first), not a `class Application`
# reopen: this file's declared namespaces are what `check` gates, and
# `Rails::Application` is not this file's to gate.
require_relative "rails"

module Rails
  def self.configuration
    Configuration.new(application)
  end

  Application.class_eval do
    def config
      Rails.configuration
    end
  end

  class Configuration
    def initialize(application)
      @application = application
    end

    def respond_to_missing?(name, include_private = false)
      lifted?(name) || super
    end

    def method_missing(name, *args, &block)
      return super unless lifted?(name)

      @application.public_send(name, *args, &block)
    end

    private

    def lifted?(name)
      @application.respond_to?("#{name.to_s.delete_suffix("=")}__build")
    end
  end
end
