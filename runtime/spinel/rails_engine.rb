# frozen_string_literal: true

# `require "rails/engine"` on the PostgreSQL CRuby tree: Rails::Engine,
# Rails::Engine::Configuration and Rails::Paths of railties 8.2.0.alpha
# (Shopify/rails 5813559f, lib/rails/engine.rb, engine/configuration.rb,
# paths.rb), ported over this tree's Rails::Railtie
# (runtime/rails_railtie.rb). runtime/features/rails/engine.rb answers
# the feature.
#
# An app's in-repo engines are carried compiled: each `class X <
# Rails::Engine` is an emitted file under app/models/, its class body
# replayed statement by statement (`Railtie.run_body_statement`), and
# app/models.rb loads them at boot in Rails' load order, after
# registering each one's root (`Engine.source_roots`), the engine's
# directory in this tree, where its data files are carried. Boot then
# runs every railtie's and engine's initializers where
# Application#initialize! runs them (`Railtie.run_initializers_of_railties`).
#
# The engine initializers Rails defines run here as Rails' do, against
# this tree: `set_load_path` puts the engine's existing load-path
# directories on $LOAD_PATH; `load_environment_config` requires the
# engine's existing config/environments file. Those whose work the
# compiler has already done for the tree run nothing:
# `set_autoload_paths` and `set_eager_load_paths` (app/models.rb's
# autoload covers every carried file), `load_config_initializers` (each
# engine's config/initializers are compiled into the boot, in engine
# order), `add_routing_paths` and `make_routes_lazy` (config/routes.rb is
# compiled). The rest are refused when the engine has what they would
# apply (error[engine_initializer_not_applied]): `add_locales`,
# `add_view_paths`, `add_mailer_preview_paths`, `add_fixture_paths`,
# `prepend_helpers_path`. Not ported: the engine as a Rack endpoint
# (`app`, `call`, `routes`, `helpers`, `load_seed`), and generators,
# console, runner and tasks loading.
require "pathname"
require_relative "rails_railtie"

