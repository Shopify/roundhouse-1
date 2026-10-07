# Ruby-family subscriber dispatch over our notification runtime. No native
# ActiveSupport is loaded by the emitted application. Ported from the
# ActiveSupport native Core runs (Shopify/rails 5813559f4aed, 8.2.0.alpha):
# subscriber.rb, log_subscriber.rb, colorize_logging.rb.
require_relative "active_support_notifications"

module ActiveSupport
  # `ActiveSupport.colorize_logging` (default true), which Rails 8.2's
  # `config.colorize_logging=` writes; LogSubscriber reads it.
  @colorize_logging = true unless instance_variable_defined?(:@colorize_logging)

  class << self
    attr_accessor :colorize_logging
  end

  # Rails 8.2's ColorizeLogging, which LogSubscriber includes: the level
  # helpers and `color` are public.
  module ColorizeLogging
    MODES = { clear: 0, bold: 1, italic: 3, underline: 4 }.freeze

    BLACK   = "\e[30m"
    RED     = "\e[31m"
    GREEN   = "\e[32m"
    YELLOW  = "\e[33m"
    BLUE    = "\e[34m"
    MAGENTA = "\e[35m"
    CYAN    = "\e[36m"
    WHITE   = "\e[37m"

    def info(progname = nil, &block)
      logger.info(progname, &block) if logger
    end

    def debug(progname = nil, &block)
      logger.debug(progname, &block) if logger
    end

    def warn(progname = nil, &block)
      logger.warn(progname, &block) if logger
    end

    def error(progname = nil, &block)
      logger.error(progname, &block) if logger
    end

    def fatal(progname = nil, &block)
      logger.fatal(progname, &block) if logger
    end

    def unknown(progname = nil, &block)
      logger.unknown(progname, &block) if logger
    end

    def color(text, color, mode_options = {})
      return text unless colorize_logging
      color = self.class.const_get(color.upcase) if color.is_a?(Symbol)
      mode = mode_from(mode_options)
      clear = "\e[#{MODES[:clear]}m"
      "#{mode}#{color}#{text}#{clear}"
    end

    def mode_from(options)
      modes = MODES.values_at(*options.reject { |_, value| value.nil? || value == false || (value.respond_to?(:empty?) && value.empty?) }.keys)
      "\e[#{modes.join(";")}m" if modes.any?
    end

    def colorize_logging
      ActiveSupport.colorize_logging
    end
  end
  class Subscriber
    SUBSCRIBERS = []

    class << self
      attr_reader :subscriber, :notifier, :namespace

      def subscribers
        SUBSCRIBERS
      end

      def attach_to(namespace, subscriber = new, notifier = Notifications, inherit_all: false)
        @namespace, @subscriber, @notifier = namespace, subscriber, notifier
        subscribers << subscriber
        fetch_public_methods(subscriber, inherit_all).each { |event| add_event_subscriber(event) }
      end

      def detach_from(namespace, notifier = Notifications)
        @namespace, @notifier = namespace, notifier
        @subscriber = subscribers.find { |entry| entry.instance_of?(self) }
        return unless @subscriber
        subscribers.delete(@subscriber)
        @subscriber.patterns.keys.each do |pattern|
          next unless pattern.end_with?(".#{namespace}")
          notifier.unsubscribe(@subscriber.patterns.delete(pattern))
        end
        @notifier = nil
      end

      def method_added(event)
        super
        add_event_subscriber(event) if @notifier && public_method_defined?(event)
      end

      private

      def fetch_public_methods(subscriber, inherit_all)
        subscriber.public_methods(inherit_all) - Subscriber.public_instance_methods(true)
      end

      def add_event_subscriber(event)
        return if [:start, :finish].include?(event.to_sym)
        pattern = "#{event}.#{namespace}"
        return if subscriber.patterns.key?(pattern)
        subscriber.patterns[pattern] = notifier.subscribe(pattern, subscriber)
      end
    end

    attr_reader :patterns

    def initialize
      @patterns = {}
      super
    end

    def call(event)
      send(event.name.split(".", 2).first, event)
    end
  end

  class LogSubscriber < Subscriber
    include ColorizeLogging

    LEVEL_CHECKS = [:debug, :info, :error].freeze

    class << self
      # Rails 8.2: a plain reader, set by railties' `active_support.
      # set_log_subscriber_logger` initializer (`LogSubscriber.logger =
      # Rails.logger`, after initialize_logger). The emitted boot runs no
      # railtie initializers, so the first read stands in for that one:
      # it takes Rails.logger unless a logger was assigned (nil included).
      def logger
        unless instance_variable_defined?(:@logger)
          self.logger = Rails.logger if defined?(Rails) && Rails.respond_to?(:logger)
        end
        @logger
      end

      def logger=(logger)
        @logger = logger
        @supports_flush = logger.respond_to?(:flush)
      end

      def log_levels
        @log_levels || (superclass.respond_to?(:log_levels) ? superclass.log_levels : {})
      end

      def log_subscribers
        subscribers
      end

      def attach_to(...)
        result = super
        set_event_levels
        result
      end

      def flush_all!
        logger.flush if @supports_flush
      end

      private

      def subscribe_log_level(event, level)
        raise KeyError, "unsupported log level: #{level}" unless LEVEL_CHECKS.include?(level)
        @log_levels = log_levels.merge(event => level).freeze
        set_event_levels
      end

      def set_event_levels
        subscriber.event_levels = log_levels.transform_keys { |event| "#{event}.#{namespace}" } if subscriber
      end

      def fetch_public_methods(subscriber, inherit_all)
        subscriber.public_methods(inherit_all) - LogSubscriber.public_instance_methods(true)
      end
    end

    attr_writer :event_levels

    def initialize
      super
      @event_levels = {}
    end

    def logger
      LogSubscriber.logger
    end

    def silenced?(event)
      level = @event_levels[event]
      logger.nil? || (level && !logger.public_send("#{level}?"))
    end

    def call(event)
      super if logger
    rescue StandardError => error
      unless ActiveSupport.respond_to?(:error_reporter)
        raise NotImplementedError, "LogSubscriber handler error reporting is not implemented: #{error.message}"
      end
      ActiveSupport.error_reporter.report(error, source: event.name)
      logger.error("Could not log #{event.name.inspect} event. #{error.class}: #{error.message} #{error.backtrace}") if logger
    end
  end
end
