# frozen_string_literal: true
# What ActionDispatch::Integration::Runner, ActiveJob::TestHelper and
# ActiveSupport::TestCase put on a test instance that the runtime test
# helper (test/test_helper.rb) has not modeled, for the CRuby tree.
# Loaded with the request encoder (project::test_case_load_hooks_block).

# `integration_session` (Runner): the test's browser. Rails' `get` is the
# session's, and the session is an object of its own whose `process` every
# verb goes through; a helper wraps it (`def integration_session; session =
# super; session.extend(EdgeStapledIntegrationSession); …`) and the wrapper's
# `process` sees each request. The runtime's integration test keeps the
# browser's state (cookie jar, host, last response) on itself, so the
# session here is a handle on the test: `process` runs the test's own verb,
# anything else the test answers. The test's verbs go through the handle,
# so a module extended onto it sees the test's requests, and a module
# prepended onto the TEST CLASS (Core's `ProcessWithHtmlCheck`, which
# natively never sees a request: the session's `process` is not the
# test's) stays out of them.
class RoundhouseIntegrationSession
  VERBS = %i[get post patch put delete head].freeze

  def initialize(test)
    @test = test
  end

  def process(method, path, **options)
    @test.__roundhouse_dispatch(method.to_s.downcase.to_sym, path, **options)
  end

  VERBS.each do |verb|
    define_method(verb) { |path, **options| process(verb, path, **options) }
  end

  def method_missing(name, *args, **options, &block)
    @test.respond_to?(name, true) ? @test.__send__(name, *args, **options, &block) : super
  end

  def respond_to_missing?(name, include_private = false)
    @test.respond_to?(name, include_private) || super
  end
end

module ActionDispatch
  class IntegrationTest
    def integration_session
      @__integration_session ||= RoundhouseIntegrationSession.new(self)
    end
  end
end

module RequestDispatch
  module ProcessEntry
    RoundhouseIntegrationSession::VERBS.each do |verb|
      define_method(verb) do |path, **options|
        return super(path, **options) if @__dispatching_verb || !respond_to?(:integration_session)
        integration_session.process(verb, path, **options)
      end
    end

    def __roundhouse_dispatch(verb, path, **options)
      outer = @__dispatching_verb
      @__dispatching_verb = true
      public_send(verb, path, **options)
    ensure
      @__dispatching_verb = outer
    end
  end

  # rack-test's `header(name, value)` on the session: a header every later
  # request of this browser carries, `nil` removing it. An explicit
  # `headers:` of a request wins.
  module DefaultRequestHeaders
    def header(name, value)
      @__default_request_headers ||= {}
      if value.nil?
        @__default_request_headers.delete(name.to_s)
      else
        @__default_request_headers[name.to_s] = value
      end
      nil
    end

    def dispatch_request(method, path, params, headers = {}, as = nil)
      defaults = @__default_request_headers
      headers = defaults.merge(headers) unless defaults.nil? || defaults.empty?
      super(method, path, params, headers, as)
    end

    def reset!
      @__default_request_headers = nil
      @__integration_session = nil
      super
    end
  end

  prepend ProcessEntry
  prepend DefaultRequestHeaders
end

# ActiveSupport::TestCase#method_name: Minitest's `name`, the test method.
class TestBase
  def method_name
    name
  end
end

# ActiveJob::TestHelper#enqueued_jobs / #performed_jobs: the jobs the
# `:test` adapter holds, each the hash its `job_to_hash` builds
# (`job.serialize` plus `:job`, `:args`, `:queue`, `:priority`, and `:at`
# for a scheduled one). The suite's adapter is the runtime's own queue
# (ActiveJob.enqueue_only holds a job; otherwise it runs at the call
# site, which is what `performed_jobs` lists), so each enqueue is recorded
# here, on the running test, as it is made.
module RoundhouseEnqueuedJobs
  module Recorder
    private

    def _raw_enqueue
      test = Thread.current[:roundhouse_current_test]
      data = test && RoundhouseEnqueuedJobs.snapshot(self)
      held = ActiveJob.enqueue_only
      result = super
      if data && successfully_enqueued?
        (held ? test.enqueued_jobs : test.performed_jobs) << data
      end
      result
    end
  end

  # An argument the serializer refuses is the enqueue's own error to raise;
  # the record is skipped rather than raising here.
  def self.snapshot(job)
    data = job.serialize
    data[:job] = job.class
    data[:args] = data.fetch("arguments")
    data[:queue] = data.fetch("queue_name")
    data[:priority] = data.fetch("priority")
    data[:at] = job.scheduled_at.to_f if job.scheduled_at
    data
  rescue StandardError
    nil
  end

  module TestHelper
    def enqueued_jobs
      @__roundhouse_enqueued_jobs ||= []
    end

    def performed_jobs
      @__roundhouse_performed_jobs ||= []
    end

    def clear_enqueued_jobs
      enqueued_jobs.clear
      super
    end

    # Without a block the held jobs run now: they move from the enqueued
    # list to the performed one.
    def perform_enqueued_jobs(only: [], &block)
      if block.nil?
        names = Array(only).map(&:to_s)
        ran, held = enqueued_jobs.partition { |job| names.empty? || names.include?(job["job_class"]) }
        enqueued_jobs.replace(held)
        performed_jobs.concat(ran)
      end
      super(only: only, &block)
    end
  end
end

ActiveJob::Enqueuing.prepend(RoundhouseEnqueuedJobs::Recorder) if defined?(ActiveJob::Enqueuing)
ActiveJob::TestHelper.prepend(RoundhouseEnqueuedJobs::TestHelper)
