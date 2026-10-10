# Test support the CRuby tree can give and the spinel tree cannot:
# loaded last by test/test_helper.rb on the ruby target only
# (`project.rs::apply_cruby_test_support`).
#
# Rails' `ActiveSupport::Testing::ConstantStubbing`
# (activesupport/lib/active_support/testing/constant_stubbing.rb), as
# Rails writes it: the constant is REPLACED for the block and put back
# after, so code that reads it at call time (`Message::Pagination::
# PAGE_SIZE`, `Opengraph::Fetch::TIMEOUT`) sees the stub; `exists: false`
# defines one for the block. ActiveSupport::TestCase includes it, and app
# test code names it too (Core's AdsAccounting::LedgerTestHelper:
# `include(ActiveSupport::Testing::ConstantStubbing)`). A compiled tree
# resolves constants at build time and has no `const_set`; there the
# helper is absent and the call fails as the gap it is.
module ActiveSupport
  module Testing
    module ConstantStubbing
      def stub_const(mod, constant, new_value, exists: true)
        if exists
          begin
            old_value = mod.const_get(constant, false)
            mod.send(:remove_const, constant)
            mod.const_set(constant, new_value)
            yield
          ensure
            mod.send(:remove_const, constant)
            mod.const_set(constant, old_value)
          end
        else
          if mod.const_defined?(constant)
            raise NameError, "already defined constant #{constant} in #{mod.name}"
          end

          begin
            mod.const_set(constant, new_value)
            yield
          ensure
            mod.send(:remove_const, constant)
          end
        end
      end
    end
  end
end

class TestBase
  include ActiveSupport::Testing::ConstantStubbing
end

# Rails' `ActionMailer::TestHelper` (actionmailer/lib/action_mailer/
# test_helper.rb), where the tree has the mailer runtime: app test code
# includes it (Core's SendAuthenticationMailJobTest: `include
# ActionMailer::TestHelper`, then `assert_emails(1) { … }`). Rails'
# `assert_emails` / `assert_no_emails` / `capture_emails` as Rails
# writes them, over `ActionMailer::Base.deliveries`. The enqueued-mail
# assertions stay TestBase's (test/test_helper.rb), which a test class's
# include does not shadow. DIVERGENCE, stated: Rails'
# `deliver_enqueued_emails` performs the mail delivery jobs the block
# enqueued; runtime/action_mailer.rb has no queue (`deliver_later`
# delivers as it is called), so it runs the block, and the mail the
# block sent is already delivered.
if defined?(ActionMailer::Base)
  module ActionMailer
    module TestHelper
      def assert_emails(number, &block)
        if block_given?
          diff = capture_emails(&block).length
          assert_equal number, diff, "#{number} emails expected, but #{diff} were sent"
        else
          assert_equal number, ActionMailer::Base.deliveries.size
        end
      end

      def assert_no_emails(&block)
        assert_emails 0, &block
      end

      def deliver_enqueued_emails(queue: nil, at: nil, &block)
        block&.call
      end

      def capture_emails(&block)
        original_count = ActionMailer::Base.deliveries.size
        deliver_enqueued_emails(&block)
        new_count = ActionMailer::Base.deliveries.size
        diff = new_count - original_count
        ActionMailer::Base.deliveries.last(diff)
      end
    end

    # Rails' `ActionMailer::TestCase < ActiveSupport::TestCase`
    # (actionmailer/lib/action_mailer/test_case.rb) and its
    # `ClearTestDeliveries`, which Core's ActiveSupport::TestCase includes
    # (app/models/active_support/test_case.rb). Core reopens the class
    # without a superclass, so this defines it first. Rails' `included`
    # block registers `clear_test_deliveries` as setup and teardown.
    # DIVERGENCE, stated: Rails clears only when `delivery_method == :test`;
    # runtime/action_mailer.rb has no delivery methods and always collects
    # into `deliveries`, which is the :test behavior.
    unless const_defined?(:TestCase, false)
      class TestCase < (defined?(ActiveSupport::TestCase) ? ActiveSupport::TestCase : TestBase)
      end
    end

    class TestCase
      module ClearTestDeliveries
        def self.included(base)
          base.setup :clear_test_deliveries
          base.teardown :clear_test_deliveries
        end

        private
          def clear_test_deliveries
            ActionMailer::Base.deliveries.clear
          end
      end
    end
  end
end

