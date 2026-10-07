# frozen_string_literal: true

# activesupport 8.1.4 lib/active_support/backtrace_cleaner.rb and railties
# 8.1.4 lib/rails/backtrace_cleaner.rb, with `Rails.backtrace_cleaner`
# (railties rails.rb:55). Verbatim in behavior: filters run first, in
# order, then silencers; `dup` copies the filter and silencer lists
# (initialize_copy) so an app's `Rails.backtrace_cleaner.dup.add_silencer`
# leaves the shared cleaner alone. The default gem filter and gem/core/
# stdlib silencers read this process's Gem.path and rubylibdir, as Rails
# does.
#
# The Rails cleaner's root filter strips "#{Rails.root}/". This runtime's
# Rails.root is the emitted tree root ("."), so the filter strips a
# leading "./" only; absolute frames from the tree's require_relative
# chain keep their absolute path, and the app-dirs silencer then silences
# them as Rails silences any frame outside app/config/lib/test.
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

    def clean_locations(locations, kind = :silent)
      locations.select { |location| clean_frame(location, kind) }
    end

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

    def first_clean_frame(kind = :silent)
      Thread.each_caller_location(2) do |location|
        frame = clean_frame(location, kind)
        return frame if frame
      end
    end

    def first_clean_location(kind = :silent)
      Thread.each_caller_location(2) do |location|
        return location if clean_frame(location, kind)
      end
    end

    def add_filter(&block)
      @filters << block
    end

    def add_silencer(&block)
      @silencers << block
    end

    def remove_silencers!
      @silencers = []
    end

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
        add_silencer { |line| line.start_with?(RbConfig::CONFIG["rubylibdir"]) }
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
    APP_DIRS_PATTERN = /\A(?:\.\/)?(?:app|config|lib|test|\(\w+(?:-\w+)*\))/
    RENDER_TEMPLATE_PATTERN = /:in [`'].*_\w+_{2,3}\d+_\d+'/

    def initialize
      super
      add_filter do |line|
        # We may be called before Rails.root is assigned.
        # When that happens we fallback to not truncating.
        @root ||= Rails.root && "#{Rails.root}/"
        @root && line.start_with?(@root) ? line[@root.size..] : line
      end
      add_filter do |line|
        if RENDER_TEMPLATE_PATTERN.match?(line)
          line.sub(RENDER_TEMPLATE_PATTERN, "")
        else
          line
        end
      end
      add_silencer { |line| !APP_DIRS_PATTERN.match?(line) }
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
