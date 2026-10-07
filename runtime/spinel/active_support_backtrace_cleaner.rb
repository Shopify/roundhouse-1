# frozen_string_literal: true

# activesupport lib/active_support/backtrace_cleaner.rb and railties
# lib/rails/backtrace_cleaner.rb, with `Rails.backtrace_cleaner` (railties
# rails.rb:55-57), from the Rails native Core runs: Shopify/rails
# 5813559f4aed, 8.2.0.alpha (docs/RAILS_VERSION.md). Compared against it by
# tests/backtrace_cleaner_runtime.rb.
#
# Filters run first, in order, then silencers. `dup` copies the filter and
# silencer lists (initialize_copy), so an app's
# `Rails.backtrace_cleaner.dup.add_silencer { ... }` leaves the shared
# cleaner alone. The default gem filter and the gem, core and stdlib
# silencers read this process's Gem.path, Gem.default_dir and rubylibdir,
# as Rails does.
#
# Rails' root filter strips "#{Rails.root}/". Rails.root in a Rails app is
# absolute: `find_root` answers the realpath of the directory holding
# config.ru (railties application.rb). This runtime's Rails.root is the
# emitted tree root written relative, "." (runtime/ruby/rails.rb), and the
# emitted app runs from it, so `Rails::BacktraceCleaner.root` is the
# realpath of that directory. A frame of a file the tree requires (an
# absolute realpath) is then made root-relative, as in Rails.
#
# One shape-only difference: Rails wraps each default filter and silencer
# in `ActiveSupport::Ractors.shareable_proc` (Ractor.shareable_proc on Ruby
# 4.0), for `config.ractor_shareable`. This runtime has no Ractor support
# and the procs are plain blocks; what each one answers is the same.
#
# CRuby-only (the PostgreSQL tree, `project::apply_postgres_bootstrap`).
require "rbconfig"

