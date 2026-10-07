# ActiveSupport::LogSubscriber::TestHelper (the activesupport native Core runs,
# Shopify/rails 5813559f4aed, 8.2.0.alpha), over the
# Ruby-family notification and log-subscriber runtime. Test-only: the PostgreSQL
# test bootstrap requires it when a test names it; SQL events there are
# delivered synchronously.
require "logger"
require_relative "active_support_log_subscriber"

module ActiveSupport
  class LogSubscriber
    module TestHelper
      # Rails' setup ends the chain at Minitest's empty `setup`; ActiveSupport's
      # own per-test work runs earlier, in before_setup. The emitted TestBase
      # does that work in `setup`, so continue to it first.
      def setup
        super if defined?(super)
        @logger = MockLogger.new
        @notifier = ActiveSupport::Notifications::Fanout.new

        ActiveSupport.colorize_logging = false

        @old_notifier = ActiveSupport::Notifications.notifier
        set_logger(@logger)
        ActiveSupport::Notifications.notifier = @notifier
      end

      def teardown
        set_logger(nil)
        ActiveSupport::Notifications.notifier = @old_notifier
        super if defined?(super)
      end

      class MockLogger
        include ::Logger::Severity

        attr_reader :flush_count
        attr_accessor :level

        def initialize(level = DEBUG)
          @flush_count = 0
          @level = level
          @logged = Hash.new { |h, k| h[k] = [] }
        end

        # Rails records any logger call by its method name.
        def method_missing(level, message = nil)
          if block_given?
            @logged[level] << yield
          else
            @logged[level] << message
          end
        end

        def logged(level)
          @logged[level].compact.map { |l| l.to_s.strip }
        end

        def flush
          @flush_count += 1
        end

        ::Logger::Severity.constants.each do |severity|
          value = ::Logger::Severity.const_get(severity)
          define_method(:"#{severity.downcase}?") { value >= @level }
        end
      end

      def wait
        @notifier.wait
      end

      def set_logger(logger)
        ActiveSupport::LogSubscriber.logger = logger
      end
    end
  end
end
