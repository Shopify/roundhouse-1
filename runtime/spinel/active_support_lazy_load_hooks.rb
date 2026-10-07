# frozen_string_literal: true

# activesupport 8.1.4 lib/active_support/lazy_load_hooks.rb, verbatim in
# behavior: `ActiveSupport.on_load(name, options, &block)` registers a
# hook (and runs it at once for every base already loaded under `name`);
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

  extend LazyLoadHooks
end
