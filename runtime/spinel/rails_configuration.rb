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
# spellings share the one built-once value. Where the tree carries the
# Railtie port, any other name is the options store every railtie's
# configuration shares (`config.i18n`, which I18n::Railtie sets): a write
# stores it, a read of a key nothing set is a NoMethodError, as in Rails.
# Without the port, any other name — a framework key the emitted app does
# not model, or a key nothing carried writes — is a NoMethodError, rather
# than a nil nobody set.
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
      lifted?(name) || railtie_option?(name) || super
    end

    # A lifted key answers from the application reopen. Any other key is
    # the one options store every railtie's configuration shares
    # (Rails::Railtie::Configuration's `@@options`, which
    # Application::Configuration inherits): `config.i18n`, which
    # I18n::Railtie's body sets (runtime/active_support_i18n_railtie.rb),
    # read and written by a gem's railtie through `app.config` (phonelib
    # 0.10.27: `app.config.i18n.load_path = ...`). A read of a key nothing
    # set is still a NoMethodError there, as in Rails; a write stores it.
    # Without the Railtie port, only the lifted keys.
    def method_missing(name, *args, &block)
      if lifted?(name)
        @application.public_send(name, *args, &block)
      elsif defined?(::Rails::Railtie::Configuration)
        ::Rails::Railtie::Configuration.new.public_send(name, *args, &block)
      else
        super
      end
    end

    # Application::Configuration#reloading_enabled? is `enable_reloading`.
    # This tree never reloads code (ingest: `cache_classes`, "no code
    # reloading in the emitted app"), so it answers false whatever the
    # environment writes, and nothing a railtie installs under it (the I18n
    # railtie's file watcher) runs. Without the Railtie port, no such key.
    def reloading_enabled?
      return method_missing(:reloading_enabled?) unless defined?(::Rails::Railtie::Configuration)

      false
    end

    # `config.eager_load`, Application::Configuration's `attr_accessor
    # :eager_load` (railties 8.2.0.alpha application/configuration.rb),
    # which bootstrap.rb's `set_eager_load` initializer fills in with
    # `!config.reloading_enabled?` when the environment left it nil. The
    # environment's write is not a lifted key: ingest compiles it into
    # `RoundhouseAutoload.eager_load?` (app/models.rb), the value boot's
    # Finisher-style `eager_load!` branches on, so the reader answers that
    # same value. Core's config/environments/test.rb writes
    # `config.eager_load = BootLib.test_eager_load?`, and
    # ShardedTable::ClassMethods.extended reads it to decide whether a
    # sharded model verifies lazy loading. Without the autoload index,
    # whatever the options store holds (a NoMethodError if nothing set it).
    def eager_load
      return method_missing(:eager_load) unless defined?(::RoundhouseAutoload.eager_load?)

      ::RoundhouseAutoload.eager_load?
    end

    # A write after the environment's cannot change what boot loaded, and
    # storing it where the reader above would not see it would make the
    # two disagree, so it is refused by name. Without the autoload index,
    # the options store takes it.
    def eager_load=(value)
      return method_missing(:eager_load=, value) unless defined?(::RoundhouseAutoload.eager_load?)

      raise NotImplementedError, "error[config_eager_load_write_not_applied]: config.eager_load = #{value.inspect}: " \
        "this tree's eager loading is the environment's config.eager_load, compiled into RoundhouseAutoload.eager_load?"
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
    # each entry only under `config.eager_load` (Railtie.eager_load_step).
    # Without the Railtie port, no such key.
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

    def railtie_option?(name)
      defined?(::Rails::Railtie::Configuration) && ::Rails::Railtie::Configuration.new.respond_to?(name)
    end
  end
end
