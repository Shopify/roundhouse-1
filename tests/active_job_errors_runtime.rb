# Native Rails oracle (the Rails native Core runs; tests/support/
# rails_oracle.rb) for Active Job's error classes against
# runtime/spinel/active_job_errors.rb.
#
#   ruby tests/active_job_errors_runtime.rb compare
#
# runs this script twice: `native` requires the pinned activejob;
# `emitted` loads runtime/ruby/active_job.rb and the port as the emitted
# PostgreSQL tree's boot.rb does. Each prints one line per observation
# over Core's uses (Hedwig's `JobEnqueueError < ActiveJob::EnqueueError`,
# `raise ActiveJob::EnqueueError, "queue down"`, the unknown-job-class
# fingerprinter's `exception.name`, `ActiveJob::DeserializationError.include`,
# `assert_raises(ActiveJob::SerializationError)`); `compare` requires
# them equal.
require_relative "support/rails_oracle"

if ARGV.first == "compare"
  require "open3"
  outputs = ["native", "emitted"].map do |mode|
    env = mode == "emitted" ? RailsOracle.core_gems_env : {}
    stdout, stderr, status = Open3.capture3(env, RbConfig.ruby, File.expand_path(__FILE__), mode)
    abort "#{mode}: #{stderr}" unless status.success?
    stdout
  end
  native, emitted = outputs.map(&:lines)
  equal = native.zip(emitted).count { |x, y| x == y }
  unless outputs.uniq.length == 1
    diff = native.zip(emitted).each_with_index.reject { |(x, y), _| x == y }.first(8)
    abort "Active Job errors differ (#{equal}/#{native.length} observations equal, emitted has #{emitted.length}):\n" +
      diff.map { |(x, y), i| "line #{i + 1}\n  native:  #{x}  emitted: #{y}" }.join("\n")
  end
  puts outputs.first
  puts "observations: #{equal}/#{native.length} equal"
  puts "Native Rails and emitted Active Job errors agree (#{RailsOracle.label})"
  exit
end

MODE = ARGV.first
abort "usage: #{$PROGRAM_NAME} compare | native | emitted" unless %w[native emitted].include?(MODE)

if MODE == "native"
  RailsOracle.activate("activejob")
  require "active_job"
  # Autoloaded in Rails; Core reaches each by name.
  ActiveJob::Arguments
  ActiveJob::Base
else
  require_relative "../runtime/ruby/active_job"
  require_relative "../runtime/spinel/active_job_errors"
end

def show(label, value)
  puts "#{label}: #{value.inspect}"
end

def outcome
  yield.inspect
rescue Exception => e
  "#{e.class}: #{e.message.inspect}"
end

ERRORS = [
  ActiveJob::EnqueueError,
  ActiveJob::UnknownJobClassError,
  ActiveJob::DeserializationError,
  ActiveJob::DeserializationError::RecordNotFound,
  ActiveJob::SerializationError,
].freeze

ERRORS.each do |error|
  show("#{error.name} ancestors", error.ancestors.take_while { |a| a != Object }.reject { |a| a.name.nil? }.map(&:name))
  show("#{error.name} superclass", error.superclass.name)
  show("#{error.name} instance_methods", error.instance_methods(false).sort)
  show("#{error.name} arity", error.instance_method(:initialize).arity)
end

# hedwig: `class JobEnqueueError < ActiveJob::EnqueueError`.
module Hedwig
  class JobEnqueueError < ActiveJob::EnqueueError; end
end
show("hedwig ancestors", Hedwig::JobEnqueueError.ancestors.take(3).map(&:name))
show("hedwig rescued as EnqueueError", outcome { begin; raise Hedwig::JobEnqueueError, "redis"; rescue ActiveJob::EnqueueError => e; [e.class.name, e.message]; end })

# EnqueueError: bare, with a message, `.new(message)`, raised and rescued.
show("enqueue bare", outcome { raise ActiveJob::EnqueueError })
show("enqueue message", outcome { raise ActiveJob::EnqueueError, "queue down" })
show("enqueue new", ActiveJob::EnqueueError.new("queue unavailable").message)
show("enqueue full_message", ActiveJob::EnqueueError.new("queue unavailable").full_message(highlight: false))
show("enqueue rescued as StandardError", outcome { begin; raise ActiveJob::EnqueueError, "x"; rescue StandardError => e; e.class.name; end })
show("enqueue name", ActiveJob::EnqueueError.name)
show("enqueue inspect", ActiveJob::EnqueueError.new("q").inspect)

# UnknownJobClassError: the fingerprinter reads `name`.
unknown = ActiveJob::UnknownJobClassError.new("SomeUnknownJob")
show("unknown message", unknown.message)
show("unknown name", unknown.name)
show("unknown receiver", outcome { unknown.receiver })
show("unknown key", outcome { unknown.key }) if unknown.respond_to?(:key)
show("unknown is NameError", unknown.is_a?(NameError))
show("unknown is StandardError", unknown.is_a?(StandardError))
show("unknown rescued as NameError", outcome { begin; raise unknown; rescue NameError => e; [e.class.name, e.name, e.message]; end })
show("unknown no argument", outcome { ActiveJob::UnknownJobClassError.new })
show("unknown two arguments", outcome { ActiveJob::UnknownJobClassError.new("A", "B") })
show("unknown grouping", "#{unknown.class.name}:#{unknown.name}")

# DeserializationError wraps `$!`.
error = begin
  begin
    raise KeyError, "missing record"
  rescue KeyError
    raise ActiveJob::DeserializationError
  end
rescue ActiveJob::DeserializationError => e
  e
end
show("deserialization message", error.message)
show("deserialization cause", [error.cause.class.name, error.cause.message])
show("deserialization backtrace copied", error.backtrace == error.cause.backtrace)
show("deserialization outside rescue", outcome { ActiveJob::DeserializationError.new })
record_error = begin
  begin
    raise ArgumentError, "Couldn't find Shop with 'id'=1"
  rescue ArgumentError
    raise ActiveJob::DeserializationError::RecordNotFound
  end
rescue ActiveJob::DeserializationError => e
  e
end
show("record not found", [record_error.class.name, record_error.message, record_error.cause.class.name])

# job_error_attribution.rb: `ActiveJob::DeserializationError.include(FromRunningJobClass)`.
module FromRunningJobClass
  def job_class
    "Probe#{self.class.name.split("::").last}"
  end
end
ActiveJob::DeserializationError.include(FromRunningJobClass)
show("included", [ActiveJob::DeserializationError.include?(FromRunningJobClass), record_error.job_class])

# SerializationError: `assert_raises(ActiveJob::SerializationError)`.
show("serialization", outcome { raise ActiveJob::SerializationError, "Unsupported argument type: Object" })
show("serialization is ArgumentError", outcome { begin; raise ActiveJob::SerializationError; rescue ArgumentError => e; [e.class.name, e.message]; end })
