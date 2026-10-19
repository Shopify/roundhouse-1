# frozen_string_literal: true
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
  # The shared harness defines a refusing `stub_const` on TestBase itself
  # (for the sites a compiled tree cannot stub); a class's own method
  # outranks an included module's, so drop it for the real one.
  remove_method :stub_const if instance_methods(false).include?(:stub_const)
  include ActiveSupport::Testing::ConstantStubbing

  # Minitest::Runnable.methods_matching (minitest/lib/minitest.rb):
  # the public instance method names, inherited ones included, that
  # match `re`. Core's GraphQLTestHelper reads the class's own test
  # names with it (`methods_matching(/^test_/).grep(/preload/)`).
  def self.methods_matching(re)
    public_instance_methods(true).grep(re).map(&:to_s)
  end
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

  # Rails' `TestHelper#after_teardown` resets the test adapter, which
  # forgets the job classes a test took out of it
  # (`disable_test_adapter`; runtime/active_job_callbacks.rb).
  module TestHelper
    def after_teardown
      super if defined?(super)
      ActiveJob::QueueAdapter.reset if defined?(ActiveJob::QueueAdapter)
    end
  end
end

# Rails-dom-testing's `css_select`: the elements of the last response
# matching `selector`, as Nokogiri nodes (`node["id"]`). Rails' own is
# Nokogiri; the shared harness's `Dom` is a scanner that matches a
# selector's LAST compound and keeps no per-element attributes, so a
# descendant selector read from it would answer for the whole page.
# A spinel tree that carries spinel-nokogiri gets the same helper
# (`project::SPINEL_CSS_SELECT`).
require "nokogiri"

# rails-dom-testing parses with HTML5 where Nokogiri has the parser and
# HTML4 where it does not, which is JRuby.
HTML_FRAGMENT_PARSER = defined?(Nokogiri::HTML5) ? Nokogiri::HTML5 : Nokogiri::HTML4

class TestBase
  def css_select(selector)
    HTML_FRAGMENT_PARSER.fragment(@__response.body.to_s).css(selector).to_a
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
      # The CRuby tree logs the Hash it publishes; the shared harness's
      # version, which this replaces, reads the spinel tree's JSON text.
      # Both compare after the same round trip.
      wire = ->(value) { ::JSON.parse(value.is_a?(String) ? value : ::JSON.generate(value)) }
      want = wire.(data)
      payloads = entries.map { |entry| entry[:payload] }.compact
      return if payloads.any? { |payload| wire.(payload) == want }
      raise "assert_broadcast_on failed: no broadcast to #{stream.inspect} carried #{want.inspect}; " \
            "got #{payloads.inspect}"
    end
  end
end