module Rails
  # railties/lib/rails/paths.rb.
  module Paths
    class Root
      attr_accessor :path

      def initialize(path)
        @path = path
        @root = {}
      end

      def []=(path, value)
        glob = self[path] ? self[path].glob : nil
        add(path, with: value, glob: glob)
      end

      def add(path, options = {})
        with = Array(options.fetch(:with, path))
        @root[path] = Path.new(self, path, with, options)
      end

      def [](path)
        @root[path]
      end

      def values
        @root.values
      end

      def keys
        @root.keys
      end

      def values_at(*list)
        @root.values_at(*list)
      end

      def all_paths
        values.tap(&:uniq!)
      end

      def autoload_once
        filter_by(&:autoload_once?)
      end

      def eager_load
        filter_by(&:eager_load?)
      end

      def autoload_paths
        filter_by(&:autoload?)
      end

      def load_paths
        filter_by(&:load_path?)
      end

      private
        def filter_by(&block)
          all_paths.find_all(&block).flat_map { |path|
            paths = path.existent_directories
            paths - path.children.flat_map { |p| yield(p) ? [] : p.existent_directories }
          }.uniq
        end
    end

    class Path
      include Enumerable

      attr_accessor :glob

      def initialize(root, current, paths, options = {})
        @paths   = paths
        @current = current
        @root    = root
        @glob    = options[:glob]
        @exclude = options[:exclude]

        options[:autoload_once] ? autoload_once! : skip_autoload_once!
        options[:eager_load]    ? eager_load!    : skip_eager_load!
        options[:autoload]      ? autoload!      : skip_autoload!
        options[:load_path]     ? load_path!     : skip_load_path!
      end

      def absolute_current # :nodoc:
        File.expand_path(@current, @root.path)
      end

      def children
        keys = @root.keys.find_all { |k|
          k.start_with?(@current) && k != @current
        }
        @root.values_at(*keys.sort)
      end

      def first
        expanded.first
      end

      def last
        expanded.last
      end

      %w(autoload_once eager_load autoload load_path).each do |m|
        define_method(:"#{m}!") { instance_variable_set(:"@#{m}", true) }
        define_method(:"skip_#{m}!") { instance_variable_set(:"@#{m}", false) }
        define_method(:"#{m}?") { instance_variable_get(:"@#{m}") }
      end

      def each(&block)
        @paths.each(&block)
      end

      def <<(path)
        @paths << path
      end
      alias :push :<<

      def concat(paths)
        @paths.concat paths
      end

      def unshift(*paths)
        @paths.unshift(*paths)
      end

      def to_ary
        @paths
      end

      def paths
        raise "You need to set a path root" unless @root.path

        map do |p|
          Pathname.new(@root.path).join(p)
        end
      end

      def extensions # :nodoc:
        $1.split(",") if @glob =~ /\{([\S]+)\}/
      end

      def expanded
        raise "You need to set a path root" unless @root.path
        result = []

        each do |path|
          path = File.expand_path(path, @root.path)

          if @glob && File.directory?(path)
            result.concat files_in(path)
          else
            result << path
          end
        end

        result.uniq!
        result
      end

      def existent
        expanded.select do |f|
          does_exist = File.exist?(f)

          if !does_exist && File.symlink?(f)
            raise "File #{f.inspect} is a symlink that does not point to a valid file"
          end
          does_exist
        end
      end

      def existent_directories
        expanded.select { |d| File.directory?(d) }
      end

      def expanded_files
        expanded.select { |file| !File.extname(file).empty? }
      end

      alias to_a expanded

      private
        def files_in(path)
          files = Dir.glob(@glob, base: path, sort: false)
          files -= @exclude if @exclude
          files.map! { |file| File.join(path, file) }
          files.sort
        end
    end
  end

  class Engine < Railtie
    class << self
      attr_accessor :called_from, :isolated

      alias :isolated? :isolated

      def engine_name(name = nil)
        railtie_name(name)
      end

      # `delegate :eager_load!, to: :instance`.
      def eager_load!(...)
        instance.eager_load!(...)
      end

      # The tree's own engines (app/models.rb): class name => the
      # engine's root, its directory in this tree. Rails finds the root
      # from the file that defines the class (`called_from`, then the
      # nearest directory with a `lib`); the class here is defined by an
      # emitted file under app/models/, so the compiler, which read the
      # engine where the source has it, names the root instead.
      def source_roots
        @@source_roots ||= {}
      end

      def inherited(base)
        unless base.abstract_railtie?
          Rails::Railtie::Configuration.eager_load_namespaces << base

          base.called_from = source_roots[base.name] || begin
            call_stack = caller_locations.map { |l| l.absolute_path || l.path }

            File.dirname(call_stack.detect { |p| !p.match?(%r[railties[\w.-]*/lib/rails|rack[\w.-]*/lib/rack]) && p != __FILE__ })
          end
        end

        super
      end

      def find_root(from)
        return Pathname.new(File.expand_path(from)) if source_roots.value?(from)

        find_root_with_flag "lib", from
      end

      def endpoint(endpoint = nil)
        @endpoint ||= nil
        @endpoint = endpoint if endpoint
        @endpoint
      end

      def isolate_namespace(mod)
        engine_name(generate_railtie_name(mod.name))

        # ActiveSupport::Inflector.underscore without the acronym table,
        # as `generate_railtie_name` (the tree has no Inflector acronyms).
        config.default_scope = { module: mod.name.gsub("::", "/").gsub(/(?<=[A-Z])(?=[A-Z][a-z])|(?<=[a-z\d])(?=[A-Z])/, "_").tr("-", "_").downcase }

        self.isolated = true

        unless mod.respond_to?(:railtie_namespace)
          name, railtie = engine_name, self

          mod.singleton_class.instance_eval do
            define_method(:railtie_namespace) { railtie }

            unless mod.respond_to?(:table_name_prefix)
              define_method(:table_name_prefix) { "#{name}_" }

              ActiveSupport.on_load(:active_record) do
                mod.singleton_class.redefine_method(:table_name_prefix) do
                  "#{ActiveRecord::Base.table_name_prefix}#{name}_"
                end
              end
            end

            unless mod.respond_to?(:use_relative_model_naming?)
              define_method(:use_relative_model_naming?) { true }
            end

            unless mod.respond_to?(:railtie_helpers_paths)
              define_method(:railtie_helpers_paths) { railtie.helpers_paths }
            end
          end
        end
      end

      # Finds engine with given path.
      def find(path)
        expanded_path = File.expand_path path
        Rails::Engine.subclasses.each do |klass|
          engine = klass.instance
          return engine if File.expand_path(engine.root) == expanded_path
        end
        nil
      end

      def find_root_with_flag(flag, root_path, default = nil) # :nodoc:
        while root_path && File.directory?(root_path) && !File.exist?("#{root_path}/#{flag}")
          parent = File.dirname(root_path)
          root_path = parent != root_path && parent
        end

        root = File.exist?("#{root_path}/#{flag}") ? root_path : default
        raise "Could not find root path for #{self}" unless root

        Pathname.new File.realpath root
      end
    end

    # `delegate :middleware, :root, :paths, to: :config`.
    def middleware
      config.middleware
    end

    def root
      config.root
    end

    def paths
      config.paths
    end

    # `delegate :engine_name, :isolated?, to: :class`.
    def engine_name
      self.class.engine_name
    end

    def isolated?
      self.class.isolated?
    end

    def initialize
      @_all_autoload_paths = nil
      @_all_load_paths     = nil
      @config              = nil
      super
    end

    def eager_load!
      # Already done by Zeitwerk::Loader.eager_load_all. By now, we leave the
      # method as a no-op for backwards compatibility.
    end

    # Returns all registered helpers paths.
    def helpers_paths
      paths["app/helpers"].existent
    end

    # Define the configuration object for the engine.
    def config
      @config ||= Engine::Configuration.new(self.class.find_root(self.class.called_from))
    end

    initializer :load_environment_config, before: :load_environment_hook, group: :all do
      paths["config/environments"].existent.each do |environment|
        require environment
      end
    end

    initializer :set_load_path, before: :bootstrap_hook do |app|
      _all_load_paths(false).reverse_each do |path|
        $LOAD_PATH.unshift(path) if File.directory?(path)
      end
      $LOAD_PATH.uniq!
    end

    # app/models.rb's autoload, generated from every carried file, is
    # this tree's ActiveSupport::Dependencies.
    initializer :set_autoload_paths, before: :bootstrap_hook do
      config.autoload_paths.freeze
      config.autoload_once_paths.freeze
    end

    initializer :set_eager_load_paths, before: :bootstrap_hook do
      config.eager_load_paths.freeze
    end

    # config/routes.rb is compiled into the tree's route table.
    initializer :make_routes_lazy, before: :bootstrap_hook do |app|
    end

    initializer :add_routing_paths do |app|
    end

    initializer :add_locales do
      not_applied(:add_locales, paths["config/locales"].existent)
    end

    initializer :add_view_paths do
      not_applied(:add_view_paths, paths["app/views"].existent)
    end

    initializer :add_mailer_preview_paths do
      not_applied(:add_mailer_preview_paths, paths["test/mailers/previews"].existent)
    end

    initializer :add_fixture_paths do
      fixtures = config.root.join("test", "fixtures")
      not_applied(:add_fixture_paths, fixtures.exist? ? [fixtures.to_s] : [])
    end

    initializer :prepend_helpers_path do |app|
      not_applied(:prepend_helpers_path, paths["app/helpers"].existent) if !isolated? || (app == self)
    end

    # Each engine's config/initializers are compiled into the boot, in
    # engine order (ingest::initializers).
    initializer :load_config_initializers do
    end

    initializer :engines_blank_point do
      # We need this initializer so all extra initializers added in engines are
      # consistently executed after all the initializers above across all engines.
    end

    private
      # What an engine initializer would apply and this tree has no
      # place for: named, by engine and path, when there is any.
      def not_applied(initializer, found)
        return if found.empty?

        Railtie.not_run_unless_it_runs("#{self.class.name} initializer #{initializer.inspect}") do
          raise NotImplementedError, "error[engine_initializer_not_applied]: the tree has no #{initializer.to_s.delete_prefix("add_").delete_prefix("prepend_")} to add #{found.map { |f| f.to_s.delete_prefix("#{root}/") }.join(", ")} to"
        end
      end

      def _all_autoload_once_paths
        config.all_autoload_once_paths.uniq
      end

      def _all_autoload_paths
        @_all_autoload_paths ||= begin
          autoload_paths  = config.all_autoload_paths
          autoload_paths += config.all_eager_load_paths
          autoload_paths -= config.all_autoload_once_paths
          autoload_paths.uniq
        end
      end

      def _all_load_paths(add_autoload_paths_to_load_path)
        @_all_load_paths ||= begin
          load_paths = config.paths.load_paths
          if add_autoload_paths_to_load_path
            load_paths += _all_autoload_paths
            load_paths += _all_autoload_once_paths
          end
          load_paths.uniq
        end
      end

    # railties/lib/rails/engine/configuration.rb.
    class Configuration < ::Rails::Railtie::Configuration
      attr_reader :root
      attr_accessor :middleware, :javascript_path, :route_set_class, :default_scope
      attr_writer :eager_load_paths, :autoload_once_paths, :autoload_paths
      attr_reader :autoload_paths, :autoload_once_paths, :eager_load_paths

      def initialize(root = nil)
        super()
        @root = root
        @middleware = Railtie::Configuration::MiddlewareStackProxy.new
        @javascript_path = "javascript"
        @route_set_class = nil
        @default_scope = nil
        @autoload_paths = []
        @autoload_once_paths = []
        @eager_load_paths = []
      end

      def paths
        @paths ||= begin
          paths = Rails::Paths::Root.new(@root)

          paths.add "app",                 eager_load: true,
                                           glob: "{*,*/concerns}",
                                           exclude: ["assets", javascript_path]
          paths.add "app/assets",          glob: "*"
          paths.add "app/controllers",     eager_load: true
          paths.add "app/channels",        eager_load: true
          paths.add "app/helpers",         eager_load: true
          paths.add "app/models",          eager_load: true
          paths.add "app/mailers",         eager_load: true
          paths.add "app/views"

          paths.add "lib",                 load_path: true
          paths.add "lib/assets",          glob: "*"
          paths.add "lib/tasks",           glob: "**/*.rake"

          paths.add "config"
          paths.add "config/environments", glob: -"#{Rails.env}.rb"
          paths.add "config/initializers", glob: "**/*.rb"
          paths.add "config/locales",      glob: "**/*.{rb,yml}"
          paths.add "config/routes.rb"
          paths.add "config/routes",       glob: "**/*.rb"

          paths.add "db"
          paths.add "db/migrate"
          paths.add "db/seeds.rb"

          paths.add "vendor",              load_path: true
          paths.add "vendor/assets",       glob: "*"

          paths.add "test/mailers/previews", autoload: true

          paths
        end
      end

      def root=(value)
        @root = paths.path = Pathname.new(value).expand_path
      end

      def all_autoload_paths # :nodoc:
        autoload_paths + paths.autoload_paths
      end

      def all_autoload_once_paths # :nodoc:
        autoload_once_paths + paths.autoload_once
      end

      def all_eager_load_paths # :nodoc:
        eager_load_paths + paths.eager_load
      end
    end
  end
end
