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
#   zone, the singleton `:execute` chain around `Base.execute`, argument
#   serialization (the arguments are passed as they are),
#   `ActiveSupport::ExecutionContext[:job]`, and the priority. The
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

require "securerandom"
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
    end

    # Configures the job with the given options.
    def set(options = {}) # :nodoc:
      self.scheduled_at = Time.now + options[:wait].to_f if options[:wait]
      self.scheduled_at = options[:wait_until] if options[:wait_until]
      self.queue_name   = self.class.queue_name_from_part(options[:queue]) if options[:queue]
      self
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
        enqueued = [arguments, job_id, executions, queue_name]
        work = -> do
          arguments, job_id, executions, queue_name = enqueued
          job = job_class.new(*arguments)
          job.job_id = job_id
          job.executions = executions
          job.queue_name = queue_name
          job.perform_now
          nil
        end
        ActiveJob.record_performed(name)
        if !ActiveJob.enqueue_only
          if ActiveJob.drain_registered
            ActiveJob.enqueue(work)
          else
            work.call
          end
        else
          ActiveJob.hold(name, work)
        end
        self.successfully_enqueued = true
      end
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
      return false if base < self
      base.include Core
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