# `ActiveSupport::Notifications.subscribe(pattern) { |name, start,
# finish, id, payload| … }` / `unsubscribe(subscriber)` for the cache
# events, `cache_read` / `cache_write.active_support` with the key in
# the payload, which campfire's caching test reads to learn which
# fragments were cached. The store's typed seam (`read_str` /
# `write_str`, every view `cache` and `cached: true` render) reports
# them while anyone listens. The keys are Rails' shape
# (`views/messages/_message/messages/4-…`), template path included.
# A pattern is a String (the exact name) or a Regexp, as in Rails.
# The spinel tree has no subscriber list; there the call is the gap.
# A tree carrying the ActiveSupport::Notifications port
# (runtime/active_support_notifications.rb, which the cache port and log
# subscribers require) keeps it: its `instrument` yields the block and
# answers its value, which this list's does not.
unless defined?(ActiveSupport::Notifications::Event)
  module ActiveSupport
    module Notifications
      @subscribers = []

      def self.subscribe(pattern, callback = nil, &block)
        subscriber = [pattern, callback || block]
        @subscribers << subscriber
        subscriber
      end

      def self.unsubscribe(subscriber)
        @subscribers.delete(subscriber)
        nil
      end

      def self.listening?(name)
        @subscribers.any? { |pattern, _| pattern === name }
      end

      # A one-argument callback gets an `Event`, as Rails hands one to a
      # subscriber whose block or callable takes one (`->(event) {
      # event.payload[:hits] }`); any other arity gets the five arguments.
      def self.instrument(name, payload)
        return unless listening?(name)
        now = Time.now
        @subscribers.dup.each do |pattern, callback|
          next unless pattern === name
          if callback.arity == 1
            callback.call(Event.new(name, now, now, nil, payload))
          else
            callback.call(name, now, now, nil, payload)
          end
        end
      end

      class Event
        attr_reader :name, :time, :end, :transaction_id, :payload

        def initialize(name, start, ending, transaction_id, payload)
          @name = name
          @time = start
          @end = ending
          @transaction_id = transaction_id
          @payload = payload
        end
      end

      # `subscribed(callback, name) { … }` for every event this file
      # reports, not just SQL: subscribed for the block, unsubscribed
      # after, as Rails does. SQL keeps the shared harness's capture,
      # which reads every statement and is not an instrument call here.
      class << self
        alias_method :__rh_shared_subscribed, :subscribed

        def subscribed(callback, name, &block)
          return __rh_shared_subscribed(callback, name, &block) if name == "sql.active_record"
          subscriber = subscribe(name, callback)
          begin
            block.call
          ensure
            unsubscribe(subscriber)
          end
        end
      end
    end
  end
end

