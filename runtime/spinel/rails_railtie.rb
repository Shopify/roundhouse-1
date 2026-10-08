# frozen_string_literal: true

# `require "rails/railtie"` on the PostgreSQL CRuby tree: Rails::Railtie,
# Rails::Initializable and Rails::Railtie::Configuration of railties
# 8.2.0.alpha (Shopify/rails 5813559f, lib/rails/railtie.rb,
# initializable.rb, railtie/configuration.rb, gem_version.rb), ported
# over this tree's runtime. runtime/features/rails/railtie.rb answers the
# feature, so a gem's optional Rails integration (hashie 5.0.0's
# `require 'rails/railtie' if defined?(::Rails)`) defines its railtie
# against this class, never the railties checkout (whose files are off
# $LOAD_PATH, runtime/git_gems.rb).
#
# Rails runs every railtie's initializers inside `Application#initialize!`
# (config/environment.rb), between its Bootstrap and Finisher groups. The
# tree's boot.rb stands in for both groups; it calls
# `Rails::Railtie.run_initializers_of_railties(app)` before the app's
# code loads (`require_relative "app/models"`), which tsorts the railties'
# initializers among the Bootstrap and Finisher names as
# Rails::Initializable::Collection does and runs the `:default` group,
# with the Bootstrap and Finisher hooks a railtie can register on
# (`:before_initialize`, `to_prepare`, `:after_initialize`) at their names.
#
# Rails::Engine is runtime/rails_engine.rb. A railtie or engine the app
# carries compiled replays its class body statement by statement
# (`run_body_statement`), each statement named where it fails.
#
# Not ported, and refused where used: `config.app_middleware` operations (the tree has
# no middleware stack a gem's railtie can join; each recorded operation is
# error[gem_railtie_middleware_not_applied] when the railties run).
# `rake_tasks`/`console`/`runner`/`generators`/`server` blocks are
# recorded as Rails records them; Rails runs them only under those
# commands, never in a test process, and so does this tree.
require "tsort"
require_relative "active_support_ordered_options"
require_relative "rails"