# Rails' `ActiveJob::QueueAdapters::TestAdapter` and its
# `AbstractAdapter` (activejob/lib/active_job/queue_adapters/
# test_adapter.rb, abstract_adapter.rb), as Rails writes them: app test
# support subclasses the test adapter (Core's `class HedwigTestAdapter <
# ActiveJob::QueueAdapters::TestAdapter`, required by the
# active_support_test_case initializer before it reaches `require
# "active_support/testing/let"`). The class is Rails'; this tree's job
# assertions run over the runtime's own queue log (TestBase's
# ActiveJob::TestHelper), and nothing here installs an adapter
# instance as the queue (`queue_adapter_for_test` is not called).
module ActiveJob
  module QueueAdapters
    class AbstractAdapter
      attr_accessor :stopping

      def enqueue(job)
        raise NotImplementedError
      end

      def enqueue_at(job, timestamp)
        raise NotImplementedError
      end

      def stopping?(job = nil)
        !!@stopping
      end
    end

    class TestAdapter < AbstractAdapter
      attr_accessor(:perform_enqueued_jobs, :perform_enqueued_at_jobs, :filter, :reject, :queue, :at, :stopping)
      attr_writer(:enqueued_jobs, :performed_jobs)

      def enqueued_jobs
        @enqueued_jobs ||= []
      end

      def performed_jobs
        @performed_jobs ||= []
      end

      def enqueue(job) # :nodoc:
        job_data = job_to_hash(job)
        perform_or_enqueue(perform_enqueued_jobs && !filtered?(job), job, job_data)
      end

      def enqueue_at(job, timestamp) # :nodoc:
        job_data = job_to_hash(job, at: timestamp)
        perform_or_enqueue(perform_enqueued_at_jobs && !filtered?(job), job, job_data)
      end

      def stopping?(job = nil)
        @stopping.is_a?(Proc) ? @stopping.call(job) : @stopping
      end

      private
        def job_to_hash(job, extras = {})
          job.serialize.tap do |job_data|
            job_data[:job] = job.class
            job_data[:args] = job_data.fetch("arguments")
            job_data[:queue] = job_data.fetch("queue_name")
            job_data[:priority] = job_data.fetch("priority")
          end.merge(extras)
        end

        def perform_or_enqueue(perform, job, job_data)
          if perform
            performed_jobs << job_data
            Base.execute(job.serialize)
          else
            enqueued_jobs << job_data
          end
        end

        def filtered?(job)
          filtered_queue?(job) || filtered_job_class?(job) || filtered_time?(job)
        end

        def filtered_time?(job)
          job.scheduled_at > at if at && job.scheduled_at
        end

        def filtered_queue?(job)
          if queue
            job.queue_name != queue.to_s
          end
        end

        def filtered_job_class?(job)
          if filter
            !filter_as_proc(filter).call(job)
          elsif reject
            filter_as_proc(reject).call(job)
          end
        end

        def filter_as_proc(filter)
          return filter if filter.is_a?(Proc)

          ->(job) { Array(filter).include?(job.class) }
        end
    end
  end
end

# Rails-dom-testing's `css_select`: the elements of the last response
# matching `selector`, as Nokogiri nodes (`node["id"]`). Rails' own is
# Nokogiri; the shared harness's `Dom` is a scanner that matches a
# selector's LAST compound and keeps no per-element attributes, so a
# descendant selector read from it would answer for the whole page.
require "nokogiri"

class TestBase
  def css_select(selector)
    Nokogiri::HTML5.fragment(@__response.body.to_s).css(selector).to_a
  end
end

# ActionCable::TestHelper#assert_broadcast_on: some broadcast to
# `stream` — during the block, or so far in this test without one —
# carries `data`, compared as Rails compares it, after a JSON round
# trip (symbol keys and all become what the wire carries).
module ActionCable
  module TestHelper
    def assert_broadcast_on(stream, data, &block)
      entries =
        if block
          capture_broadcasts_on(stream, &block)
        else
          Broadcasts.log.select { |entry| entry[:stream] == stream }
        end
      # The CRuby tree logs the Hash it publishes, the spinel tree the
      # JSON text; both compare after the same round trip.
      wire = ->(value) { ::JSON.parse(value.is_a?(String) ? value : ::JSON.generate(value)) }
      want = wire.(data)
      payloads = entries.map { |entry| entry[:payload] }.compact
      return if payloads.any? { |payload| wire.(payload) == want }
      raise "assert_broadcast_on failed: no broadcast to #{stream.inspect} carried #{want.inspect}; " \
            "got #{payloads.inspect}"
    end
  end
end