# ActiveSupport's own store reports what Rails' does: `cache_read` with
# the normalized key and `hit`, `cache_write`, and `cache_read_multi`
# with the keys asked for and the ones found (`hits`) — the events
# campfire's fragment tests subscribe to. A fetch is a read, then on a
# miss a write, as Rails instruments it.
#
# Only a store that does not report them itself is instrumented here.
# The ported Rails store (runtime/spinel/active_support_cache.rb)
# publishes every `cache_<operation>.active_support` through its own
# private `Store#instrument`, so a second report from this file would
# fire each event twice; the bounded store
# (runtime/spinel/active_support_cache_memory.rb) has none. The probe
# for a hit reads the store without reporting, so it is no event of its
# own.
module ActiveSupport
  module Cache
    class MemoryStore
      unless private_method_defined?(:instrument)
        module Instrumented
          def read(name, options = nil)
            payload = __rh_payload(name, options)
            payload[:hit] = __rh_hit?(name, options)
            value = super
            ActiveSupport::Notifications.instrument("cache_read.active_support", payload)
            value
          end

          def exist?(name, options = nil)
            found = super
            ActiveSupport::Notifications.instrument("cache_exist?.active_support", __rh_payload(name, options))
            found
          end

          def write(name, value, options = nil)
            stored = super
            unless @__rh_in_write_multi
              ActiveSupport::Notifications.instrument("cache_write.active_support", __rh_payload(name, options))
            end
            stored
          end

          # The bounded `write_multi` writes through `write`; Rails reports
          # one `cache_write_multi` for the batch, keyed by the names.
          def write_multi(hash, options = nil)
            @__rh_in_write_multi = true
            stored = super
            @__rh_in_write_multi = false
            payload = { store: self.class.name, key: hash.keys.map { |name| __rh_key(name, options) } }
            payload.merge!(options) if options
            ActiveSupport::Notifications.instrument("cache_write_multi.active_support", payload) unless hash.empty?
            stored
          ensure
            @__rh_in_write_multi = false
          end

          # `fetch_multi(*names, **options) { |name| … }` reads the names
          # first — one `cache_read_multi` marked `super_operation:
          # :fetch_multi` — then writes the computed ones (`write_multi`).
          def fetch_multi(*names, &block)
            return super unless block
            options = names.last.instance_of?(Hash) ? names.last : nil
            keys = options ? names[0...-1] : names
            unless options && options[:force]
              payload = { store: self.class.name, key: keys.map { |name| __rh_key(name, options) } }
              payload.merge!(options) if options
              payload[:super_operation] = :fetch_multi
              payload[:hits] = keys.select { |name| __rh_hit?(name, options) }.map { |name| __rh_key(name, options) }
              ActiveSupport::Notifications.instrument("cache_read_multi.active_support", payload)
            end
            super
          end

          # Rails' `fetch(name, options = nil, &block)`: the options a
          # caller writes as keywords arrive as the one Hash, and go on to
          # the store (and to the hit probe, so a `namespace:` finds its
          # entry). A blockless fetch is a plain `read`, which reports
          # itself; `force: true` skips the read, as Rails does. The
          # bounded store writes a computed value without going through
          # `write`, so the write is reported here.
          def fetch(name, options = nil, &block)
            return super unless block
            unless options && options[:force]
              payload = __rh_payload(name, options)
              payload[:super_operation] = :fetch
              payload[:hit] = __rh_hit?(name, options)
              ActiveSupport::Notifications.instrument("cache_read.active_support", payload)
            end
            computed = false
            result = super(name, options) { |computed_name| computed = true; block.call(computed_name) }
            unless !computed || (result.nil? && options && options[:skip_nil])
              ActiveSupport::Notifications.instrument("cache_write.active_support", __rh_payload(name, options))
            end
            result
          end

          # `read_multi(*names, **options)`: a trailing Hash is the options,
          # not a name.
          def read_multi(*names)
            found = super
            options = names.last.instance_of?(Hash) ? names.last : nil
            keys = options ? names[0...-1] : names
            payload = { store: self.class.name, key: keys.map { |name| __rh_key(name, options) } }
            payload.merge!(options) if options
            payload[:hits] = found.keys.map { |name| __rh_key(name, options) }
            ActiveSupport::Notifications.instrument("cache_read_multi.active_support", payload)
            found
          end

          private

          # `Store#instrument`'s payload: the store, the normalized key
          # (namespace included) and the call's options.
          def __rh_payload(name, options)
            payload = { store: self.class.name, key: __rh_key(name, options) }
            payload.merge!(options) if options
            payload
          end

          def __rh_key(name, options)
            storage_key(name, ActiveSupport::Cache::MemoryStore.options(options))
          end

          # Whether a live entry is filed under `name`, asked of the store
          # without the report `exist?` makes.
          def __rh_hit?(name, options)
            key = __rh_key(name, options)
            @lock.synchronize { live_locked(key)[0] }
          end
        end
        prepend Instrumented
      end
    end
  end
end

# Rails' collection renderer reports `render_collection.action_view`
# once per collection: the partial's template file, the element count,
# and — under collection caching — how many fragments were served from
# the store. The runtime's hook carries the facts; this reports them.
module ActionView
  module ViewHelpers
    class << self
      alias_method :__rh_collection_rendered, :collection_rendered

      def collection_rendered(identifier, hits)
        ActiveSupport::Notifications.instrument("render_collection.action_view", {
          identifier: File.join(Dir.pwd, identifier),
          count: hits.length,
          cache_hits: hits.count { |hit| !hit.nil? }
        })
        __rh_collection_rendered(identifier, hits)
      end
    end
  end
end

# The integration session's cookie jar is Rack::Test's, which answers nil
# for a cookie the browser never held — `assert_not cookies[
# "_campfire_session"]` asks exactly that — and "" for one a response
# deleted, as it keeps the expired entry (Rails 8's generated
# `assert_empty cookies[:session_id]` after sign-out reads it). The
# shared harness drops a deleted cookie (`accept_cookies`), so the names
# it dropped are remembered here; the runtime's controller-side jar
# answers "" for both (typed String, see action_controller/cookies.rb).
# The names live on the test, not the jar: the harness builds a fresh jar
# from each response's accepted cookies.
module TestCookieReads
  attr_writer :deleted_cookie_names

  def [](key)
    name = key.to_s
    return super if to_h.key?(name)
    (@deleted_cookie_names || []).include?(name) ? "" : nil
  end
