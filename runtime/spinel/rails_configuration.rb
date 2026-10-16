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
#
# `Rails.logger` is here for the same reason it is not in rails.rb: the
# log device this tree writes is ActiveSupport::Logger over Ruby's Logger,
# which only the ruby-family trees carry (see `Rails.logger` below).
require_relative "rails"

module Rails
  # One configuration for the process, as natively (railties rails.rb:
  # `application.config`, the Application's one
  # Application::Configuration). Its state lives on the application's
  # class-level slots and this object's class, so building it once is
  # also what lets a test stub a key on it (Core:
  # `Rails.application.config.stubs(consider_all_requests_local: false)`)
  # and have the next `Rails.application.config` see the stub.
  def self.configuration
    @configuration ||= Configuration.new(application)
  end

  # `delegate :config, to: :instance` (railties application.rb,
  # `class << self`): `MyApp::Application.config`, the same configuration
  # as `Rails.application.config`.
  class << Application
    def config(...)
      Rails.configuration
    end
  end

  Application.class_eval do
    def config
      Rails.configuration
    end

    # railties 8.1.4 application.rb `config_for`: config/<name>.yml read as
    # ActiveSupport::ConfigurationFile.parse does (ERB rendered with
    # stdlib `ERB.new(content).result` when the file has a tag, then
    # `YAML.unsafe_load`), deep-symbolized, the env's section deep-merged
    # over `shared:`, and a Hash answered as an OrderedOptions. Core's
    # essentials Config reads `config_for(:memcached)` in the initializer
    # that names Rails.cache's store.
    def config_for(name, env: Rails.env)
      yaml = name.is_a?(Pathname) ? name : Pathname.new("#{Rails.root.join("config")}/#{name}.yml")
      raise "Could not load configuration. No such file - #{yaml}" unless yaml.exist?

      require "erb"
      require "yaml"
      content = File.read(yaml.to_s)
      warn "#{yaml} contains invisible non-breaking spaces, you may want to remove those" if content.include?(" ")
      begin
        source = content.include?("<%") ? ERB.new(content).tap { |e| e.filename = yaml.to_s }.result : content
        all_configs = YAML.unsafe_load(source) || {}
      rescue Psych::SyntaxError => error
        raise "YAML syntax error occurred while parsing #{yaml}. " \
              "Please note that YAML must be consistently indented using spaces. Tabs are not allowed. " \
              "Error: #{error.message}"
      end
      all_configs = Configuration::Custom.deep_symbolize(all_configs)
      config, shared = all_configs[env.to_sym], all_configs[:shared]

      if shared
        config = {} if config.nil? && shared.is_a?(Hash)
        if config.is_a?(Hash) && shared.is_a?(Hash)
          config = Configuration::Custom.deep_merge(shared, config)
        elsif config.nil?
          config = shared
        end
      end

      config = Configuration::Custom.empty_options.update(config) if config.is_a?(Hash)
      config
    end
  end

  # `Rails.context` — the gurgel gem's RailsContextExtension (Core's
  # `Rails.load_embedded_engines` reads `config/contexts.yml` and requires
  # the engines of the context `RAILS_CONTEXT` selects, "all" by default;
  # `Rails.context` answers that name as an inquirer). Core asks it
  # `Rails.context.essentials?` / `server_intents?` / `all?`, compares it
  # (`Rails.context != "essentials"`) and stores it as the request's
  # RAILS_CONTEXT. A StringInquirer like `Rails.env`: the name is the
  # value, a predicate for any other name is false. Read from ENV on each
  # call so a test that sets RAILS_CONTEXT sees it; the inquirer built for
  # the current name is kept (a stub or `equal?` check sees one object).
  def self.context
    name = ENV.fetch("RAILS_CONTEXT", "all")
    name = "all" if name.empty?
    cached = @context
    return cached if cached && cached == name
    @context = ActiveSupport::StringInquirer.new(name)
  end

  # `Rails.logger` on this tree is the logger railties' bootstrap
  # initializer makes (railties 8.2.0.alpha application/bootstrap.rb:34-64,
  # `initialize_logger`): `config.logger`, or else an ActiveSupport::Logger
  # (runtime/active_support_logger.rb, a ::Logger) on the environment's
  # log file, `log/<env>.log` under Rails.root, with
  # ActiveSupport::Logger::SimpleFormatter, wrapped in
  # ActiveSupport::TaggedLogging; then, unless it is one already, wrapped
  # in an ActiveSupport::BroadcastLogger at the level of
  # `config.log_level` (:debug unless set). So `Rails.logger.warn "x"`
  # appends "x\n" to log/test.log in a test run, as natively, the stdlib
  # Logger's call forms (`level`, `debug?`, the block form
  # `Dalli.logger.debug { ... }`, `add`, `silence`) are Ruby's own, and
  # `tagged`, `broadcasts` and `broadcast_to` are activesupport's.
  #
  # The shared runtime's Rails::Logger (rails.rb) is the strict targets'
  # logger: one positional message, written to stderr through Kernel#warn.
  # On this tree that was wrong where it mattered: a test environment that
  # turns warnings into errors (Core's StrictWarning, whose `warn` raises
  # StrictWarning::Offense) killed boot at the first log line, bugsnag's
  # "No API key has been set". Natively those lines go to the log device
  # and never reach Kernel#warn.
  #
  # Each value the initializer reads is taken from `Rails.configuration`
  # where it answers the key (a lifted key, or one a railtie stored), with
  # Rails' default otherwise: `logger` (nil), `log_file_size` (nil),
  # `paths["log"]` (log/<env>.log), `autoflush_log` (true),
  # `log_formatter` (SimpleFormatter), `log_level` (:debug) and
  # `broadcast_log_level` (nil). Ingest does not lift `config.logger` or
  # `config.log_level` today (error[environment_statement_not_carried]),
  # so on Core's tree each is Rails' default, which for Core's test.rb
  # (LOG_LEVEL and CI unset) writes the same lines to the same file.
  #
  # Not carried, by name: a configured logger that is neither a ::Logger
  # nor a BroadcastLogger (error[rails_logger_not_carried]).
  # A stand-in the shared reader memoized before this file loaded is not
  # this tree's logger; the first read below builds Rails'.
  if instance_variable_defined?(:@logger) && const_defined?(:Logger, false) && @logger.instance_of?(Logger)
    remove_instance_variable(:@logger)
  end

  class << self
    # The shared runtime's reader, replaced (removed first, so the
    # redefinition is not a warning under $VERBOSE). The runtime shake
    # drops it from an emitted rails.rb whose app never reads it, while a
    # gem's code (dalli, bugsnag) still may; this file's reader is not
    # shaken.
    remove_method :logger if method_defined?(:logger, false)

    # railties rails.rb `attr_accessor :logger`, which Rails fills in
    # `initialize_logger`. This tree's boot runs none of the bootstrap
    # initializers (rails_railtie.rb, BOOTSTRAP_INITIALIZERS), so the
    # first read stands in for that one, as ActiveSupport::LogSubscriber's
    # first `logger` read stands in for set_log_subscriber_logger. An
    # assigned logger, nil included, is kept.
    def logger
      @logger = initialize_logger unless instance_variable_defined?(:@logger)
      @logger
    end

    attr_writer :logger

    private

    def initialize_logger
      require_relative "active_support_logger"
      config = configuration
      logger = configured(config, :logger, nil)
      unless logger.nil? || logger.is_a?(::Logger) || logger.is_a?(ActiveSupport::BroadcastLogger)
        $stderr.puts "roundhouse: error[rails_logger_not_carried]: config.logger is #{logger.class.name || logger.class.inspect}, " \
          "which this tree cannot carry as Rails.logger (it carries a ::Logger or an ActiveSupport::BroadcastLogger); " \
          "Rails.logger is the framework default, an ActiveSupport::Logger on #{log_path(config)}"
        logger = nil
      end
      logger ||= begin
        size = configured(config, :log_file_size, nil)
        built = size ? ActiveSupport::Logger.new(default_log_file(config), 1, size) : ActiveSupport::Logger.new(default_log_file(config))
        built.formatter = configured(config, :log_formatter, ActiveSupport::Logger::SimpleFormatter.new)
        ActiveSupport::TaggedLogging.new(built)
      rescue StandardError
        path = log_path(config)
        fallback = ActiveSupport::TaggedLogging.new(ActiveSupport::Logger.new(STDERR))
        fallback.level = ActiveSupport::Logger::WARN
        fallback.warn(
          "Rails Error: Unable to access log file. Please ensure that #{path} exists and is writable " \
          "(i.e. make it writable for user and group: chmod 0664 #{path}). " \
          "The log level has been raised to WARN and the output directed to STDERR until the problem is fixed."
        )
        fallback
      end
      if logger.is_a?(ActiveSupport::BroadcastLogger)
        if (broadcast_level = configured(config, :broadcast_log_level, nil))
          logger.level = ActiveSupport::Logger.const_get(broadcast_level.to_s.upcase)
        end
        logger
      else
        logger.level = ActiveSupport::Logger.const_get(configured(config, :log_level, :debug).to_s.upcase)
        ActiveSupport::BroadcastLogger.new(logger)
      end
    end

    # Application::Configuration#default_log_file.
    def default_log_file(config)
      require "fileutils"
      path = log_path(config)
      FileUtils.mkdir_p(File.dirname(path)) unless File.exist?(File.dirname(path))
      file = File.open(path, "a")
      file.binmode
      file.sync = configured(config, :autoflush_log, true) # if true make sure every write flushes
      file
    end

    # `config.paths["log"].first`: Application::Configuration's
    # `paths.add "log", with: "log/#{Rails.env}.log"`, expanded under the
    # root as Rails::Paths does (this tree's root is the directory it runs
    # in), so the logger's file name is absolute, as natively.
    def log_path(config)
      return config.paths["log"].first.to_s if config.respond_to?(:paths)

      File.expand_path(root.join("log", "#{env}.log").to_s)
    end

    def configured(config, key, default)
      config.respond_to?(key) ? config.public_send(key) : default
    end
  end

  class Configuration
    def initialize(application)
      @application = application
    end

    def respond_to_missing?(name, include_private = false)
      lifted?(name) || engine_option?(name) || railtie_option?(name) || framework_namespace?(name) || super
    end

    # The names Application::Configuration inherits from
    # Engine::Configuration (railties 8.2.0.alpha engine/configuration.rb),
    # the path lists and tables an application initializer reads and
    # prunes (`config.autoload_paths -= unwanted`, `config.paths["app/views"]
    # .expanded`). One Rails::Application::Configuration for the process,
    # rooted at the tree's root; where the tree carries no Engine port,
    # none of them.
    ENGINE_OPTIONS = %i[
      paths root autoload_paths autoload_once_paths eager_load_paths
      all_autoload_paths all_autoload_once_paths all_eager_load_paths
      route_set_class default_scope javascript_path
    ].freeze

    # The `config.<framework>` option namespaces each framework's railtie
    # body sets (`config.action_controller = ActiveSupport::
    # OrderedOptions.new`, actionpack's action_controller/railtie.rb and
    # the like), where an assignment is the definition and an unset
    # option reads as nil.
    FRAMEWORK_NAMESPACES = %w[
      action_controller action_dispatch action_view action_mailer action_mailbox action_cable
      action_text active_job active_model active_record active_storage active_support
    ].freeze

    def engine_config # :nodoc:
      @engine_config ||= ::Rails::Application::Configuration.new(Pathname.new(::Rails::Engine.application_root))
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
      elsif engine_option?(name)
        engine_config.public_send(name, *args, &block)
      elsif defined?(::Rails::Railtie::Configuration)
        if framework_namespace?(name) && !railtie_option?(name)
          framework_namespace(name)
        else
          ::Rails::Railtie::Configuration.new.public_send(name, *args, &block)
        end
      else
        super
      end
    end

    # `config.x`, Application::Configuration's `@x = Custom.new`
    # (railties 8.2.0.alpha application/configuration.rb): the app's own
    # namespace, where an assignment is the definition and every unset
    # name reads as a fresh ActiveSupport::OrderedOptions. Ingest lifts
    # each `config.x.a.b = v` the tree writes to `x_a_b` on the
    # application reopen (`config_receiver_path`), so the Custom here
    # answers those through the same readers and writers, and stores any
    # other name the way Rails' does.
    def x
      @x ||= Custom.new(@application, "x")
    end

    # Application::Configuration's `attr_accessor
    # :consider_all_requests_local`, `false` until written
    # (configuration.rb:56). Environments write it (Core's test.rb:
    # true); the environment program carries the write as written
    # (`initializers::RUNTIME_FRAMEWORK_CONFIG_KEYS`), and app code reads
    # it to decide whether to raise or report (Core's
    # QueryCapture::Capture, PlatformComponent's test slices).
    def consider_all_requests_local
      Configuration.settings.fetch(:consider_all_requests_local, false)
    end

    def consider_all_requests_local=(value)
      Configuration.settings[:consider_all_requests_local] = value
    end

    # Application::Configuration#session_store (railties 8.2.0.alpha
    # application/configuration.rb:604-622, Shopify/rails 5eb8c4c45bee):
    # with a name, records the store and its options; without, answers the
    # store class (`:disabled` is nil, a Symbol resolves under
    # ActionDispatch::Session as ActionDispatch::Session.resolve_store
    # does, anything else as given). `session_store?` is the recorded name.
    def session_store(new_session_store = nil, **options)
      if new_session_store
        Configuration.settings[:session_store] = new_session_store
        Configuration.settings[:session_options] = options || {}
      else
        case (store = Configuration.settings[:session_store])
        when :disabled
          nil
        when Symbol
          resolve_session_store(store)
        else
          store
        end
      end
    end

    def session_store? # :nodoc:
      Configuration.settings[:session_store]
    end

    # `config.session_options`, Application::Configuration's
    # `attr_accessor :session_options` (configuration.rb:21).
    def session_options
      Configuration.settings[:session_options]
    end

    def session_options=(value)
      Configuration.settings[:session_options] = value
    end

    # The Application::Configuration accessors this class answers
    # itself (above), one store for the process like the native
    # singleton's instance variables.
    def self.settings # :nodoc:
      @settings ||= {}
    end

    # Application::Configuration::Custom. A name the tree's lifted keys
    # cover reads and writes through the application reopen (`x_<name>`,
    # or a deeper Custom when lifted keys extend `x_<name>_`); any other
    # name is Rails' own store: a write sets it, a read of an unset name
    # creates and keeps an empty ActiveSupport::OrderedOptions.
    class Custom
      # `reads_nil`: the options a framework railtie's namespace holds
      # (`config.action_controller.asset_host`), where an unset name
      # reads as nil, not as a namespace of its own.
      def initialize(application, prefix, reads_nil: false)
        @application = application
        @prefix = prefix
        @reads_nil = reads_nil
      end

      def method_missing(method, *args)
        name = method.to_s
        if name.end_with?("=")
          key = "#{@prefix}_#{name.delete_suffix("=")}"
          return @application.public_send("#{key}=", args.first) if lifted?(key)

          Custom.store[key] = args.first
        else
          key = "#{@prefix}_#{name}"
          return @application.public_send(key) if lifted?(key)
          return Custom.store[key] if Custom.store.key?(key)
          return Custom.nested[key] ||= Custom.new(@application, key) if lifted_below?(key)
          return nil if @reads_nil

          Custom.store[key] = Custom.empty_options
        end
      end

      def respond_to_missing?(_symbol, _include_private = false)
        true
      end

      def self.store # :nodoc:
        @store ||= {}
      end

      def self.nested # :nodoc:
        @nested ||= {}
      end

      # The OrderedOptions port rails_railtie.rb requires; loaded here
      # when this tree carries no railtie (a missing file is a LoadError
      # naming it, not a stand-in).
      def self.empty_options # :nodoc:
        require_relative "active_support_ordered_options" unless defined?(::ActiveSupport::OrderedOptions)
        ::ActiveSupport::OrderedOptions.new
      end

      # Hash#deep_symbolize_keys (arrays walked too) and Hash#deep_merge,
      # for `config_for`.
      def self.deep_symbolize(value) # :nodoc:
        case value
        when Hash then value.each_with_object({}) { |(k, v), h| h[(k.to_sym rescue k)] = deep_symbolize(v) }
        when Array then value.map { |v| deep_symbolize(v) }
        else value
        end
      end

      def self.deep_merge(base, other) # :nodoc:
        base.merge(other) { |_, a, b| a.is_a?(Hash) && b.is_a?(Hash) ? deep_merge(a, b) : b }
      end

      private

      def lifted?(key)
        @application.respond_to?("#{key}__build")
      end

      def lifted_below?(key)
        prefix = "#{key}_"
        @application.class.instance_methods.any? { |m| (s = m.to_s).start_with?(prefix) && s.end_with?("__build") }
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

    def engine_option?(name)
      defined?(::Rails::Application::Configuration) && ENGINE_OPTIONS.include?(name.to_s.delete_suffix("=").to_sym)
    end

    def framework_namespace?(name)
      defined?(::Rails::Railtie::Configuration) && FRAMEWORK_NAMESPACES.include?(name.to_s)
    end

    def framework_namespace(name)
      Custom.nested[name.to_s] ||= Custom.new(@application, name.to_s, reads_nil: true)
    end

    # ActionDispatch::Session.resolve_store (actionpack 8.2.0.alpha
    # action_dispatch.rb:118), whose message it keeps.
    def resolve_session_store(session_store)
      ::ActionDispatch::Session.const_get(::ActiveSupport::Inflector.camelize(session_store.to_s))
    rescue NameError
      raise <<~ERROR
        Unable to resolve session store #{session_store.inspect}.

        #{session_store.inspect} resolves to ActionDispatch::Session::#{::ActiveSupport::Inflector.camelize(session_store.to_s)},
        but that class is undefined.

        Is #{session_store.inspect} spelled correctly, and are any necessary gems installed?
      ERROR
    end
  end
end
