# frozen_string_literal: true
# `Rails.autoloaders` — railties 8.1.4 autoloaders.rb — over the emitted
# tree's compiled autoloader (`RoundhouseAutoload`, app/models.rb), with
# Zeitwerk's `on_load(cpath)` callbacks (zeitwerk 2.8.2, the version Core
# locks).
#
# CRuby/JRuby only, pushed at the ruby-family fork in `project.rs` beside
# rails_configuration.rb: the callbacks are blocks kept per constant path
# and the firing reads constants by computed name, which the strict
# targets cannot compile. Where this file is absent, `check` refuses each
# `Rails.autoloaders` read as a runtime constant (`analyze::runtime_reads`).
#
# TIMING. Zeitwerk runs `on_load("Cpath")`'s callbacks right after the
# file that defines Cpath is loaded through its loader (loader/
# callbacks.rb:9 `on_file_autoloaded`: `run_on_load_callbacks(cref.path,
# cref.get, file)`, from the autoload or from its decorated
# `Kernel#require`), once, in registration order, with the constant's
# value and the file. A callback registered for a constant already loaded
# is kept and never called (its autoload is gone). The emitted tree has
# no Zeitwerk: each constant one of the app's literal `on_load("Cpath")`
# names gets, at the end of the emitted file whose autoload it is, a
# `RoundhouseAutoload.autoloaded("Cpath", __FILE__)` (apply_models_autoload),
# which runs the callbacks here. `abspath` is that emitted file.
#
# WHAT IS NOT PORTED, BY NAME. `on_load` with no constant path (every
# constant's load) would need every emitted file to report its load: it
# raises NotImplementedError. A callback for a constant no emitted file
# reports (no single file autoloads it) is named at registration, since
# nothing will ever call it. Unloading and reloading (`on_unload`,
# `reload`) are not modeled: the emitted tree loads each file once, as
# Zeitwerk does with reloading disabled (Core's test environment).
require "set"
require_relative "rails"

module Rails
  def self.autoloaders
    @autoloaders ||= Autoloaders.new
  end

  class Autoloaders
    include Enumerable

    attr_reader :main, :once

    def initialize
      @main = Loader.new("rails.main")
      @once = Loader.new("rails.once")
    end

    def each
      yield main
      yield once
    end

    def zeitwerk_enabled?
      true
    end

    # Whether `cpath` is loaded: every segment defined and none a pending
    # autoload.
    def self.loaded?(cpath)
      mod = Object
      cpath.split("::").each do |name|
        return false unless mod.is_a?(Module) && mod.const_defined?(name, false) && !mod.autoload?(name, false)
        mod = mod.const_get(name, false)
      end
      true
    end

    # Zeitwerk's ConstantPathValidator: a NameError for a string that is
    # no constant path.
    CNAME_VALIDATOR = Module.new

    class Loader
      UNDEFINED = Object.new.freeze

      attr_reader :tag

      def initialize(tag)
        @tag = tag
        @on_load_callbacks = {}
      end

      def on_load(cpath = UNDEFINED, &block)
        if cpath.equal?(UNDEFINED)
          raise NotImplementedError,
            "roundhouse: Zeitwerk `on_load` without a constant path (every constant's load) is not ported: " \
            "only a constant path named by the app's own `on_load(\"Cpath\")` reports its load"
        end
        raise TypeError, "on_load only accepts strings" unless cpath.is_a?(String)

        cpath = cpath.delete_prefix("::")
        CNAME_VALIDATOR.const_defined?(cpath, false)
        reported = defined?(RoundhouseAutoload::AUTOLOAD_CALLBACK_CPATHS) &&
          RoundhouseAutoload::AUTOLOAD_CALLBACK_CPATHS.include?(cpath)
        if !reported && !Autoloaders.loaded?(cpath)
          $stderr.puts "roundhouse: error[autoload_callback_not_carried]: an on_load(#{cpath.inspect}) callback " \
                       "registered with #{@tag}, but no emitted file autoloads #{cpath}; it will not run"
        end
        (@on_load_callbacks[cpath] ||= []) << block
        nil
      end

      # A callback whose whole block ingest moved into the constant's
      # body (`ingest::load_hook_includes::lift_autoload_includes`): it
      # registers nothing, since the body already ran it. Natively it
      # would not run for a constant already loaded here, which is named.
      def on_load_lifted(cpath, site)
        if Autoloaders.loaded?(cpath)
          $stderr.puts "roundhouse: error[autoload_callback_lifted_after_load]: #{site}: #{cpath} was loaded before " \
                       "its on_load callback registered, so Zeitwerk would not run it, but the emitted #{cpath} " \
                       "runs it at the end of its body"
        end
        nil
      end

      # Zeitwerk::Loader#do_not_eager_load (zeitwerk 2.8.2
      # loader/eager_load.rb): the directories (or files) `eager_load`
      # skips, kept as expanded paths. The emitted tree's eager load is
      # the compiled list of app/models.rb (`RoundhouseAutoload.eager_load!`),
      # which does not consult them, so under an eager-loading
      # environment the exclusion is named, once per loader.
      def do_not_eager_load(*paths)
        paths = paths.flatten
        paths.each { |path| eager_load_exclusions << File.expand_path(path.to_s) }
        if !@exclusions_named && defined?(RoundhouseAutoload) && RoundhouseAutoload.eager_load?
          @exclusions_named = true
          $stderr.puts "roundhouse: error[autoload_exclusion_not_applied]: #{@tag}.do_not_eager_load(#{paths.first.inspect}, ...) " \
                       "is not applied: this tree's eager load is the compiled list of app/models.rb"
        end
        nil
      end

      def eager_load_exclusions # :nodoc:
        @eager_load_exclusions ||= Set.new
      end

      # loader/callbacks.rb:88 with reloading disabled: the constant's
      # callbacks, once each.
      def __on_file_autoloaded(cpath, value, abspath)
        callbacks = @on_load_callbacks.delete(cpath)
        callbacks&.each { |c| c.call(value, abspath) }
        nil
      end
    end
  end
end
