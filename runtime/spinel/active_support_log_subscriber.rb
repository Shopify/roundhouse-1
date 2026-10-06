# Ruby-family subscriber dispatch over our notification runtime. No native
# ActiveSupport is loaded by the emitted application.
require_relative "active_support_notifications"

module ActiveSupport
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
    LEVEL_CHECKS = [:debug, :info, :error].freeze

    class << self
      attr_writer :logger

      def logger
        @logger ||= Rails.logger if defined?(Rails) && Rails.respond_to?(:logger)
        @logger
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
        logger.flush if logger.respond_to?(:flush)
      end

      private

      def subscribe_log_level(event, level)
        raise KeyError, "unsupported log level: #{level}" unless LEVEL_CHECKS.include?(level)
        @log_levels = log_levels.merge(event => level)
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

    private

    def debug(message = nil, &block)
      logger.debug(message, &block) if logger
    end

    def info(message = nil, &block)
      logger.info(message, &block) if logger
    end

    def warn(message = nil, &block)
      logger.warn(message, &block) if logger
    end

    def error(message = nil, &block)
      logger.error(message, &block) if logger
    end

    def fatal(message = nil, &block)
      logger.fatal(message, &block) if logger
    end

    def unknown(message = nil, &block)
      logger.unknown(message, &block) if logger
    end
  end
end
