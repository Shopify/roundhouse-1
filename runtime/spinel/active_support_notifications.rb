# Ruby-family notification boundary. Events are delivered synchronously around
# real operations; no Rails runtime or post-hoc query-log replay is involved.
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

    @subscriptions = []
    @mutex = Mutex.new

    def self.subscribe(pattern = nil, callback = nil, &block)
      add_subscription(pattern, callback || block, false)
    end

    def self.monotonic_subscribe(pattern = nil, callback = nil, &block)
      add_subscription(pattern, callback || block, true)
    end

    def self.add_subscription(pattern, callback, monotonic)
      unless pattern.nil? || pattern.is_a?(String) || pattern.is_a?(Regexp)
        raise ArgumentError, "notification pattern must be a String, Regexp or nil"
      end
      raise ArgumentError, "notification subscriber must be callable" unless callback.respond_to?(:call)
      subscription = Subscription.new(pattern, callback, monotonic, [])
      @mutex.synchronize { @subscriptions << subscription }
      subscription
    end

    def self.unsubscribe(subscriber)
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

    def self.subscribed(callback, pattern = nil, monotonic: false)
      subscription = add_subscription(pattern, callback, monotonic)
      begin
        yield
      ensure
        unsubscribe(subscription)
      end
    end

    def self.instrument(name, payload = {})
      listeners = @mutex.synchronize { @subscriptions.select { |entry| entry.matches?(name) } }
      return block_given? ? yield(payload) : nil if listeners.empty?

      id = Thread.current.thread_variable_get(:roundhouse_notification_id) ||
        Thread.current.thread_variable_set(:roundhouse_notification_id, SecureRandom.hex(10))
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
  end
end