module Rails
  # railties/lib/rails/gem_version.rb.
  module VERSION
    MAJOR = 8
    MINOR = 2
    TINY  = 0
    PRE   = "alpha"

    STRING = [MAJOR, MINOR, TINY, PRE].compact.join(".").freeze
  end

  def self.gem_version
    Gem::Version.new(VERSION::STRING)
  end

  def self.version
    VERSION::STRING
  end

  # railties rails.rb: `class << self; ... alias :app :application`.
  def self.app
    application
  end

  # Pathname#read (`Rails.root.join("REVISION").read`): File.read of the
  # path, raising Errno::ENOENT when it is absent. The shared runtime's
  # Rails.root is an AppPath, not a Pathname (rails.rb).
  AppPath.class_eval do
    def read(*args, **kwargs)
      File.read(@base, *args, **kwargs)
    end
  end

  # railties 8.2.0.alpha application.rb:396-424, `Application#revision`
  # and its internal writer. The runtime's `Rails.application` answers a
  # fresh Application per call (rails.rb), so the once-per-process memo
  # Rails keeps in the singleton's `@revision` / `@revision_initialized`
  # lives on the class instead. `root` is `Rails.root`, as the
  # application's config.root is.
  class << Application
    def revision_state # :nodoc:
      @revision_state ||= {}
    end
  end

  Application.class_eval do
    # Returns the application's revision (deployment identifier).
    # Set via config.revision (string), ENV["REVISION"], or REVISION file.
    # Always either a String or +nil+.
    def revision
      state = Rails::Application.revision_state
      unless state[:initialized]
        state[:revision] = begin
          ENV["REVISION"].presence ||
            Rails.root.join("REVISION").read.strip.presence
        rescue SystemCallError
          r, w = IO.pipe
          success = system("git", "-C", Rails.root.to_s, "rev-parse", "HEAD", in: File::NULL, err: File::NULL, out: w)
          w.close
          rev = r.read.strip
          r.close
          rev if success
        end
        state[:initialized] = true
      end
      state[:revision]
    end

    def revision=(rev) # :nodoc:
      state = Rails::Application.revision_state
      state[:revision] = rev&.to_s
      state[:initialized] = true
    end
  end

  module Initializable
    def self.included(base) # :nodoc:
      base.extend ClassMethods
    end

    class Initializer
      attr_reader :name, :block, :before, :after

      def initialize(name, context, before:, after:, group: nil, &block)
        @group = group || :default
        @name, @before, @after, @context, @block = name, before, after, context, block
      end

      def belongs_to?(group)
        @group == group || @group == :all
      end

      def run(*args)
        @context.instance_exec(*args, &block)
      end

      def bind(context)
        return self if @context
        Initializer.new(@name, context, before:, after:, group: @group, &block)
      end

      def context_class
        @context.class
      end
    end

    class Collection
      include Enumerable
      include TSort

      def initialize(initializers = nil)
        @order = Hash.new { |hash, key| hash[key] = Set.new }
        @resolve = Hash.new { |hash, key| hash[key] = Set.new }
        @collection = []
        concat(initializers) if initializers
      end

      # `delegate_missing_to :@collection`.
      def method_missing(name, ...)
        return super unless @collection.respond_to?(name)
        @collection.public_send(name, ...)
      end

      def respond_to_missing?(name, include_private = false)
        @collection.respond_to?(name) || super
      end

      def freeze
        return self if frozen?

        @order.default_proc = nil
        @resolve.default_proc = nil
        @order.freeze
        @resolve.freeze
        @collection.freeze
        super
      end

      def to_a
        @collection
      end

      def last
        @collection.last
      end

      def each(&block)
        @collection.each(&block)
      end

      alias :tsort_each_node :each
      def tsort_each_child(initializer, &block)
        @order[initializer.name].each do |name|
          @resolve[name].each(&block)
        end
      end

      def +(other)
        dup.concat(other.to_a)
      end

      def <<(initializer)
        @collection << initializer
        @order[initializer.before] << initializer.name if initializer.before
        @order[initializer.name] << initializer.after if initializer.after
        @resolve[initializer.name] << initializer
        self
      end

      def push(*initializers)
        initializers.each(&method(:<<))
        self
      end

      alias_method(:append, :push)

      def concat(*initializer_collections)
        initializer_collections.each do |initializers|
          initializers.each(&method(:<<))
        end
        self
      end

      def has?(name)
        @resolve.key?(name)
      end
    end

    def run_initializers(group = :default, *args)
      return if instance_variable_defined?(:@ran)
      initializers.tsort_each do |initializer|
        initializer.run(*args) if initializer.belongs_to?(group)
      end
      @ran = true
    end

    def initializers
      @initializers ||= self.class.initializers_for(self)
    end

    module ClassMethods
      def initializers
        @initializers ||= Collection.new
      end

      def initializers_chain
        initializers = Collection.new
        ancestors.reverse_each do |klass|
          next unless klass.respond_to?(:initializers)
          initializers = initializers + klass.initializers
        end
        initializers
      end

      def initializers_for(binding)
        Collection.new(initializers_chain.map { |i| i.bind(binding) })
      end

      def initializer(name, opts = {}, &blk)
        raise ArgumentError, "A block must be passed when defining an initializer" unless blk
        opts[:after] ||= initializers.last&.name unless initializers.has?(opts[:before])
        initializers << Initializer.new(
          name, nil, before: opts[:before], after: opts[:after], group: opts[:group], &blk
        )
      end
    end
  end

  class Railtie
    include Initializable

    ABSTRACT_RAILTIES = %w(Rails::Railtie Rails::Engine Rails::Application).freeze

    # Rails::Application::Bootstrap's and Finisher's initializer names,
    # in their order (railties 8.2.0.alpha application/bootstrap.rb and
    # finisher.rb). The tree's boot has done the work of each by the
    # time the railties run, so they run nothing here; they are in the
    # collection so a railtie's `before:`/`after:` naming one orders as
    # in Rails.
    BOOTSTRAP_INITIALIZERS = %i[
      load_environment_hook load_active_support set_eager_load initialize_logger
      initialize_error_reporter initialize_event_reporter initialize_cache
      setup_once_autoloader bootstrap_hook
    ].freeze
    FINISHER_INITIALIZERS = %i[
      add_generator_templates setup_main_autoloader setup_default_session_store
      build_middleware_stack define_main_app_helper add_to_prepare_blocks
      run_prepare_callbacks eager_load! finisher_hook configure_executor_for_concurrency
      add_internal_routes set_routes_reloader_hook set_clear_dependencies_hook enable_yjit
    ].freeze

    @@not_run = 0

    class << self
      private :new

      # `delegate :config, to: :instance`.
      def config(...)
        instance.config(...)
      end

      # ActiveSupport::DescendantsTracker on Ruby >= 3.1 is Class#subclasses.
      def subclasses
        super.reject(&:abstract_railtie?).sort
      end

      def rake_tasks(&blk)
        register_block_for(:rake_tasks, &blk)
      end

      def console(&blk)
        register_block_for(:load_console, &blk)
      end

      def runner(&blk)
        register_block_for(:runner, &blk)
      end

      def generators(&blk)
        register_block_for(:generators, &blk)
      end

      def server(&blk)
        register_block_for(:server, &blk)
      end

      def abstract_railtie?
        ABSTRACT_RAILTIES.include?(name)
      end

      def railtie_name(name = nil)
        @railtie_name = name.to_s if name
        @railtie_name ||= generate_railtie_name(self.name)
      end

      def instance
        @instance ||= new
      end

      def configure(&block)
        instance.configure(&block)
      end

      def <=>(other) # :nodoc:
        load_index <=> other.load_index
      end

      def inherited(subclass)
        subclass.increment_load_index
        super
      end

      # Application#initialize!'s railtie part: Bootstrap's names, every
      # railtie's initializers in load order, Finisher's names, tsorted
      # and run for the `:default` group with the application as the
      # argument (Rails::Initializable#run_initializers). Of the Bootstrap
      # and Finisher work, the part a railtie registers into runs at its
      # Rails name: `:bootstrap_hook` runs the `:before_initialize` load
      # hooks, `:run_prepare_callbacks` the `to_prepare` blocks and
      # `:finisher_hook` the `:after_initialize` load hooks, and
      # `:build_middleware_stack` names each `app_middleware` operation.
      #
      # In two phases on the emitted boot: `:boot` runs the sorted list up
      # to Finisher's first name, before the app's code loads; `:finish`
      # runs the rest after the app's code and its initializers, as Rails
      # runs the Finisher after `load_config_initializers` and eager
      # loading (an after_initialize hook reads what an app initializer
      # set: bugsnag's reads `config.filter_parameters`). `:all` (the
      # default) runs both. Each phase runs once.
      def run_initializers_of_railties(app, phase = :all)
        @@ran_phases ||= []
        phases = (phase == :all ? [:boot, :finish] : [phase]) - @@ran_phases
        return if phases.empty?
        @@ran_phases.concat(phases)
        @@sorted ||= sorted_initializers_of_railties(app)
        finisher = @@sorted.index { |initializer| initializer.name == FINISHER_INITIALIZERS.first }
        part = { boot: @@sorted[0...finisher], finish: @@sorted[finisher..] }
        phases.each do |name|
          part.fetch(name).each do |initializer|
            next unless initializer.belongs_to?(:default)
            if initializer.context_class == app.class
              initializer.run(app)
            else
              not_run_unless_it_runs("#{initializer.context_class.name} initializer #{initializer.name.inspect}") { initializer.run(app) }
            end
          end
        end
        return unless phases.include?(:finish)
        $stderr.puts "roundhouse: #{@@not_run} railtie and engine initializer(s), hook(s) and body statement(s) did not run (error[gem_railtie_not_run], error[railtie_body_statement_not_run])" if @@not_run > 0
      end

      def sorted_initializers_of_railties(app) # :nodoc:
        work = {
          bootstrap_hook: -> { ActiveSupport.run_load_hooks(:before_initialize, app) },
          build_middleware_stack: -> { Configuration.refuse_app_middleware! },
          run_prepare_callbacks: -> { Configuration.new.to_prepare_blocks.each(&:call) },
          eager_load!: -> { eager_load_step(app) },
          finisher_hook: -> { ActiveSupport.run_load_hooks(:after_initialize, app) },
        }
        collection = Initializable::Collection.new
        add = lambda do |names|
          previous = nil
          names.each do |name|
            step = work[name]
            collection << Initializable::Initializer.new(name, app, before: nil, after: previous, group: :all) { step&.call }
            previous = name
          end
        end
        add.(BOOTSTRAP_INITIALIZERS)
        # Rails::Engine::Railties#_all: the railties, then the engines.
        subclasses.each { |railtie| collection.concat(railtie.instance.initializers) }
        if defined?(::Rails::Engine)
          ::Rails::Engine.subclasses.each { |engine| collection.concat(engine.instance.initializers) }
          # The application is itself an engine, the last in
          # Engine::Railties order (Application#ordered_railties).
          collection.concat(::Rails::Engine.application_initializers(app))
        end
        add.(FINISHER_INITIALIZERS)
        collection.tsort
      end

      # Finisher's `:eager_load!` (railties application/finisher.rb):
      # when the environment eager loads, the `:before_eager_load` hooks
      # (I18n::Railtie initializes I18n there), then the app's own code
      # (`Zeitwerk::Loader.eager_load_all`; the emitted autoloader is
      # app/models.rb's, and the app's initializers have registered their
      # Zeitwerk `on_load` callbacks by now, runtime/rails_autoloaders.rb),
      # then every `config.eager_load_namespaces` member's `eager_load!`
      # (I18n, each engine, a gem's: phonelib loads its phone data). The
      # reloader part runs only under `config.reloading_enabled?`, false
      # in this tree.
      def eager_load_step(app) # :nodoc:
        return unless defined?(::RoundhouseAutoload) && ::RoundhouseAutoload.eager_load?

        ActiveSupport.run_load_hooks(:before_eager_load, app)
        ::RoundhouseAutoload.eager_load!
        Configuration.eager_load_namespaces.each(&:eager_load!)
      end

      # A railtie's initializer or hook that needs Rails this tree does
      # not port (ActionController's internals, Rails::Engine, app config
      # the runtime does not answer) does not run: boot names it,
      # error[gem_railtie_not_run], and goes on, as the Bundler.require
      # list names a gem it cannot load.
      def not_run_unless_it_runs(what) # :nodoc:
        yield
      rescue StandardError, LoadError, NotImplementedError => e
        @@not_run += 1
        site = e.backtrace&.find { |line| !line.include?(__FILE__) && !line.include?("active_support_lazy_load_hooks") }
        $stderr.puts "roundhouse: error[gem_railtie_not_run]: #{what} did not run: #{e.class}: #{e.message.lines.first&.chomp}#{compiled_copy_note(e)} (#{site})"
        nil
      end

      # One statement of a railtie's or engine's class body the app
      # carries compiled (emitted under app/models/), run where the
      # class body has it. Rails runs the body as the class loads, and a
      # failure there stops the boot; here a statement that needs what
      # this tree does not port (a gem's railtie that is not loaded, so
      # `config.<gem>` is not there; a constant no carried file defines)
      # is named, error[railtie_body_statement_not_run], and the body
      # goes on, as `not_run_unless_it_runs` does for initializers.
      def run_body_statement(site) # :nodoc:
        yield
      rescue StandardError, LoadError, NotImplementedError => e
        @@not_run += 1
        $stderr.puts "roundhouse: error[railtie_body_statement_not_run]: #{name} body statement at #{site} did not run: #{e.class}: #{e.message.lines.first&.chomp}#{compiled_copy_note(e)}"
        nil
      end

      # A LoadError for a file of one of the app's own gems: this tree
      # carries the gem compiled, and the gem home's copy is registered
      # as loaded but not loaded (runtime/gem_facades.rb,
      # RH_COMPILED_GEM_COPIES), so a native gem cannot load it over the
      # compiled one.
      def compiled_copy_note(e) # :nodoc:
        return "" unless e.is_a?(LoadError) && e.path && defined?(::RH_COMPILED_GEM_COPIES)
        spec = Gem::Specification.find_by_path(e.path)
        return "" unless spec && ::RH_COMPILED_GEM_COPIES.key?(spec.name)
        " (error[compiled_gem_native_copy]: #{e.path.inspect} is a file of the #{spec.name} gem this tree carries compiled; the gem home's #{spec.name}-#{spec.version} is not loaded over it)"
      end

      protected
        attr_reader :load_index

        def increment_load_index
          @@load_counter ||= 0
          @load_index = (@@load_counter += 1)
        end

      private
        # ActiveSupport::Inflector (runtime/active_support_inflector.rb,
        # which runtime/gem_facades.rb loads before any gem; boot.rb
        # replays the app's acronyms into it before the app's code).
        def generate_railtie_name(string)
          ActiveSupport::Inflector.underscore(string).tr("/", "_")
        end

        def respond_to_missing?(name, _)
          return super if abstract_railtie?

          instance.respond_to?(name) || super
        end

        def method_missing(name, ...)
          if !abstract_railtie? && instance.respond_to?(name)
            instance.public_send(name, ...)
          else
            super
          end
        end

        def register_block_for(type, &blk)
          var_name = "@#{type}"
          blocks = instance_variable_defined?(var_name) ? instance_variable_get(var_name) : instance_variable_set(var_name, [])
          blocks << blk if blk
          blocks
        end
    end

    def railtie_name
      self.class.railtie_name
    end

    def initialize # :nodoc:
      if self.class.abstract_railtie?
        raise "#{self.class.name} is abstract, you cannot instantiate it directly."
      end
    end

    def configure(&block) # :nodoc:
      instance_eval(&block)
    end

    def config
      @config ||= Railtie::Configuration.new
    end

    def railtie_namespace # :nodoc:
      @railtie_namespace ||= module_parents_of(self.class).detect { |n| n.respond_to?(:railtie_namespace) }
    end

    # Module#module_parents (core_ext/module/introspection.rb), which
    # railtie_namespace reads: the enclosing modules by name, innermost
    # first, then Object.
    private def module_parents_of(mod) # :nodoc:
      parents = []
      parent_name = mod.name =~ /::[^:]+\z/ ? -$` : nil
      if parent_name
        parts = parent_name.split("::")
        until parts.empty?
          parents << Object.const_get(parts * "::")
          parts.pop
        end
      end
      parents << Object unless parents.include? Object
      parents
    end

    def inspect
      "#<#{self.class.name}>"
    end

    class Configuration
      def initialize
        @@options ||= {}
      end

      def self.eager_load_namespaces # :nodoc:
        @@eager_load_namespaces ||= []
      end

      def eager_load_namespaces
        @@eager_load_namespaces ||= []
      end

      def watchable_files
        @@watchable_files ||= []
      end

      def watchable_dirs
        @@watchable_dirs ||= {}
      end

      # Rails::Configuration::MiddlewareStackProxy records the operations
      # and merges them into the application's stack. This tree has no
      # stack for them; boot names each one (`refuse_app_middleware!`).
      # Values set on app_generators become the application's generator
      # defaults (tailwindcss-rails 4.1.0's engine: `config.app_generators
      # { |g| g.template_engine :tailwindcss }`); recorded, as natively,
      # for generator commands this tree never runs.
      def app_generators
        @@app_generators ||= Rails::Configuration::Generators.new
        yield(@@app_generators) if block_given?
        @@app_generators
      end

      def app_middleware
        @@app_middleware ||= MiddlewareStackProxy.new
      end

      def self.refuse_app_middleware! # :nodoc:
        return unless defined?(@@app_middleware)
        @@app_middleware.operations.each do |operation, args|
          $stderr.puts "roundhouse: error[gem_railtie_middleware_not_applied]: config.app_middleware.#{operation}(#{args.map(&:inspect).join(", ")}) from a gem's railtie is not applied: the tree has no middleware stack a railtie joins"
        end
      end

      def before_configuration(&block)
        ActiveSupport.on_load(:before_configuration, yield: true, &Configuration.guarded("before_configuration", block))
      end

      def before_eager_load(&block)
        ActiveSupport.on_load(:before_eager_load, yield: true, &Configuration.guarded("before_eager_load", block))
      end

      def before_initialize(&block)
        ActiveSupport.on_load(:before_initialize, yield: true, &Configuration.guarded("before_initialize", block))
      end

      def after_initialize(&block)
        ActiveSupport.on_load(:after_initialize, yield: true, &Configuration.guarded("after_initialize", block))
      end

      def after_routes_loaded(&block)
        ActiveSupport.on_load(:after_routes_loaded, yield: true, &Configuration.guarded("after_routes_loaded", block))
      end

      def to_prepare_blocks
        @@to_prepare_blocks ||= []
      end

      def to_prepare(&blk)
        to_prepare_blocks << Configuration.guarded("to_prepare", blk) if blk
      end

      # The hook a railtie registers, run under
      # `Railtie.not_run_unless_it_runs` (error[gem_railtie_not_run]).
      def self.guarded(kind, block) # :nodoc:
        return block if block.nil?
        site = block.source_location&.join(":")
        proc { |*args| Railtie.not_run_unless_it_runs("config.#{kind} hook (#{site})") { block.call(*args) } }
      end

      def respond_to?(name, include_private = false)
        super || @@options.key?(name.to_sym)
      end

      private
        def actual_method?(key)
          !@@options.key?(key) && respond_to?(key)
        end

        def method_missing(name, *args, &blk)
          if name.end_with?("=")
            key = name[0..-2].to_sym
            if actual_method?(key)
              raise NoMethodError.new("Cannot assign to `#{key}`, it is a configuration method")
            end

            @@options[key] = args.first
          elsif @@options.key?(name)
            @@options[name]
          else
            super
          end
        end

      # railties/lib/rails/configuration.rb's MiddlewareStackProxy: the
      # recording half.
      class MiddlewareStackProxy
        attr_reader :operations

        def initialize
          @operations = []
        end

        %i[insert_before insert insert_after swap use delete move_before move move_after unshift].each do |operation|
          define_method(operation) do |*args, &block|
            @operations << [operation, args]
            self
          end
        end
      end
    end
  end
