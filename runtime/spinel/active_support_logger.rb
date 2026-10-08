# frozen_string_literal: true

# ActiveSupport::Logger, ported from the activesupport native Core runs
# (Shopify/rails 5813559f4aed, 8.2.0.alpha: lib/active_support/logger.rb,
# logger_silence.rb, logger_thread_safe_level.rb) for the CRuby and JRuby
# trees, over Ruby's own Logger (the logger gem these trees load).
#
# Native's shape, because apps rely on it: `ActiveSupport::Logger <
# ::Logger`, so a subclass reads the severities (`DEBUG` .. `UNKNOWN`,
# Logger::Severity) as inherited constants, calls `add(severity, message,
# progname)`, keeps `@level` and reaches `level` through `super` (Core's
# StructuredLogger does all four). The thread-local level of
# LoggerThreadSafeLevel and `silence` of LoggerSilence come with it.
#
# The trees with no stdlib (Spinel and the transpiled targets) keep the
# hand-written runtime/ruby/logger.rb, which is not a ::Logger subclass;
# a read of a severity through ActiveSupport::Logger is refused there
# (`analyze::runtime_reads`).
#
# Differences, at the edges:
# - ActiveSupport::Concern is not used: LoggerSilence's `included` block
#   (`cattr_accessor :silencer, default: true` and the include of
#   LoggerThreadSafeLevel) is written out as an `included` hook with the
#   same methods and class variable.
# - `logger_outputs_to?` reads BroadcastLogger's broadcasts only where
#   that class is loaded (no port defines it), and `try(:filename)` /
#   `try(:dev)` are written as the respond_to? checks `try` performs.

require "logger"
require_relative "active_support_isolated_execution_state"

module ActiveSupport
  # logger_thread_safe_level.rb
  module LoggerThreadSafeLevel # :nodoc:
    def initialize(...)
      super
      @local_level_key = :"logger_thread_safe_level_#{object_id}"
    end

    def initialize_copy(other)
      super
      @local_level_key = :"logger_thread_safe_level_#{object_id}"
    end

    def local_level
      IsolatedExecutionState[local_level_key]
    end

    def local_level=(level)
      case level
      when Integer
      when Symbol
        level = Logger::Severity.const_get(level.to_s.upcase)
      when nil
      else
        raise ArgumentError, "Invalid log level: #{level.inspect}"
      end
      if level.nil?
        IsolatedExecutionState.delete(local_level_key)
      else
        IsolatedExecutionState[local_level_key] = level
      end
    end

    def level
      local_level || super
    end

    # Change the thread-local level for the duration of the given block.
    def log_at(level)
      old_local_level, self.local_level = local_level, level
      yield
    ensure
      self.local_level = old_local_level
    end

    private
      attr_reader :local_level_key
  end

  # logger_silence.rb
  module LoggerSilence
    # `included do cattr_accessor :silencer, default: true; include
    # ActiveSupport::LoggerThreadSafeLevel end`
    def self.included(base)
      super
      base.class_variable_set(:@@silencer, true)
      base.define_singleton_method(:silencer) { class_variable_get(:@@silencer) }
      base.define_singleton_method(:silencer=) { |value| class_variable_set(:@@silencer, value) }
      base.define_method(:silencer) { self.class.class_variable_get(:@@silencer) }
      base.define_method(:silencer=) { |value| self.class.class_variable_set(:@@silencer, value) }
      base.include(LoggerThreadSafeLevel)
    end

    # Silences the logger for the duration of the block.
    def silence(severity = Logger::ERROR)
      silencer ? log_at(severity) { yield self } : yield(self)
    end
  end

  # logger.rb
  class Logger < ::Logger
    include LoggerSilence

    # Returns true if the logger destination matches one of the sources
    def self.logger_outputs_to?(logger, *sources)
      loggers = if defined?(ActiveSupport::BroadcastLogger) && logger.is_a?(ActiveSupport::BroadcastLogger)
        logger.broadcasts
      else
        [logger]
      end

      logdevs = loggers.map { |logger| logger.instance_variable_get(:@logdev) }
      logger_sources = logdevs.filter_map do |logdev|
        filename = logdev.filename if logdev.respond_to?(:filename)
        filename || (logdev.dev if logdev.respond_to?(:dev))
      end

      normalize_sources(sources).intersect?(normalize_sources(logger_sources))
    end

    def initialize(*args, **kwargs)
      super
      @formatter ||= SimpleFormatter.new
    end

    # Simple formatter which only displays the message.
    class SimpleFormatter < ::Logger::Formatter
      # This method is invoked when a log event occurs
      def call(severity, timestamp, progname, msg)
        "#{String === msg ? msg : msg.inspect}\n"
      end
    end

    private
      def self.normalize_sources(sources)
        sources.map do |source|
          source = source.path if source.respond_to?(:path)
          source = File.realpath(source) if source.is_a?(String) && File.exist?(source)
          source
        end
      end
  end
end
