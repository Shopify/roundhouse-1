# frozen_string_literal: true

# Active Job's error classes (Shopify/rails 5813559f4aed, 8.2.0.alpha),
# verbatim: activejob/lib/active_job/enqueuing.rb (EnqueueError),
# core.rb (UnknownJobClassError) and arguments.rb (DeserializationError,
# its RecordNotFound, SerializationError). `require "active_job"` defines
# them before any app code; the app's and gems' code subclasses them
# (`class JobEnqueueError < ActiveJob::EnqueueError`), rescues and raises
# them, and reads `UnknownJobClassError#name`.
#
# The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`);
# runtime/ruby/active_job.rb, which every target shares, keeps its
# strict-target ActiveJob::Base.
module ActiveJob
  # Can be raised by adapters if they wish to communicate to the caller a reason
  # why the adapter was unexpectedly unable to enqueue a job.
  class EnqueueError < StandardError; end

  # Raised during job payload deserialization when it references an uninitialized job class.
  class UnknownJobClassError < NameError
    def initialize(job_class_name)
      super("Failed to instantiate job, class `#{job_class_name}` doesn't exist", job_class_name)
    end
  end

  # Raised when an exception is raised during job arguments deserialization.
  #
  # Wraps the original exception raised as +cause+.
  class DeserializationError < StandardError
    def initialize # :nodoc:
      super("Error while trying to deserialize arguments: #{$!.message}")
      set_backtrace $!.backtrace
    end

    # Raised when arguments couldn't be deserialized because a record they
    # reference through a Global ID no longer exists, most likely because it
    # was deleted after the job was enqueued.
    class RecordNotFound < DeserializationError
    end
  end

  # Raised when an unsupported argument type is set as a job argument, a
  # Hash key is something else than a string or a symbol, or an object
  # can't be identified with a GlobalID.
  class SerializationError < ArgumentError; end
end