end

module Rails
  # railties/lib/rails/configuration.rb's Generators (8.2.0.alpha),
  # verbatim: what `config.app_generators` and a railtie's
  # `config.generators` record. Rails::Configuration is
  # runtime/rails_configuration.rb's class here, a module natively.
  class Configuration
    class Generators # :nodoc:
      attr_accessor :aliases, :options, :templates, :fallbacks, :colorize_logging, :api_only
      attr_reader :hidden_namespaces, :after_generate_callbacks

      def initialize
        @aliases = Hash.new { |h, k| h[k] = {} }
        @options = Hash.new { |h, k| h[k] = {} }
        @fallbacks = {}
        @templates = []
        @colorize_logging = true
        @api_only = false
        @hidden_namespaces = []
        @after_generate_callbacks = []
      end

      def freeze
        return self if frozen?

        @aliases.default_proc = nil
        @options.default_proc = nil
        @aliases.freeze
        @options.freeze
        @fallbacks.freeze
        @templates.freeze
        @hidden_namespaces.freeze
        @after_generate_callbacks.freeze
        super
      end

      def initialize_copy(source)
        @aliases = @aliases.deep_dup
        @aliases.default_proc = proc { |h, k| h[k] = {} }
        @options = @options.deep_dup
        @options.default_proc = proc { |h, k| h[k] = {} }
        @fallbacks = @fallbacks.deep_dup
        @templates = @templates.dup
      end

      def hide_namespace(namespace)
        @hidden_namespaces << namespace
      end

      def after_generate(&block)
        @after_generate_callbacks << block
      end

      def apply_rubocop_autocorrect_after_generate!
        after_generate do |files|
          parsable_files = files.filter { |file| File.exist?(file) && file.end_with?(".rb") }
          unless parsable_files.empty?
            system(RbConfig.ruby, "bin/rubocop", "-A", "--fail-level=E", "--format=quiet", *parsable_files, exception: true)
          end
        end
      end

      def method_missing(method, *args)
        method = method.name.delete_suffix("=").to_sym

        if args.empty?
          if method == :rails
            return @options[method]
          else
            return @options[:rails][method]
          end
        end

        if method == :rails || args.first.is_a?(Hash)
          namespace, configuration = method, args.shift
        else
          namespace, configuration = args.shift, args.shift
          namespace = namespace.to_sym if namespace.respond_to?(:to_sym)
          @options[:rails][method] = namespace
        end

        if configuration
          aliases = configuration.delete(:aliases)
          @aliases[namespace].merge!(aliases) if aliases
          @options[namespace].merge!(configuration)
        end
      end
    end
  end
end