module ActiveSupport
  class BacktraceCleaner
    def initialize
      @filters, @silencers = [], []
      add_core_silencer
      add_gem_filter
      add_gem_silencer
      add_stdlib_silencer
    end

    # Returns the backtrace after all filters and silencers have been run
    # against it. Filters run first, then silencers.
    def clean(backtrace, kind = :silent)
      filtered = filter_backtrace(backtrace)

      case kind
      when :silent
        silence(filtered)
      when :noise
        noise(filtered)
      else
        filtered
      end
    end
    alias :filter :clean

    # Given an array of Thread::Backtrace::Location objects, returns an
    # array with the clean ones. Filters and silencers receive the
    # locations' strings; the locations themselves are returned unchanged.
    def clean_locations(locations, kind = :silent)
      locations.select { |location| clean_frame(location, kind) }
    end

    # Returns the frame with all filters applied, or nil if the frame was
    # silenced.
    def clean_frame(frame, kind = :silent)
      frame = frame.to_s
      @filters.each do |f|
        frame = f.call(frame.to_s)
      end

      case kind
      when :silent
        frame unless @silencers.any? { |s| s.call(frame) }
      when :noise
        frame if @silencers.any? { |s| s.call(frame) }
      else
        frame
      end
    end

    if Thread.method(:each_caller_location).arity == 0
      # Returns the first clean frame of the caller's backtrace, or nil.
      # Frames are strings.
      def first_clean_frame(kind = :silent)
        caller_location_skipped = false

        Thread.each_caller_location do |location|
          unless caller_location_skipped
            caller_location_skipped = true
            next
          end

          frame = clean_frame(location, kind)
          return frame if frame
        end
      end

      # Returns the first clean location of the caller's call stack, or nil.
      def first_clean_location(kind = :silent)
        caller_location_skipped = false

        Thread.each_caller_location do |location|
          unless caller_location_skipped
            caller_location_skipped = true
            next
          end

          return location if clean_frame(location, kind)
        end
      end
    else
      # Returns the first clean frame of the caller's backtrace, or nil.
      # Frames are strings.
      def first_clean_frame(kind = :silent)
        Thread.each_caller_location(2) do |location|
          frame = clean_frame(location, kind)
          return frame if frame
        end
      end

      # Returns the first clean location of the caller's call stack, or nil.
      def first_clean_location(kind = :silent)
        Thread.each_caller_location(2) do |location|
          return location if clean_frame(location, kind)
        end
      end
    end

    # Adds a filter from the block provided. Each line in the backtrace
    # will be mapped against this filter.
    def add_filter(&block)
      @filters << block
    end

    # Adds a silencer from the block provided. If the silencer returns
    # true for a given line, it will be excluded from the clean backtrace.
    def add_silencer(&block)
      @silencers << block
    end

    # Removes all silencers, but leaves in the filters.
    def remove_silencers!
      @silencers = []
    end

    # Removes all filters, but leaves in the silencers.
    def remove_filters!
      @filters = []
    end

    private
      FORMATTED_GEMS_PATTERN = /\A[^\/]+ \([\w.]+\) /

      def initialize_copy(_other)
        @filters = @filters.dup
        @silencers = @silencers.dup
      end

      def add_gem_filter
        gems_paths = (Gem.path | [Gem.default_dir]).map { |p| Regexp.escape(p) }
        return if gems_paths.empty?

        gems_regexp = %r{\A(#{gems_paths.join('|')})/(bundler/)?gems/([^/]+)-([\w.]+)/(.*)}
        gems_result = '\3 (\4) \5'
        add_filter { |line| line.sub(gems_regexp, gems_result) }
      end

      def add_core_silencer
        add_silencer { |line| line.include?("<internal:") }
      end

      def add_gem_silencer
        add_silencer { |line| FORMATTED_GEMS_PATTERN.match?(line) }
      end

      def add_stdlib_silencer
        rubylibdir = -RbConfig::CONFIG["rubylibdir"]
        add_silencer { |line| line.start_with?(rubylibdir) }
      end

      def filter_backtrace(backtrace)
        @filters.each do |f|
          backtrace = backtrace.map { |line| f.call(line.to_s) }
        end

        backtrace
      end

      def silence(backtrace)
        @silencers.each do |s|
          backtrace = backtrace.reject { |line| s.call(line.to_s) }
        end

        backtrace
      end

      def noise(backtrace)
        backtrace.select do |line|
          @silencers.any? do |s|
            s.call(line.to_s)
          end
        end
      end
  end
end

module Rails
  class BacktraceCleaner < ActiveSupport::BacktraceCleaner # :nodoc:
    RENDER_TEMPLATE_PATTERN = /:in [`'].*_\w+_{2,3}\d+_\d+'/

    class << self
      # "#{Rails.root}/", with Rails.root absolute as in Rails (see the
      # file note).
      def root
        @root ||= Rails.root && "#{File.realpath(Rails.root.to_s)}/".freeze
      end
    end

    def initialize
      super
      add_filter do |line|
        # We may be called before Rails.root is assigned.
        # When that happens we fallback to not truncating.
        BacktraceCleaner.root && line.start_with?(BacktraceCleaner.root) ? line[BacktraceCleaner.root.size..] : line
      end
      add_filter do |line|
        if RENDER_TEMPLATE_PATTERN.match?(line)
          line.sub(RENDER_TEMPLATE_PATTERN, "")
        else
          line
        end
      end

      add_silencer do |line|
        line.start_with?(File::SEPARATOR, "vendor/", "bin/")
      end
    end

    def freeze
      self.class.root
      super
    end

    def clean(backtrace, kind = :silent)
      return backtrace if ENV["BACKTRACE"]

      super(backtrace, kind)
    end
    alias_method :filter, :clean

    def clean_frame(frame, kind = :silent)
      return frame if ENV["BACKTRACE"]

      super(frame, kind)
    end
  end

  def self.backtrace_cleaner
    @backtrace_cleaner ||= Rails::BacktraceCleaner.new
  end
end