end

module RequestDispatch
  module NilCookieReads
    def cookies
      jar = super
      jar.extend(TestCookieReads) unless jar.is_a?(TestCookieReads)
      jar.deleted_cookie_names = deleted_cookie_names
      jar
    end

    def deleted_cookie_names
      @__deleted_cookie_names ||= []
    end

    def accept_cookies(carried, pending)
      names = deleted_cookie_names
      pending.each do |k, v|
        if v == ""
          names << k unless names.include?(k)
        else
          names.delete(k)
        end
      end
      super
    end
  end
  prepend NilCookieReads
end

# rails-dom-testing's `assert_select` hands its block the matched
# elements; the shared harness's string scanner has no elements to
# hand. Here they are Nokogiri's, selected from the same root.
module RequestDispatch
  module SelectedElements
    def assert_select(selector_or_root, content_or_opts = nil, opts = nil, &block)
      return super unless block
      super(selector_or_root, content_or_opts, opts, &nil)
      if selector_or_root.is_a?(String)
        root = HTML_FRAGMENT_PARSER.fragment(@__response.body.to_s)
        selector = selector_or_root
        substitute = content_or_opts
      else
        root = selector_or_root
        root = HTML_FRAGMENT_PARSER.fragment(root.to_s) if root.is_a?(String)
        selector = content_or_opts
        substitute = opts
      end
      if selector.include?("?") && (substitute.is_a?(String) || substitute.is_a?(Numeric))
        selector = selector.sub("?", substitute.to_s.inspect)
      end
      block.call(root.css(selector))
    end
  end
  prepend SelectedElements
end

module Rails
  class MemoryStore
    module Instrumented
      def read_str(key)
        value = super
        ActiveSupport::Notifications.instrument("cache_read.active_support", { key: key.to_s, hit: !value.nil? })
        value
      end

      def write_str(key, value, ttl)
        stored = super
        ActiveSupport::Notifications.instrument("cache_write.active_support", { key: key.to_s })
        stored
      end
    end
    prepend Instrumented
  end
end

# Rails' time helpers stub `Time.now`: under `travel_to` / `freeze_time`
# app and test code that asks the stdlib clock — `Time.current`, which
# the lowering grounds to `Time.now.utc`, included — reads the traveled
# instant, the same one `ActiveSupport.now` (and so every stamped
# `created_at`) reads. The harness's helpers move `ActiveSupport`'s
# clock; this lets `Time.now` follow it while one is set, and reads
# the real clock otherwise. A compiled tree cannot redefine `Time.now`;
# there the test clock reaches only what goes through `ActiveSupport`.
# A tree carrying the ActiveSupport::Testing::TimeHelpers port
# (runtime/active_support_testing_time_helpers.rb) keeps Rails' own
# helpers: they stub `Time.now` themselves, and this `travel_to` would
# shadow theirs (`freeze_time` passes it `with_usec:`).
unless defined?(ActiveSupport::Testing::TimeHelpers)
  class << Time
    alias_method :__roundhouse_real_now, :now

    def now(**options)
      traveling = ActiveSupport::FROZEN_AT[0] != 0 || ActiveSupport::TRAVEL_OFFSET[0] != 0
      return __roundhouse_real_now(**options) unless traveling && options.empty?
      ActiveSupport.clock
    end
  end

  module ActiveSupport
    def self.clock
      frozen = FROZEN_AT[0]
      return Time.at(frozen) if frozen != 0
      Time.__roundhouse_real_now + TRAVEL_OFFSET[0]
    end
  end

  class TestBase
    # Relative to the REAL clock, which is what the offset is kept against;
    # `Time.now` may already be traveled.
    def travel_to(target)
      ActiveSupport.travel(target.to_i - Time.__roundhouse_real_now.to_i)
      return unless block_given?
      begin
        yield
      ensure
        travel_back
      end
    end
  end
end
