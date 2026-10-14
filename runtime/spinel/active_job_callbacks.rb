# frozen_string_literal: true

# ActiveJob::Callbacks, with the parts of ActiveJob::Core, Enqueuing and
# Execution that run its two chains, and ActiveJob::QueueName, ported from
# the activejob native Core runs (Shopify/rails 5813559f4aed, 8.2.0.alpha:
# lib/active_job/callbacks.rb, core.rb, enqueuing.rb, execution.rb,
# queue_name.rb) for the CRuby and JRuby trees.
#
# Core's jobs register `before_/after_/around_perform` and `_enqueue`
# callbacks, `set_callback` on chains of their own and `rescue_from`
# handlers, in their bodies and in their concerns' `included` blocks
# (BackgroundQueue::Job and its two dozen concerns). Each method follows
# the native one; the chains are ActiveSupport::Callbacks' (its port,
# active_support_callbacks.rb) and the handlers ActiveSupport::Rescuable's
# (active_support_rescuable.rb).
#
# Where it runs: Rails' ActiveJob::Base includes Core, Enqueuing,
# Execution (and through it ActiveSupport::Rescuable) and Callbacks (and
# through it ActiveSupport::Callbacks). The shared runtime's ActiveJob::Base
# is the bare class every target carries (active_job.rb), so the topmost
# app job class of a hierarchy that registers a callback or a handler
# includes this module instead (`lower::job_class_side`), which brings the
# same modules, in the same order, to every job below it. Its jobs take
# `perform_now` and `perform_later` from here rather than from the
# per-class wrappers the other jobs get.
#
# The queue adapter is the shared runtime's (active_job.rb): `_raw_enqueue`
# does what the generated `perform_later` wrapper does, inside the enqueue
# chain, as Rails' adapter call is: it logs the job for the test helpers
# and then holds it (the `:test` adapter), hands it to the drain, or runs
# it inline. The job that runs is a new instance of the class with the
# enqueued job's arguments, id and executions, as the instance Rails
# deserializes from the enqueued payload is.
#
# Differences, at the edges:
# - ActiveSupport::Concern is not used: each `included` block is written
#   out as Concern#append_features (`define_callbacks :perform` and
#   `:enqueue`, `skip_after_callbacks_if_terminated: true`).
# - Not ported, with no observable callback behaviour: Instrumentation and
#   Logging (their `around_enqueue` callbacks and `perform_now` wrappers
#   emit notifications and tag a logger), ExecutionState's locale and time
#   zone, the singleton `:execute` chain around `Base.execute`,
#   `ActiveSupport::ExecutionContext[:job]`, and QueuePriority's class
#   side. The
#   queue name is QueueName's, and routes nothing: the in-process adapter
#   has one queue (active_job.rb), so it is only what a job and its
#   callbacks read back.
# - QueueName's class attributes are written out as their readers and
#   writers, Concern's `included` block as `append_features`, and
#   `ActiveSupport::Ractors.try_shareable_proc` as the block itself (it
#   returns the block unless an app configures Ractor shareability).
# - Exceptions has `discard_on`, `after_discard` and `retry_job`, without
#   their `instrument` notifications. `retry_on` is not ported: Core's
#   BackgroundQueue::Retry replaces it with a `raise`, and its waits
#   schedule a re-enqueue the in-process adapter has no schedule for (the
#   shared base's `retry_on` stays inert, active_job.rb).
# - `set` keeps `scheduled_at`; the in-process adapter has no schedule, so
#   a scheduled job is enqueued like any other, as the generated wrapper
#   does with the `set(wait:)` options it drops.
# - `_raw_enqueue` rescues no ActiveJob::EnqueueError: the shared adapter
#   raises none.
# - Core's `serialize`/`deserialize` (core.rb) are here, with
#   ActiveJob::Arguments (arguments.rb) and ActiveJob::Serializers and its
#   built-in serializers (serializers.rb, serializers/*): Core's jobs
#   extend the payload with `super.merge(...)`, BackgroundQueue::Logging
#   logs it, and Core calls `ActiveJob::Arguments.serialize` itself. The
#   in-process adapter still hands the job its arguments as they are, so
#   `serialize` is what a job and its callers read, not a round trip.
#   `delegate` and `safe_constantize` are written out; a serializer whose
#   `klass` the tree does not define serializes nothing.

require "securerandom"
require "set"
require "singleton"
require "time"
require "date"
require "bigdecimal"
require_relative "active_support_callbacks"
require_relative "active_support_rescuable"

