# frozen_string_literal: true

# ActiveSupport::Logger, ported from the activesupport native Core runs
# (Shopify/rails 5813559f4aed, 8.2.0.alpha: lib/active_support/logger.rb,
# logger_silence.rb, logger_thread_safe_level.rb, tagged_logging.rb and
# broadcast_logger.rb) for the CRuby and JRuby trees, over Ruby's own
# Logger (the logger gem these trees load).
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
# - `try(:filename)` / `try(:dev)` in `logger_outputs_to?` are written
#   as the respond_to? checks `try` performs.

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
      loggers = if logger.is_a?(BroadcastLogger)
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

module ActiveSupport
  # tagged_logging.rb. Rails' own shape on these trees, replacing the
  # shared port's wrapper class (runtime/ruby/logger.rb, kept for the
  # targets with no per-object method tables): `TaggedLogging.new(logger)`
  # clones the logger, extends its formatter with Formatter and the clone
  # with this module, so the result is still the logger it wrapped (a
  # ::Logger, or a BroadcastLogger) and answers every call form the
  # wrapped one does, `debug { ... }` and `add` included.
  #
  # Differences, at the edges: `delegate :push_tags, :pop_tags,
  # :clear_tags!, to: :formatter` is written out, and TagStack's
  # `reject!(&:blank?)` is Object#blank? written out (`blank_tag?`).
  module TaggedLogging
    module Formatter # :nodoc:
      # This method is invoked when a log event occurs.
      def call(severity, timestamp, progname, msg)
        super(severity, timestamp, progname, tag_stack.format_message(msg))
      end

      def tagged(*tags)
        pushed_count = tag_stack.push_tags(tags).size
        yield self
      ensure
        pop_tags(pushed_count)
      end

      def push_tags(*tags)
        tag_stack.push_tags(tags)
      end

      def pop_tags(count = 1)
        tag_stack.pop_tags(count)
      end

      def clear_tags!
        tag_stack.clear
      end

      def tag_stack
        # We use our object ID here to avoid conflicting with other instances
        @thread_key ||= "activesupport_tagged_logging_tags:#{object_id}"
        IsolatedExecutionState[@thread_key] ||= TagStack.new
      end

      def current_tags
        tag_stack.tags
      end

      def tags_text
        tag_stack.format_message("")
      end

      def freeze
        tag_stack
        super
      end
    end

    class TagStack # :nodoc:
      attr_reader :tags

      def initialize
        @tags = []
        @tags_string = nil
      end

      def push_tags(tags)
        @tags_string = nil
        tags.flatten!
        tags.reject! { |tag| blank_tag?(tag) }
        @tags.concat(tags)
        tags
      end

      def pop_tags(count)
        @tags_string = nil
        @tags.pop(count)
      end

      def clear
        @tags_string = nil
        @tags.clear
      end

      def format_message(message)
        if @tags.empty?
          message
        elsif @tags.size == 1
          "[#{@tags[0]}] #{message}"
        else
          @tags_string ||= "[#{@tags.join("] [")}] "
          "#{@tags_string}#{message}"
        end
      end

      private
        # Object#blank?: nil and false, a whitespace-only String, and an
        # empty collection.
        def blank_tag?(tag)
          case tag
          when nil, false then true
          when String then tag.match?(/\A[[:space:]]*\z/)
          else tag.respond_to?(:empty?) ? !!tag.empty? : false
          end
        end
    end

    module LocalTagStorage # :nodoc:
      attr_accessor :tag_stack

      def self.extended(base)
        base.tag_stack = TagStack.new
      end
    end

    def self.logger(*args, **kwargs)
      new ActiveSupport::Logger.new(*args, **kwargs)
    end

    def self.new(logger)
      logger = logger.clone

      if logger.formatter
        logger.formatter = logger.formatter.clone

        # Workaround for https://bugs.ruby-lang.org/issues/20250
        # Can be removed when Ruby 3.4 is the least supported version.
        logger.formatter.object_id if logger.formatter.is_a?(Proc)
      else
        # Ensure we set a default formatter so we aren't extending nil!
        logger.formatter = ActiveSupport::Logger::SimpleFormatter.new
      end

      logger.formatter.extend Formatter
      logger.extend(self)
    end

    def push_tags(*tags)
      formatter.push_tags(*tags)
    end

    def pop_tags(count = 1)
      formatter.pop_tags(count)
    end

    def clear_tags!
      formatter.clear_tags!
    end

    def tagged(*tags)
      if block_given?
        formatter.tagged(*tags) { yield self }
      else
        logger = ActiveSupport::TaggedLogging.new(self)
        logger.formatter.extend LocalTagStorage
        logger.push_tags(*formatter.current_tags, *tags)
        logger
      end
    end

    def flush
      clear_tags!
      super if defined?(super)
    end
  end

  # broadcast_logger.rb: writes each call to every logger it broadcasts
  # to. Rails 8.2's Rails.logger is one, over the application's logger
  # (railties application/bootstrap.rb `initialize_logger`), and an app
  # reads it back (`Rails.logger.broadcasts`, `broadcast_to`).
  class BroadcastLogger
    include ActiveSupport::LoggerSilence

    # Returns all the logger that are part of this broadcast.
    attr_reader :broadcasts
    attr_accessor :progname

    def initialize(*loggers)
      @broadcasts = []
      @progname = "Broadcast"

      broadcast_to(*loggers)
    end

    # Add logger(s) to the broadcast.
    def broadcast_to(*loggers)
      @broadcasts.concat(loggers)
    end

    # Remove a logger from the broadcast. When a logger is removed, messages sent to
    # the broadcast will no longer be written to its sink.
    def stop_broadcasting_to(logger)
      @broadcasts.delete(logger)
    end

    def local_level=(level)
      @broadcasts.each do |logger|
        logger.local_level = level if logger.respond_to?(:local_level=)
      end
    end

    def local_level
      loggers = @broadcasts.select { |logger| logger.respond_to?(:local_level) }

      loggers.map do |logger|
        logger.local_level
      end.first
    end

    LOGGER_METHODS = %w[
      << log add debug info warn error fatal unknown
      close formatter
    ].freeze # :nodoc:
    LOGGER_METHODS.each do |method|
      class_eval <<~RUBY, __FILE__, __LINE__ + 1
        def #{method}(...)
          return if @broadcasts.empty?

          return dispatch_block(:#{method}, ...) if block_given? && @broadcasts.size > 1

          result = @broadcasts[0].#{method}(...)
          1.upto(@broadcasts.size - 1) { |i| @broadcasts[i].#{method}(...) }
          result
        end
      RUBY
    end

    # Sets the log level for all the loggers in the broadcast.
    def level=(level)
      @broadcasts.each { |logger| logger.level = level }
    end

    def sev_threshold=(level)
      @broadcasts.each { |logger| logger.sev_threshold = level }
    end

    def formatter=(formatter)
      @broadcasts.each { |logger| logger.formatter = formatter }
    end

    # Returns the lowest level of all the loggers in the broadcast.
    def level
      @broadcasts.map(&:level).min
    end

    # True if the log level allows entries with severity +Logger::DEBUG+ to be written
    # to at least one broadcast. False otherwise.
    def debug?
      @broadcasts.any? { |logger| logger.debug? }
    end

    # Sets the log level to +Logger::DEBUG+ for the whole broadcast.
    def debug!
      @broadcasts.each(&:debug!)
    end

    def info?
      @broadcasts.any? { |logger| logger.info? }
    end

    def info!
      @broadcasts.each(&:info!)
    end

    def warn?
      @broadcasts.any? { |logger| logger.warn? }
    end

    def warn!
      @broadcasts.each(&:warn!)
    end

    def error?
      @broadcasts.any? { |logger| logger.error? }
    end

    def error!
      @broadcasts.each(&:error!)
    end

    def fatal?
      @broadcasts.any? { |logger| logger.fatal? }
    end

    def fatal!
      @broadcasts.each(&:fatal!)
    end

    def tagged(*tags, &block)
      if block
        tagging_loggers = @broadcasts.select { |logger| logger.respond_to?(:tagged) }

        _tagged(tagging_loggers, 0, tags, &block)
      else
        loggers = @broadcasts.map { |logger| logger.respond_to?(:tagged) ? logger.tagged(*tags) : logger }
        self.class.new(*loggers)
      end
    end

    def initialize_copy(other)
      @broadcasts = []
      @progname = other.progname.dup

      broadcast_to(*other.broadcasts.map(&:dup))
    end

    private
      def dispatch_block(method, *args, **kwargs)
        result = nil
        called, memo = false, nil

        @broadcasts.each_with_index do |logger, i|
          value = logger.send(method, *args, **kwargs) do |*args, **kwargs|
            if called
              memo
            else
              called = true
              memo = yield(*args, **kwargs)
            end
          end

          result = value if i.zero?
        end

        result
      end

      def _tagged(loggers, index, tags, &block)
        if index >= loggers.size
          yield self
        else
          loggers[index].tagged(*tags) do
            _tagged(loggers, index + 1, tags, &block)
          end
        end
      end

      def method_missing(name, ...)
        loggers = @broadcasts.select { |logger| logger.respond_to?(name) }

        if loggers.none?
          super
        elsif loggers.one?
          loggers.first.send(name, ...)
        else
          loggers.map { |logger| logger.send(name, ...) }
        end
      end

      def respond_to_missing?(method, include_all)
        @broadcasts.any? { |logger| logger.respond_to?(method, include_all) }
      end
  end
end
