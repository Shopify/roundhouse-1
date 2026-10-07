# Ruby-family notification boundary. Events are delivered synchronously around
# real operations; no Rails runtime or post-hoc query-log replay is involved.
# Delivery order and the subscribe API follow the ActiveSupport native Core
# runs (Shopify/rails 5813559f4aed, 8.2.0.alpha, notifications/fanout.rb):
# a name's String subscribers first, in subscription order (`prepend: true`
# puts one first), then the Regexp and catch-all ones.
require "securerandom"

module ActiveSupport
  module Notifications
    class Event
      attr_reader :name, :time, :end, :transaction_id, :duration
      attr_accessor :payload

      def initialize(name, started, finished, transaction_id, payload, duration)
        @name = name
        @time = started
        @end = finished
        @transaction_id = transaction_id
        @payload = payload
        @duration = duration
      end
    end

    Subscription = Struct.new(:pattern, :callback, :monotonic, :exclusions) do
      def matches?(name)
        (pattern.nil? || pattern === name) && !exclusions.include?(name)
      end
    end

    class Fanout
      def initialize
        @subscriptions = []
        @mutex = Mutex.new
      end

      def subscribe(pattern = nil, callback = nil, prepend: false, &block)
        add_subscription(pattern, callback || block, false, prepend: prepend)
      end

      def monotonic_subscribe(pattern = nil, callback = nil, prepend: false, &block)
        add_subscription(pattern, callback || block, true, prepend: prepend)
      end

      def add_subscription(pattern, callback, monotonic, prepend: false)
        unless pattern.nil? || pattern.is_a?(String) || pattern.is_a?(Regexp)
          raise ArgumentError, "notification pattern must be a String, Regexp or nil"
        end
        raise ArgumentError, "Cannot prepend Regex subscribers" if prepend && !pattern.is_a?(String)
        raise ArgumentError, "notification subscriber must be callable" unless callback.respond_to?(:call)
        subscription = Subscription.new(pattern, callback, monotonic, [])
        @mutex.synchronize { prepend ? @subscriptions.unshift(subscription) : @subscriptions << subscription }
        subscription
      end

      def unsubscribe(subscriber)
        @mutex.synchronize do
          @subscriptions.delete_if do |entry|
            if subscriber.is_a?(String)
              entry.exclusions << subscriber if entry.pattern.is_a?(Regexp) && entry.pattern === subscriber
              entry.pattern == subscriber
            else
              entry.equal?(subscriber)
            end
          end
        end
        nil
      end

      def subscribed(callback, pattern = nil, monotonic: false)
        subscription = add_subscription(pattern, callback, monotonic)
        begin
          yield
        ensure
          unsubscribe(subscription)
        end
      end

      def instrument(name, payload = {})
        listeners = listeners_for(name)
        return block_given? ? yield(payload) : nil if listeners.empty?

        identities = Thread.current.thread_variable_get(:roundhouse_notification_ids) ||
          Thread.current.thread_variable_set(:roundhouse_notification_ids, {})
        id = identities[self] ||= SecureRandom.hex(10)
        started = Time.now
        monotonic_start = Process.clock_gettime(Process::CLOCK_MONOTONIC)
        begin
          block_given? ? yield(payload) : nil
        rescue Exception => error
          payload[:exception] = [error.class.name, error.message]
          payload[:exception_object] = error
          raise
        ensure
          finished = Time.now
          monotonic_finish = Process.clock_gettime(Process::CLOCK_MONOTONIC)
          duration = (monotonic_finish - monotonic_start) * 1000.0
          event = Event.new(name, monotonic_start, monotonic_finish, id, payload, duration)
          listeners.each do |entry|
            callback = entry.callback
            procish = callback.respond_to?(:parameters) ? callback : callback.method(:call)
            if procish.arity == 1 && procish.parameters.length == 1
              callback.call(event)
            else
              start_time, end_time = entry.monotonic ? [monotonic_start, monotonic_finish] : [started, finished]
              callback.call(name, start_time, end_time, id, payload)
            end
          end
        end
      end

      def listeners_for(name)
        entries = @mutex.synchronize { @subscriptions.select { |entry| entry.matches?(name) } }
        entries = entries.partition { |entry| entry.pattern.is_a?(String) }.flatten(1)
        entries.reject { |entry| entry.callback.respond_to?(:silenced?) && entry.callback.silenced?(name) }
      end

      def listening?(name)
        !listeners_for(name).empty?
      end

      # Delivery is synchronous; there is no background queue to wait for.
      def wait
        nil
      end
    end

    # Bound to one notifier for its lifetime, as in ActiveSupport: a caller
    # that keeps an instrumenter keeps publishing to that notifier even after
    # `Notifications.notifier` is replaced.
    class Instrumenter
      attr_reader :id

      def initialize(notifier)
        @notifier = notifier
        @id = SecureRandom.hex(10)
      end

      def instrument(name, payload = {}, &block)
        @notifier.instrument(name, payload, &block)
      end
    end

    class << self
      attr_accessor :notifier

      # One instrumenter per notifier per thread (ActiveSupport keeps this
      # registry in IsolatedExecutionState, thread-scoped by default).
      def instrumenter
        registry = Thread.current.thread_variable_get(:roundhouse_notifications_registry) ||
          Thread.current.thread_variable_set(:roundhouse_notifications_registry, {})
        registry[notifier] ||= Instrumenter.new(notifier)
      end

      def subscribe(pattern = nil, callback = nil, prepend: false, &block)
        notifier.subscribe(pattern, callback, prepend: prepend, &block)
      end

      def monotonic_subscribe(pattern = nil, callback = nil, prepend: false, &block)
        notifier.monotonic_subscribe(pattern, callback, prepend: prepend, &block)
      end

      def unsubscribe(subscriber)
        notifier.unsubscribe(subscriber)
      end

      def subscribed(callback, pattern = nil, monotonic: false)
        subscriber = notifier.add_subscription(pattern, callback, monotonic)
        yield
      ensure
        unsubscribe(subscriber)
      end

      def instrument(name, payload = {}, &block)
        notifier.instrument(name, payload, &block)
      end
    end

    self.notifier = Fanout.new
  end
end
