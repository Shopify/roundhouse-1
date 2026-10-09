# frozen_string_literal: true

# activesupport 8.1.4 lib/active_support/lazy_load_hooks.rb, verbatim in
# behavior (Shopify/rails 5813559f4aed has the same file):
# `ActiveSupport.on_load(name, options, &block)` registers a hook (and
# runs it at once for every base already loaded under `name`);
# `ActiveSupport.run_load_hooks(name, base)` records `base` as loaded and
# runs every hook registered so far. A Module base runs the block under
# `class_eval`, anything else under `instance_eval`; `yield: true` calls
# it with the base instead; `run_once: true` runs a block at most once.
#
# CRuby-only (the PostgreSQL tree, `project::apply_on_load_hooks`). The
# emitted boot registers the app's lifted initializer hooks
# (`ingest::on_load_hooks`), and `DatabaseConfiguration.configure!` runs
# `:active_record_postgresqladapter` where Active Record would load the
# adapter class: inside establish_connection, before any connection.
#
# One divergence, error[load_hook_not_run]. A native gem (loaded from
# the gem home, outside this tree) registers hooks for framework classes
# this tree ports only in part: turbo-rails' engine includes
# Turbo::Broadcastable into ActiveRecord::Base, responders' requires
# active_support/core_ext/array/extract_options. Such a hook that raises
# a StandardError, LoadError or NotImplementedError does not run: boot
# names it and goes on, as runtime/rails_railtie.rb names a gem's
# railtie initializer it cannot run (error[gem_railtie_not_run]). A hook
# of this tree's own code raises as Rails does, unless the code that
# registered it already runs under that convention: a railtie or engine
# initializer (`Railtie.not_run_unless_it_runs`) or body statement
# (`Railtie.run_body_statement`), whose deferred hook is the same work.
#
# This file is also where the tree loads what active_support.rb sets up
# first: below, its `autoload` entries for the constants this tree ports.
module ActiveSupport
  module LazyLoadHooks
    def self.extended(base) # :nodoc:
      base.class_eval do
        @load_hooks = Hash.new { |h, k| h[k] = [] }
        @loaded     = Hash.new { |h, k| h[k] = [] }
        @run_once   = Hash.new { |h, k| h[k] = [] }
      end
    end

    def on_load(name, options = {}, &block)
      LazyLoadHooks.note_registration(block)
      @loaded[name].each do |base|
        execute_hook(name, base, options, block)
      end

      @load_hooks[name] << [block, options]
    end

    def run_load_hooks(name, base = Object)
      @loaded[name] << base
      @load_hooks[name].each do |hook, options|
        execute_hook(name, base, options, hook)
      end
    end

    private
      def with_execution_control(name, block, once)
        unless @run_once[name].include?(block)
          @run_once[name] << block if once

          yield
        end
      end

      def execute_hook(name, base, options, block)
        with_execution_control(name, block, options[:run_once]) do
          LazyLoadHooks.unless_it_runs(name, block) do
            if options[:yield]
              block.call(base)
            else
              if base.is_a?(Module)
                base.class_eval(&block)
              else
                base.instance_eval(&block)
              end
            end
          end
        end
      end

    # The emitted tree: this file is its runtime/active_support_lazy_load_hooks.rb.
    TREE_ROOT = File.expand_path("..", __dir__) # :nodoc:

    # The runtime/rails_railtie.rb methods under which a railtie's or
    # engine's code runs named-and-skipped, with the convention each is.
    RAILTIE_CONVENTIONS = { # :nodoc:
      "not_run_unless_it_runs" => "a railtie or engine initializer, error[gem_railtie_not_run]",
      "run_body_statement" => "a railtie or engine body statement, error[railtie_body_statement_not_run]",
    }.freeze

    @covered = {}.compare_by_identity
    @native = {}

    class << self
      # A hook this tree's code registers while a railtie's or engine's
      # initializer or body statement runs is covered by that statement's
      # convention.
      def note_registration(block) # :nodoc:
        return unless block
        return if native?(block.source_location&.first)
        frame = caller_locations(2).find do |location|
          RAILTIE_CONVENTIONS.key?(location.base_label) && File.basename(location.path.to_s) == "rails_railtie.rb"
        end
        @covered[block] = RAILTIE_CONVENTIONS.fetch(frame.base_label) if frame
      end

      # Runs one hook; a native gem's hook or a covered one that fails is
      # named, error[load_hook_not_run], and boot goes on.
      def unless_it_runs(name, block) # :nodoc:
        yield
      rescue StandardError, LoadError, NotImplementedError => e
        path, line = block&.source_location
        if native?(path)
          site, registered = "#{gem_site(path)}:#{line}", ""
        elsif (convention = @covered[block])
          site, registered = "#{tree_site(path)}:#{line}", " (registered by #{convention})"
        else
          raise
        end
        $stderr.puts "roundhouse: error[load_hook_not_run]: #{site} on_load(#{name.inspect}) did not run: #{e.class}: #{e.message.lines.first&.chomp}#{compiled_copy_note(e)}#{registered}"
        nil
      end

      private
        # A block defined in a file outside this tree: a gem home's (or
        # another native) file. A relative path is the cwd's (a test file
        # Ruby runs as `ruby test/x_test.rb`); an eval'd block or `-e`
        # has no file and is not native.
        def native?(path)
          return false if path.nil? || path.start_with?("(", "-")
          @native.fetch(path) do
            full = File.expand_path(path)
            @native[path] = !tree_relative(full)
          end
        end

        # `app/models/platform_component/admin/engine.rb` for a tree file.
        def tree_site(path)
          full = File.expand_path(path)
          tree_relative(full) || full
        end

        # The path below TREE_ROOT, as given or through symlinks; nil if
        # the file is not in this tree.
        def tree_relative(full)
          prefix = "#{TREE_ROOT}/"
          return full.delete_prefix(prefix) if full.start_with?(prefix)
          real = File.exist?(full) ? File.realpath(full) : full
          real.delete_prefix(prefix) if real.start_with?(prefix)
        end

        # `turbo-rails-2.0.13/lib/turbo/engine.rb` for a gem's file: a
        # loaded spec's, else the last `gems/<name>-<version>/` directory
        # of the path (a gem home's layout, which this boot puts on the
        # load path without activating a spec), else the path.
        def gem_site(path)
          spec = defined?(Gem.loaded_specs) && Gem.loaded_specs.each_value.find do |s|
            path.start_with?("#{s.full_gem_path}/")
          end
          return "#{spec.full_name}/#{path.delete_prefix("#{spec.full_gem_path}/")}" if spec
          path[%r{.*/gems/([^/]+-\d[^/]*/.+)\z}, 1] || path
        end

        def compiled_copy_note(e)
          return "" unless defined?(::Rails::Railtie) && ::Rails::Railtie.respond_to?(:compiled_copy_note)
          ::Rails::Railtie.compiled_copy_note(e)
        end
    end
  end

  extend LazyLoadHooks

  # active_support.rb's `autoload` entries (8.2.0.alpha: `extend
  # ActiveSupport::Autoload; autoload :Concern`, ...) for each constant
  # this tree ports in a file of its own, the file's absolute path for
  # Rails' `active_support/<name>` feature. As natively, a gem or app file
  # that names one with no require of its own loads it (identifications:
  # `extend ActiveSupport::Concern`). Each loads with nothing but this
  # file (tests/active_support_lazy_load_hooks_runtime.rb). The rest of
  # Rails' list is not ported, or its port also defines what Rails' file
  # does not (Deprecation and BacktraceCleaner define Rails', ClassAttribute
  # and OptionMerger core extensions, JSON is reopened by
  # active_support_ext), or needs a file boot loads before it (Inflector
  # calls module_delegate's `delegate`), so it is left to the requires boot
  # already makes. A port the tree does not carry (NumberHelper without
  # i18n) is not declared.
  {
    Concern: "active_support_concern",
    DescendantsTracker: "active_support_descendants_tracker",
    ErrorReporter: "active_support_reporters",
    EventReporter: "active_support_reporters",
    FileUpdateChecker: "active_support_file_update_checker",
    LogSubscriber: "active_support_log_subscriber",
    IsolatedExecutionState: "active_support_isolated_execution_state",
    Notifications: "active_support_notifications",
    SecureCompareRotator: "active_support_messages",
    Benchmark: "active_support_benchmark",
    Cache: "active_support_cache",
    Callbacks: "active_support_callbacks",
    ColorizeLogging: "active_support_log_subscriber",
    ExecutionContext: "active_support_reporters",
    KeyGenerator: "active_support_messages",
    MessageEncryptor: "active_support_messages",
    MessageVerifier: "active_support_messages",
    Multibyte: "active_support_multibyte_unicode",
    NumberHelper: "active_support_number_helper",
    OrderedOptions: "active_support_ordered_options",
    StringInquirer: "active_support_string_inquirer",
    Ractors: "active_support_ractors",
    Rescuable: "active_support_rescuable",
  }.each do |name, port|
    path = File.join(__dir__, "#{port}.rb")
    autoload name, path if File.exist?(path)
  end
end
