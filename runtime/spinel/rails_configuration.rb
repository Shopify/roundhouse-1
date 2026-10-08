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

  # `Rails.logger` takes the stdlib Logger's call forms here: the shared
  # runtime's Logger (rails.rb) takes one positional message, which is
  # every call the strict targets compile, while gems on this tree log
  # lazily (`Dalli.logger.debug { "..." }`, `.warn { "..." }`; dalli logs
  # through Rails.logger when Rails is defined). As in the stdlib
  # (`Logger#add`), a block is the message and the argument becomes the
  # progname. Same class_eval reasoning as above: Rails::Logger is the
  # shared runtime's to gate.
  Logger.class_eval do
    %i[info error warn fatal].each do |level|
      write = instance_method(level)
      define_method(level) do |progname = nil, &block|
        write.bind_call(self, block ? block.call : progname)
      end
    end

    # Dropped, as the shared runtime's debug is; the block is not called.
    define_method(:debug) { |progname = nil, &block| nil }
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

    # Two framework keys (railties 8.2.0.alpha
    # application/configuration.rb) a gem's railtie reads, outside the
    # lifted ones. `filter_parameters` is the app's list, lifted onto the
    # reopen as a plain reader with the framework's `[]` default
    # (runtime/rails.rb); bugsnag 5.5's after_initialize hook reads it.
    def filter_parameters
      @application.filter_parameters
    end

    # `config.middleware`, the application's MiddlewareStackProxy: the
    # one a railtie's `config.app_middleware` records into
    # (Rails::Railtie::Configuration#app_middleware), which boot names
    # operation by operation (error[gem_railtie_middleware_not_applied]):
    # the tree has no middleware stack to apply them to. bugsnag 5.5's
    # `bugsnag.use_rack_middleware` initializer writes it. Without the
    # Railtie port (runtime/rails_railtie.rb), no such key.
    def middleware
      return method_missing(:middleware) unless defined?(::Rails::Railtie::Configuration)

      ::Rails::Railtie::Configuration.new.app_middleware
    end

    # `config.eager_load_namespaces`, the one list every railtie's
    # configuration shares (Railtie::Configuration's
    # `@@eager_load_namespaces`; Application::Configuration inherits it).
    # worldwide 1.29.0's `worldwide.setup` initializer appends Worldwide
    # through `application.config`. Rails' Finisher calls `eager_load!` on
    # each entry only under `config.eager_load`. Without the Railtie port,
    # no such key.
    def eager_load_namespaces
      return method_missing(:eager_load_namespaces) unless defined?(::Rails::Railtie::Configuration)

      ::Rails::Railtie::Configuration.eager_load_namespaces
    end

    # The boot hooks `Rails.application.config` inherits from
    # Rails::Railtie::Configuration (Application::Configuration <
    # Engine::Configuration < Railtie::Configuration): an app initializer's
    # `Rails.application.config.after_initialize { ... }` registers on the
    # same load hooks a gem's railtie does, which the Railtie port's
    # Initializable phases run (`:after_initialize` in the Finisher).
    # Without the port, no such key.
    %i[before_configuration before_eager_load before_initialize after_initialize after_routes_loaded to_prepare].each do |hook|
      define_method(hook) do |&block|
        return method_missing(hook, &block) unless defined?(::Rails::Railtie::Configuration)

        ::Rails::Railtie::Configuration.new.public_send(hook, &block)
      end
    end

    private

    def lifted?(name)
      @application.respond_to?("#{name.to_s.delete_suffix("=")}__build")
    end
  end
end