module ActiveJob
  # The job's state, from ActiveJob::Core.
  module Core
    attr_accessor :arguments
    attr_accessor :scheduled_at
    attr_accessor :job_id
    attr_accessor :executions
    attr_accessor :exception_executions
    attr_writer :successfully_enqueued # :nodoc:

    def successfully_enqueued?
      @successfully_enqueued
    end

    attr_accessor :enqueue_error

    # Queue in which the job will reside.
    attr_writer :queue_name

    # The job's arguments as `serialize` wrote them, until `perform_now`
    # deserializes them.
    attr_writer :serialized_arguments
    attr_writer :priority
    attr_accessor :provider_job_id
    attr_accessor :locale
    attr_accessor :timezone
    attr_accessor :enqueued_at

    # QueuePriority#priority: a Proc priority is evaluated on the job.
    def priority
      @priority = instance_exec(&@priority) if @priority.is_a?(Proc)
      @priority
    end

    # Creates a new job instance. Takes the arguments that will be
    # passed to the perform method.
    def initialize(*arguments, **kwargs)
      arguments << Hash.ruby2_keywords_hash(kwargs) unless kwargs.empty?
      @arguments  = arguments
      @job_id     = SecureRandom.uuid
      @scheduled_at = nil
      @executions = 0
      @exception_executions = {}
      @queue_name = self.class.queue_name
      @timezone = Time.zone&.name if Time.respond_to?(:zone)
    end

    # Configures the job with the given options.
    def set(options = {}) # :nodoc:
      self.scheduled_at = Time.now + options[:wait].to_f if options[:wait]
      self.scheduled_at = options[:wait_until] if options[:wait_until]
      self.queue_name   = self.class.queue_name_from_part(options[:queue]) if options[:queue]
      self.priority     = options[:priority].to_i if options[:priority]
      self
    end

    # The job as a Hash of primitives, the payload a queue adapter
    # stores; an app job extends it with `super.merge(...)`.
    def serialize
      {
        "job_class"  => self.class.name,
        "job_id"     => job_id,
        "provider_job_id" => provider_job_id,
        "queue_name" => queue_name,
        "priority"   => priority,
        "arguments"  => serialize_arguments_if_needed(arguments),
        "executions" => executions,
        "exception_executions" => exception_executions,
        "locale"     => locale || (I18n.locale.to_s if defined?(I18n)),
        "timezone"   => timezone,
        "enqueued_at" => Time.now.utc.iso8601(9),
        "scheduled_at" => scheduled_at ? scheduled_at.utc.iso8601(9) : nil,
      }
    end

    # Attaches the stored job data to the current instance; the arguments
    # stay serialized until `perform_now` needs them.
    def deserialize(job_data)
      self.job_id               = job_data["job_id"]
      self.provider_job_id      = job_data["provider_job_id"]
      self.queue_name           = job_data["queue_name"]
      self.priority             = job_data["priority"]
      self.serialized_arguments = job_data["arguments"]
      self.executions           = job_data["executions"]
      self.exception_executions = job_data["exception_executions"]
      self.locale               = job_data["locale"] || (I18n.locale.to_s if defined?(I18n))
      self.timezone             = job_data["timezone"] || (Time.zone&.name if Time.respond_to?(:zone))
      self.enqueued_at          = deserialize_time(job_data["enqueued_at"]) if job_data["enqueued_at"]
      self.scheduled_at         = deserialize_time(job_data["scheduled_at"]) if job_data["scheduled_at"]
    end

    module ClassMethods
      # Returns a ConfiguredJob (configured_job.rb) that holds the options
      # until it is performed or enqueued:
      #
      #   MyJob.set(wait: 5.minutes, queue: :low).perform_later(post)
      def set(options = {})
        ConfiguredJob.new(self, options)
      end

      # Creates a new job instance from a hash created with +serialize+.
      def deserialize(job_data)
        job_class = begin
          Object.const_get(job_data["job_class"])
        rescue NameError
          nil
        end
        raise UnknownJobClassError, job_data["job_class"] unless job_class

        job = job_class.new
        job.deserialize(job_data)
        job
      end
    end

    private
      def serialize_arguments_if_needed(arguments)
        if arguments_serialized?
          @serialized_arguments
        else
          serialize_arguments(arguments)
        end
      end

      def deserialize_arguments_if_needed
        if arguments_serialized?
          @arguments = deserialize_arguments(@serialized_arguments)
          @serialized_arguments = nil
        end
      end

      def serialize_arguments(arguments)
        Arguments.serialize(arguments)
      end

      def deserialize_arguments(serialized_args)
        Arguments.deserialize(serialized_args)
      end

      def arguments_serialized?
        @serialized_arguments
      end

      def deserialize_time(time)
        time.is_a?(Time) ? time : Time.iso8601(time)
      end
  end

  # ActiveJob::Arguments: a job's arguments to and from primitives.
  module Arguments
    extend self

    GLOBALID_KEY = "_aj_globalid"
    SYMBOL_KEYS_KEY = "_aj_symbol_keys"
    RUBY2_KEYWORDS_KEY = "_aj_ruby2_keywords"
    WITH_INDIFFERENT_ACCESS_KEY = "_aj_hash_with_indifferent_access"
    OBJECT_SERIALIZER_KEY = "_aj_serialized"

    RESERVED_KEYS = [
      GLOBALID_KEY, GLOBALID_KEY.to_sym,
      SYMBOL_KEYS_KEY, SYMBOL_KEYS_KEY.to_sym,
      RUBY2_KEYWORDS_KEY, RUBY2_KEYWORDS_KEY.to_sym,
      OBJECT_SERIALIZER_KEY, OBJECT_SERIALIZER_KEY.to_sym,
      WITH_INDIFFERENT_ACCESS_KEY, WITH_INDIFFERENT_ACCESS_KEY.to_sym,
    ].to_set
    private_constant :RESERVED_KEYS, :GLOBALID_KEY,
      :SYMBOL_KEYS_KEY, :RUBY2_KEYWORDS_KEY, :WITH_INDIFFERENT_ACCESS_KEY

    # Serializes a set of arguments. Intrinsic types that can safely be
    # serialized without mutation are returned as-is. Arrays/Hashes are
    # serialized element by element. All other types are serialized using
    # GlobalID or a registered serializer.
    def serialize(arguments)
      arguments.map { |argument| serialize_argument(argument) }
    end

    def serialize_argument(argument) # :nodoc:
      case argument
      when nil, true, false, Integer, Float # Types that can hardly be subclassed
        argument
      when String
        if argument.class == String
          argument
        else
          begin
            Serializers.serialize(argument)
          rescue SerializationError
            argument
          end
        end
      when Symbol
        { OBJECT_SERIALIZER_KEY => "ActiveJob::Serializers::SymbolSerializer", "value" => argument.name }
      when Array
        argument.map { |arg| serialize_argument(arg) }
      when Hash
        if indifferent_hash?(argument)
          serialize_indifferent_hash(argument)
        else
          symbol_keys = argument.keys
          symbol_keys.select! { |k| k.is_a?(Symbol) }
          symbol_keys.map!(&:name)

          aj_hash_key = if Hash.ruby2_keywords_hash?(argument)
            RUBY2_KEYWORDS_KEY
          else
            SYMBOL_KEYS_KEY
          end
          result = serialize_hash(argument)
          result[aj_hash_key] = symbol_keys
          result
        end
      else
        if defined?(GlobalID::Identification) && argument.is_a?(GlobalID::Identification)
          convert_to_global_id_hash(argument)
        else
          Serializers.serialize(argument)
        end
      end
    end

    # Deserializes a set of arguments. Intrinsic types that can safely be
    # deserialized without mutation are returned as-is. Arrays/Hashes are
    # deserialized element by element. All other types are deserialized
    # using GlobalID or a registered serializer.
    def deserialize(arguments)
      arguments.map { |argument| deserialize_argument(argument) }
    rescue
      raise DeserializationError
    end

    private
      def indifferent_hash?(argument)
        defined?(ActiveSupport::HashWithIndifferentAccess) && argument.is_a?(ActiveSupport::HashWithIndifferentAccess)
      end

      def deserialize_argument(argument)
        case argument
        when nil, true, false, String, Integer, Float
          argument
        when Array
          argument.map { |arg| deserialize_argument(arg) }
        when Hash
          if serialized_global_id?(argument)
            deserialize_global_id argument
          elsif custom_serialized?(argument)
            Serializers.deserialize(argument)
          else
            deserialize_hash(argument)
          end
        else
          raise ArgumentError, "Can only deserialize primitive arguments: #{argument.inspect}"
        end
      end

      def serialized_global_id?(hash)
        hash.size == 1 && hash.include?(GLOBALID_KEY)
      end

      def deserialize_global_id(hash)
        GlobalID::Locator.locate hash[GLOBALID_KEY]
      end

      def custom_serialized?(hash)
        hash.key?(OBJECT_SERIALIZER_KEY)
      end

      def serialize_hash(argument)
        argument.each_with_object({}) do |(key, value), hash|
          hash[serialize_hash_key(key)] = serialize_argument(value)
        end
      end

      def deserialize_hash(serialized_hash)
        result = serialized_hash.transform_values { |v| deserialize_argument(v) }
        if result.delete(WITH_INDIFFERENT_ACCESS_KEY)
          result = result.with_indifferent_access
        elsif symbol_keys = result.delete(SYMBOL_KEYS_KEY)
          result = transform_symbol_keys(result, symbol_keys)
        elsif symbol_keys = result.delete(RUBY2_KEYWORDS_KEY)
          result = transform_symbol_keys(result, symbol_keys)
          result = Hash.ruby2_keywords_hash(result)
        end
        result
      end

      def serialize_hash_key(key)
        case key
        when RESERVED_KEYS
          raise SerializationError.new("Can't serialize a Hash with reserved key #{key.inspect}")
        when String
          key
        when Symbol
          key.name
        else
          raise SerializationError.new("Only string and symbol hash keys may be serialized as job arguments, but #{key.inspect} is a #{key.class}")
        end
      end

      def serialize_indifferent_hash(indifferent_hash)
        result = serialize_hash(indifferent_hash)
        result[WITH_INDIFFERENT_ACCESS_KEY] = true
        result
      end

      def transform_symbol_keys(hash, symbol_keys)
        hash.to_h.transform_keys do |key|
          if symbol_keys.include?(key)
            key.to_sym
          else
            key
          end
        end
      end

      def convert_to_global_id_hash(argument)
        { GLOBALID_KEY => argument.to_global_id.to_s }
      rescue URI::GID::MissingModelIdError
        raise SerializationError, "Unable to serialize #{argument.class} " \
          "without an id. (Maybe you forgot to call save?)"
      end
  end

  # ActiveJob::Serializers: the registry of argument serializers, with
  # the built-in ones. `ActiveJob::Serializers.add_serializers` is where
  # an app (Core's MoneyBagJobArgumentSerializer) and a gem's railtie
  # (shopify-money's JobArgumentSerializer) register theirs.
  module Serializers
    OBJECT_SERIALIZER_KEY = Arguments::OBJECT_SERIALIZER_KEY

    @serializers = Set.new
    @serializers_index = {}

    class << self
      # Returns serialized representative of the passed object.
      # Will look up through all known serializers.
      # Raises ActiveJob::SerializationError if it can't find a proper serializer.
      def serialize(argument)
        serializer = @serializers_index[argument.class] || serializers.find { |s| s.serialize?(argument) }
        raise SerializationError.new("Unsupported argument type: #{argument.class.name}") unless serializer
        serializer.serialize(argument)
      end

      # Returns deserialized object.
      # Will look up through all known serializers.
      # If no serializer found will raise <tt>ArgumentError</tt>.
      def deserialize(argument)
        serializer_name = argument[OBJECT_SERIALIZER_KEY]
        raise ArgumentError, "Serializer name is not present in the argument: #{argument.inspect}" unless serializer_name

        serializer = begin
          Object.const_get(serializer_name)
        rescue NameError
          nil
        end
        raise ArgumentError, "Serializer #{serializer_name} is not known" unless serializer

        serializer.deserialize(argument)
      end

      # Returns list of known serializers.
      attr_reader :serializers

      def serializers=(serializers)
        @serializers_index.clear
        @serializers = Set.new
        add_new_serializers(serializers)
      end

      # Adds new serializers to a list of known serializers.
      def add_serializers(*new_serializers)
        new_serializers = new_serializers.flatten
        add_new_serializers(new_serializers)
      end

      private
        def add_new_serializers(new_serializers)
          new_serializers.map! do |s|
            if s.is_a?(Class) && s < ObjectSerializer
              s.instance
            else
              s
            end
          end

          @serializers += new_serializers
          new_serializers.each do |s|
            @serializers_index[s.klass] = s if s.respond_to?(:klass) && s.klass
          end
          @serializers
        end
    end

    # Base class for serializing and deserializing custom objects.
    # ActiveSupport's `delegate` is written out as the three methods.
    class ObjectSerializer
      include Singleton

      class << self
        def serialize?(argument) = instance.serialize?(argument)
        def serialize(argument) = instance.serialize(argument)
        def deserialize(argument) = instance.deserialize(argument)
      end

      def initialize
        super
        @template = { Serializers::OBJECT_SERIALIZER_KEY => self.class.name }.freeze
      end

      # Determines if an argument should be serialized by a serializer.
      # A `klass` the tree does not define (no ActiveSupport::Duration)
      # serializes nothing.
      def serialize?(argument)
        k = klass
        k ? argument.is_a?(k) : false
      end

      # Serializes an argument to a JSON primitive type.
      def serialize(hash)
        @template.merge(hash)
      end

      # Deserializes an argument from a JSON primitive type.
      def deserialize(hash)
        raise NotImplementedError, "#{self.class.name} should implement a public #deserialize(hash) method"
      end

      # The class of the object that will be serialized.
      def klass
        raise NotImplementedError, "#{self.class.name} should implement a public #klass method"
      end
    end

    class SymbolSerializer < ObjectSerializer # :nodoc:
      def serialize(argument)
        super("value" => argument.name)
      end

      def deserialize(argument)
        argument["value"].to_sym
      end

      def klass
        Symbol
      end
    end

    class DurationSerializer < ObjectSerializer # :nodoc:
      def serialize(duration)
        super("value" => duration.value, "parts" => Arguments.serialize(duration.parts.to_a))
      end

      def deserialize(hash)
        value = hash["value"]
        parts = Arguments.deserialize(hash["parts"].to_h)
        klass.new(value, parts.to_h)
      end

      def klass
        ActiveSupport::Duration if defined?(ActiveSupport::Duration)
      end
    end

    class TimeObjectSerializer < ObjectSerializer # :nodoc:
      NANO_PRECISION = 9

      def serialize(time)
        super("value" => time.iso8601(NANO_PRECISION))
      end
    end

    class DateTimeSerializer < TimeObjectSerializer # :nodoc:
      def deserialize(hash)
        DateTime.iso8601(hash["value"])
      end

      def klass
        DateTime
      end
    end

    class DateSerializer < ObjectSerializer # :nodoc:
      def serialize(date)
        super("value" => date.iso8601)
      end

      def deserialize(hash)
        Date.iso8601(hash["value"])
      end

      def klass
        Date
      end
    end

    class TimeWithZoneSerializer < ObjectSerializer # :nodoc:
      NANO_PRECISION = 9

      def serialize(time_with_zone)
        super(
          "value" => time_with_zone.iso8601(NANO_PRECISION),
          "time_zone" => time_with_zone.time_zone.tzinfo.name
        )
      end

      def deserialize(hash)
        Time.iso8601(hash["value"]).in_time_zone(hash["time_zone"] || Time.zone)
      end

      def klass
        ActiveSupport::TimeWithZone if defined?(ActiveSupport::TimeWithZone)
      end
    end

    class TimeSerializer < TimeObjectSerializer # :nodoc:
      def deserialize(hash)
        Time.iso8601(hash["value"])
      end

      def klass
        Time
      end
    end

    class ModuleSerializer < ObjectSerializer # :nodoc:
      def serialize(constant)
        raise SerializationError, "Serializing an anonymous class is not supported" unless constant.name
        super("value" => constant.name)
      end

      def deserialize(hash)
        Object.const_get(hash["value"])
      end

      def klass
        Module
      end
    end

    class RangeSerializer < ObjectSerializer
      def serialize(range)
        super(
          "begin" => Arguments.serialize_argument(range.begin),
          "end" => Arguments.serialize_argument(range.end),
          "exclude_end" => range.exclude_end?, # Always boolean, no need to serialize
        )
      end

      def deserialize(hash)
        Range.new(*Arguments.deserialize([hash["begin"], hash["end"]]), hash["exclude_end"])
      end

      def klass
        ::Range
      end
    end

    class BigDecimalSerializer < ObjectSerializer # :nodoc:
      def serialize(big_decimal)
        super("value" => big_decimal.to_s)
      end

      def deserialize(hash)
        BigDecimal(hash["value"])
      end

      def klass
        BigDecimal
      end
    end

    add_serializers SymbolSerializer.instance,
      DurationSerializer.instance,
      DateTimeSerializer.instance,
      DateSerializer.instance,
      TimeWithZoneSerializer.instance,
      TimeSerializer.instance,
      ModuleSerializer.instance,
      RangeSerializer.instance,
      BigDecimalSerializer.instance
  end

  # ActiveJob::ConfiguredJob (configured_job.rb): the job class with
  # `set`'s options, answered by the class-side `set`.
  class ConfiguredJob # :nodoc:
    def initialize(job_class, options = {})
      @options = options
      @job_class = job_class
    end

    def perform_now(...)
      @job_class.new(...).set(@options).perform_now
    end

    def perform_later(...)
      @job_class.new(...).enqueue @options
    end
  end

  # ActiveJob::Enqueuing.
  module Enqueuing
    module ClassMethods
      # Push a job onto the queue. By default the arguments must be either
      # String, Integer, Float, NilClass, TrueClass, FalseClass, BigDecimal,
      # Symbol, Date, Time, DateTime, ActiveSupport::TimeWithZone,
      # ActiveSupport::Duration, Hash, ActiveSupport::HashWithIndifferentAccess,
      # Array, Range, or GlobalID::Identification instances.
      #
      # Returns an instance of the job class queued with arguments available in
      # Job#arguments or +false+ if the enqueue did not succeed.
      #
      # After the attempted enqueue, the job will be yielded to an optional block.
      def perform_later(...)
        job = job_or_instantiate(...)
        enqueue_result = job.enqueue

        yield job if block_given?

        enqueue_result
      end

      private
        def job_or_instantiate(*args, **kwargs, &) # :doc:
          args.first.is_a?(self) ? args.first : new(*args, **kwargs)
        end
    end

    # Enqueues the job to be performed by the queue adapter.
    #
    # Returns an instance of the job class queued with arguments available in
    # Job#arguments or +false+ if the enqueue did not succeed.
    def enqueue(options = {})
      set(options)
      self.successfully_enqueued = false
      self.enqueue_error = nil

      raw_enqueue

      if successfully_enqueued?
        self
      else
        false
      end
    end

    private
      def raw_enqueue
        run_callbacks :enqueue do
          _raw_enqueue
        end
      end

      # The shared runtime's adapter (active_job.rb), as the generated
      # `perform_later` wrapper calls it.
      def _raw_enqueue
        job_class = self.class
        name = job_class.name
        enqueued = [arguments, job_id, executions, queue_name, @priority]
        work = -> do
          arguments, job_id, executions, queue_name, priority = enqueued
          job = job_class.new(*arguments)
          job.job_id = job_id
          job.executions = executions
          job.queue_name = queue_name
          job.priority = priority
          job.perform_now
          nil
        end
        ActiveJob.record_performed(name)
        if !ActiveJob.enqueue_only
          if ActiveJob.drain_registered
            ActiveJob.enqueue(work) unless enqueue_payload_for(name)
          else
            work.call
          end
        else
          ActiveJob.hold(name, work)
        end
        self.successfully_enqueued = true
      end

      # The payload the generated `JobRegistry` writes for this job
      # (`lower::job_payload`) in place of the Proc, when the job has a
      # plan, its arguments are the ones the plan's codecs write, and
      # nothing about the enqueue is beyond a fresh payload: a retry's
      # executions, a priority, a schedule or a queue `set` chose are
      # not in it. The drain then runs the job through `perform_now`
      # (the registry's arm), as the Proc does. Answers whether it queued.
      def enqueue_payload_for(name)
        return false unless defined?(::JobRegistry) && ::JobRegistry.respond_to?(:enqueue_arguments)
        return false unless executions == 0 && @priority.nil? && scheduled_at.nil?
        # The queue the instance the drain builds resolves; a `set(queue:)`
        # or an enqueue callback that moved this job is not in the payload.
        return false unless queue_name == self.class.new(*arguments).queue_name
        ::JobRegistry.enqueue_arguments(name, arguments, job_id)
      end
  end

  # Push many jobs onto the queue at once without running enqueue callbacks.
  # Queue adapters may communicate the enqueue status of each job by setting
  # successfully_enqueued and/or enqueue_error on the passed-in job instances.
  # (activejob/lib/active_job.rb. The shared adapter takes each job
  # through `_raw_enqueue`, in place of the per-adapter `enqueue_all`, and
  # there is no `instrument_enqueue_all`.)
  def self.perform_all_later(*jobs)
    jobs.flatten!
    jobs.each do |job|
      job.successfully_enqueued = false
      job.enqueue_error = nil
      begin
        job.__send__(:_raw_enqueue)
      rescue EnqueueError => e
        job.enqueue_error = e
      end
    end
    nil
  end

  # ActiveJob::Execution.
  module Execution
    module ClassMethods
      # Performs the job immediately.
      #
      #   MyJob.perform_now("mike")
      #
      def perform_now(...)
        job_or_instantiate(...).perform_now
      end
    end

    # Performs the job immediately. The job is not sent to the queuing adapter
    # but directly executed by blocking the execution of others until it's finished.
    # +perform_now+ returns the value of your job's +perform+ method.
    #
    #   class MyJob < ActiveJob::Base
    #     def perform
    #       "Hello World!"
    #     end
    #   end
    #
    #   puts MyJob.new(*args).perform_now # => "Hello World!"
    def perform_now
      # Guard against jobs that were persisted before we started counting executions by zeroing out nil counters
      self.executions = (executions || 0) + 1

      _perform_job
    rescue Exception => exception
      handled = rescue_with_handler(exception)
      return handled if handled

      run_after_discard_procs(exception)
      raise
    end

    def perform(*)
      fail NotImplementedError
    end

    private
      def _perform_job
        run_callbacks :perform do
          perform(*arguments)
        end
      end
  end

  # ActiveJob::QueueName.
  module QueueName
    @default_queue_name = "default"
    singleton_class.attr_reader :default_queue_name # :nodoc:

    def self.default_queue_name=(name) # :nodoc:
      @default_queue_name = -name.to_s
    end

    # The `included` block: the three class attributes' defaults.
    def self.append_features(base)
      # A module includer is a concern natively (an app concern is lowered
      # without its extend): Concern#append_features defers to its includers.
      unless base.is_a?(Class)
        base.extend(ActiveSupport::Concern) if defined?(ActiveSupport::Concern) && !base.instance_variable_defined?(:@_dependencies)
        return super unless base.instance_variable_defined?(:@_dependencies)
        base.instance_variable_get(:@_dependencies) << self
        return false
      end
      return false if base < self
      super
      base.extend ClassMethods
      base.queue_name = proc { self.class.default_queue_name }
      base.queue_name_delimiter = "_"
      base.queue_name_prefix = nil
    end

    # Includes the ability to override the default queue name and prefix.
    module ClassMethods
      def default_queue_name
        QueueName.default_queue_name
      end

      def default_queue_name=(name)
        QueueName.default_queue_name = name
      end

      # Specifies the name of the queue to process the job on.
      #
      #   class PublishToFeedJob < ActiveJob::Base
      #     queue_as :feeds
      #
      #     def perform(post)
      #       post.to_feed!
      #     end
      #   end
      #
      # Can be given a block that will evaluate in the context of the job
      # so that a dynamic queue name can be applied:
      #
      #   class PublishToFeedJob < ApplicationJob
      #     queue_as do
      #       post = self.arguments.first
      #
      #       if post.paid?
      #         :paid_feeds
      #       else
      #         :feeds
      #       end
      #     end
      #
      #     def perform(post)
      #       post.to_feed!
      #     end
      #   end
      def queue_as(part_name = nil, &block)
        if block_given?
          self.queue_name = block
        else
          self.queue_name = queue_name_from_part(part_name)
        end
      end

      def queue_name_from_part(part_name) # :nodoc:
        queue_name = part_name || default_queue_name
        name_parts = [QueueName.presence(queue_name_prefix), queue_name]
        -name_parts.compact.join(queue_name_delimiter)
      end

      # The class_attribute readers (`instance_accessor: false` for the
      # first two): the value the nearest class that assigned one holds.
      def queue_name
        QueueName.inherited_value(self, :@queue_name)
      end

      def queue_name=(value)
        @queue_name = value
      end

      def queue_name?
        !!queue_name
      end

      def queue_name_delimiter
        QueueName.inherited_value(self, :@queue_name_delimiter)
      end

      def queue_name_delimiter=(value)
        @queue_name_delimiter = value
      end

      def queue_name_delimiter?
        !!queue_name_delimiter
      end

      def queue_name_prefix
        QueueName.inherited_value(self, :@queue_name_prefix)
      end

      def queue_name_prefix=(value)
        @queue_name_prefix = value
      end

      def queue_name_prefix?
        !!queue_name_prefix
      end
    end

    # `class_attribute :queue_name_prefix`'s instance reader, writer and
    # predicate.
    def queue_name_prefix
      defined?(@queue_name_prefix) ? @queue_name_prefix : self.class.queue_name_prefix
    end

    def queue_name_prefix=(value)
      @queue_name_prefix = value
    end

    def queue_name_prefix?
      !!queue_name_prefix
    end

    # Returns the name of the queue the job will be run on.
    def queue_name
      if @queue_name.is_a?(Proc)
        @queue_name = self.class.queue_name_from_part(instance_exec(&@queue_name))
      end
      @queue_name
    end

    def self.inherited_value(klass, ivar) # :nodoc:
      owner = klass
      owner = owner.superclass until owner.instance_variable_defined?(ivar)
      owner.instance_variable_get(ivar)
    end

    # `Object#presence` (active_support/core_ext/object/blank.rb): a
    # String is blank when it is all whitespace, anything else that has
    # `empty?` when it is empty, the rest when it is falsy.
    def self.presence(value) # :nodoc:
      blank =
        if value.is_a?(String)
          value.match?(/\A[[:space:]]*\z/)
        elsif value.respond_to?(:empty?)
          value.empty?
        else
          !value
        end
      blank ? nil : value
    end
  end

  # ActiveJob::Exceptions: `discard_on` and `after_discard` on the
  # Rescuable port, and `retry_job`.
  module Exceptions
    # The `included` block: the two class attributes' defaults.
    def self.append_features(base)
      # A module includer is a concern natively (an app concern is lowered
      # without its extend): Concern#append_features defers to its includers.
      unless base.is_a?(Class)
        base.extend(ActiveSupport::Concern) if defined?(ActiveSupport::Concern) && !base.instance_variable_defined?(:@_dependencies)
        return super unless base.instance_variable_defined?(:@_dependencies)
        base.instance_variable_get(:@_dependencies) << self
        return false
      end
      return false if base < self
      super
      base.extend ClassMethods
      base.retry_jitter = 0.0
      base.after_discard_procs = []
    end

    module ClassMethods
      # Discard the job with no attempts to retry, if the exception is raised. This is useful when the subject of the job,
      # like an Active Record, is no longer available, and the job is thus no longer relevant.
      #
      # Passing the <tt>:report</tt> option reports the error through the error reporter before discarding the job.
      #
      # You can also pass a block that'll be invoked. This block is yielded with the job instance as the first and the error instance as the second parameter.
      #
      # +retry_on+ and +discard_on+ handlers are searched from bottom to top, and up the class hierarchy. The handler of the first class for
      # which <tt>exception.is_a?(klass)</tt> holds true is the one invoked, if any.
      def discard_on(*exceptions, report: false)
        rescue_from(*exceptions) do |error|
          ActiveSupport.error_reporter.report(error, source: "application.active_job") if report
          yield self, error if block_given?
          run_after_discard_procs(error)
        end
      end

      # A block to run when a job is about to be discarded for any reason.
      def after_discard(&blk)
        self.after_discard_procs += [blk]
      end

      # The class_attribute readers (`instance_accessor: false` for
      # `retry_jitter`): the value the nearest class that assigned one
      # holds.
      def retry_jitter
        QueueName.inherited_value(self, :@retry_jitter)
      end

      def retry_jitter=(value)
        @retry_jitter = value
      end

      def retry_jitter?
        !!retry_jitter
      end

      def after_discard_procs
        QueueName.inherited_value(self, :@after_discard_procs)
      end

      def after_discard_procs=(value)
        @after_discard_procs = value
      end

      def after_discard_procs?
        !!after_discard_procs
      end
    end

    # `class_attribute :after_discard_procs`'s instance reader, writer and
    # predicate.
    def after_discard_procs
      defined?(@after_discard_procs) ? @after_discard_procs : self.class.after_discard_procs
    end

    def after_discard_procs=(value)
      @after_discard_procs = value
    end

    def after_discard_procs?
      !!after_discard_procs
    end

    # Reschedules the job to be re-executed. This is useful in combination with
    # {rescue_from}[rdoc-ref:ActiveSupport::Rescuable::ClassMethods#rescue_from].
    # When you rescue an exception from your job you can ask Active Job to retry
    # performing your job.
    def retry_job(options = {})
      scheduled_at, queue_name = self.scheduled_at, self.queue_name
      enqueue options
    ensure
      self.scheduled_at, self.queue_name = scheduled_at, queue_name
    end

    private
      def run_after_discard_procs(exception)
        exceptions = []
        after_discard_procs.each do |blk|
          instance_exec(self, exception, &blk)
        rescue StandardError => e
          exceptions << e
        end
        raise exceptions.last unless exceptions.empty?
      end
  end

  module Callbacks
    # ActiveJob::Base's includes, Concern#append_features's way: the
    # dependencies first, then the module, its class methods and its
    # `included` block.
    def self.append_features(base)
      # A module includer is a concern natively (an app concern is lowered
      # without its extend): Concern#append_features defers to its includers.
      unless base.is_a?(Class)
        base.extend(ActiveSupport::Concern) if defined?(ActiveSupport::Concern) && !base.instance_variable_defined?(:@_dependencies)
        return super unless base.instance_variable_defined?(:@_dependencies)
        base.instance_variable_get(:@_dependencies) << self
        return false
      end
      return false if base < self
      base.include Core
      base.extend Core::ClassMethods
      base.include QueueName
      base.include Enqueuing
      base.extend Enqueuing::ClassMethods
      base.include ActiveSupport::Rescuable
      base.include Execution
      base.extend Execution::ClassMethods
      base.include ActiveSupport::Callbacks
      super
      base.extend ClassMethods
      base.define_callbacks :perform, skip_after_callbacks_if_terminated: true
      base.define_callbacks :enqueue, skip_after_callbacks_if_terminated: true
      base.include Exceptions
    end

    # These methods will be included into any Active Job object, adding
    # callbacks for +perform+ and +enqueue+ methods.
    module ClassMethods
      # Defines a callback that will get called right before the
      # job's perform method is executed.
      def before_perform(*filters, &blk)
        set_callback(:perform, :before, *filters, &blk)
      end

      # Defines a callback that will get called right after the
      # job's perform method has finished.
      def after_perform(*filters, &blk)
        set_callback(:perform, :after, *filters, &blk)
      end

      # Defines a callback that will get called around the job's perform method.
      #
      # You can access the return value of the job only if the execution wasn't halted.
      def around_perform(*filters, &blk)
        set_callback(:perform, :around, *filters, &blk)
      end

      # Defines a callback that will get called right before the
      # job is enqueued.
      def before_enqueue(*filters, &blk)
        set_callback(:enqueue, :before, *filters, &blk)
      end

      # Defines a callback that will get called right after the
      # job is enqueued.
      def after_enqueue(*filters, &blk)
        set_callback(:enqueue, :after, *filters, &blk)
      end

      # Defines a callback that will get called around the enqueuing
      # of the job.
      def around_enqueue(*filters, &blk)
        set_callback(:enqueue, :around, *filters, &blk)
      end
    end
  end
end
