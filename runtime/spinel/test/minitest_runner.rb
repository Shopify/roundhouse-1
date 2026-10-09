# CRuby test harness only (packaged explicitly). No autorun and no Rails: application code still
# executes entirely in the emitted project. Spinel and JRuby do not load this.
require "minitest/test"
require "json"

module RoundhouseMinitestAssertions
  include Minitest::Assertions

  attr_writer :assertions

  def assertions
    @assertions || 0
  end

  # Rails' fixture phase. ActiveRecord::TestFixtures#before_setup calls
  # setup_fixtures and only then super, which reaches ActiveSupport's
  # before_setup (run_callbacks :setup); Minitest calls setup after that.
  # TestBase#setup is this tree's fixture/harness reset (SchemaSetup.reset!,
  # clock, job/mail/broadcast/stub/cache clears), so the runner calls it
  # here, before any setup callback or authored setup reads a record.
  def __roundhouse_setup_fixtures
    TestBase.instance_method(:setup).super_method.bind_call(self)
  end

  # Where an authored `super` from setup lands: Minitest::Test#setup is
  # empty. The reset already ran above; running it again here reloaded
  # fixtures AFTER setup callbacks had read them (a `User.take` held a
  # password digest whose salt the reload had replaced).
  def setup
  end

  # The compiler emits setup blocks here, separately from an authored setup
  # method. Rails runs these callbacks before calling setup on the test.
  def __roundhouse_before_setup
  end

  # ActiveSupport's names for the same primitive assertions
  # (ActiveSupport::TestCase's aliases of Minitest's refutations).
  alias assert_not_equal refute_equal
  alias assert_not_nil refute_nil
  alias assert_not_empty refute_empty
  alias assert_not_includes refute_includes
  alias assert_not_same refute_same
  alias assert_no_match refute_match
  alias assert_not_in_delta refute_in_delta
  alias assert_not_in_epsilon refute_in_epsilon
  alias assert_not_instance_of refute_instance_of
  alias assert_not_kind_of refute_kind_of
  alias assert_not_operator refute_operator
  alias assert_not_predicate refute_predicate
  alias assert_not_respond_to refute_respond_to
  alias assert_not_pattern refute_pattern if Minitest::Assertions.method_defined?(:refute_pattern)

  # Minitest's capture_io serializes global IO replacement with this hook.
  def _synchronize(&block)
    Minitest::Test.io_lock.synchronize(&block)
  end

  # ---- Mocha's Minitest adapter -----------------------------------
  #
  # mocha/minitest's lifecycle (Mocha::Integration::Minitest::Adapter),
  # which TestBase wires by hand: mocha_setup first in its setup,
  # mocha_verify in its teardown (or `__roundhouse_verify_stubs`),
  # mocha_teardown after it. These keep the adapter's conditions on the
  # three calls.
  #
  # mocha/minitest counts one assertion for every verified expectation
  # whose cardinality needs verifying (`expects`, `.never`, `.once`,
  # `.times(n)`, a `mock(name, k: v)` key; not a plain `stubs`), through
  # an AssertionCounter whose `increment` is `test.assert(true)`. Mocha 2
  # (Core's lock: 2.5.0) hands the counter to `mocha_verify` in
  # `before_teardown`; Mocha 3 hands it to `mocha_setup` (and also counts
  # an unexpected invocation of a matching expectation). Both are honored
  # here, whichever gem the tree loaded, so the count is that gem's own.
  # A mocha whose hook takes no counter (or that has none) is called as
  # it is defined.
  #
  # mocha_verify runs only when the test has passed so far
  # (`return unless passed?` in the adapter's before_teardown).
  #
  # mocha_verify and mocha_teardown run only after this test's
  # mocha_setup. The runner's fixture reset runs before TestBase#setup,
  # as ActiveRecord::TestFixtures#before_setup runs before the adapter's
  # before_setup; when the reset raised, there is no Mockery to verify or
  # tear down, and Mockery.teardown popped a stack nothing had pushed:
  # every errored row carried a second NoMethodError ("undefined method
  # 'pop' for nil") from TestBase#teardown.
  def mocha_setup(*args)
    if args.empty? && __roundhouse_counting(:mocha_setup)
      super(Mocha::Integration::AssertionCounter.new(self))
    else
      super(*args)
    end
    @__roundhouse_mocha_set_up = true
  end

  def mocha_verify(*args)
    return unless @__roundhouse_mocha_set_up && __roundhouse_passed?
    if args.empty? && __roundhouse_counting(:mocha_verify)
      super(Mocha::Integration::AssertionCounter.new(self))
    else
      super(*args)
    end
  end

  def mocha_teardown(*args)
    return unless @__roundhouse_mocha_set_up
    @__roundhouse_mocha_set_up = false
    super
  end

  private def __roundhouse_counting(hook)
    return false unless defined?(Mocha::Integration::AssertionCounter)
    defined_by = RoundhouseMinitestAssertions.instance_method(hook).bind(self).super_method
    !defined_by.nil? && !defined_by.arity.zero?
  end

  # Minitest::Test#passed?: no failure, error or skip recorded so far.
  # The runner hands the test its live exception list.
  attr_writer :__roundhouse_errors

  def __roundhouse_passed?
    @__roundhouse_errors.nil? || @__roundhouse_errors.empty?
  end
end

# ---- ActiveSupport::Testing::Assertions (ActiveSupport 8.2) ---------
#
# Ported from activesupport/lib/active_support/testing/assertions.rb at
# the Rails Core runs (Shopify/rails 5813559f4aed). Each one asserts
# through Minitest, so it counts and fails as Rails' does:
# `assert_difference` is `assert_nothing_raised` (1) plus one
# `assert_equal` per expression. TestBase's own versions of these stay
# for the targets without Minitest; this module is prepended over them.
module RoundhouseActiveSupportAssertions
  UNTRACKED = Object.new.freeze

  def assert_not(object, message = nil)
    message ||= -> { "Expected #{mu_pp(object)} to be nil or false" }
    assert !object, message
  end

  def assert_raises(*exp, match: nil, &block)
    error = super(*exp, &block)
    assert_match(match, error.message) if match
    error
  end
  alias :assert_raise :assert_raises

  def assert_nothing_raised
    yield.tap { assert(true) }
  rescue => error
    raise Minitest::UnexpectedError.new(error)
  end

  def assert_difference(expression, *args, &block)
    expressions =
      if expression.is_a?(Hash)
        message = args[0]
        expression
      else
        difference = args[0] || 1
        message = args[1]
        Array(expression).to_h { |e| [e, difference] }
      end

    exps = expressions.keys.map { |e| _expression_to_callable(e, block.binding) }
    before = exps.map(&:call)

    retval = _assert_nothing_raised_or_warn("assert_difference", &block)

    expressions.zip(exps, before) do |(code, diff), exp, before_value|
      actual = exp.call
      rich_message = -> do
        code_string = code.respond_to?(:call) ? _callable_to_source_string(code) : code
        error = "`#{code_string}` didn't change by #{diff}, but by #{actual - before_value}."
        error = "#{error}\n#{diff before_value + diff, actual}" if Minitest::VERSION > "6"
        error = "#{message}.\n#{error}" if message
        error
      end
      assert_equal(before_value + diff, actual, rich_message)
    end

    retval
  end

  def assert_no_difference(expression, message = nil, &block)
    assert_difference expression, 0, message, &block
  end

  def assert_changes(expression, message = nil, from: UNTRACKED, to: UNTRACKED, &block)
    exp = _expression_to_callable(expression, block.binding)

    before = exp.call
    retval = _assert_nothing_raised_or_warn("assert_changes", &block)

    unless from == UNTRACKED
      rich_message = -> do
        error = "Expected change from #{from.inspect}, got #{before.inspect}"
        error = "#{message}.\n#{error}" if message
        error
      end
      assert from === before, rich_message
    end

    after = exp.call

    rich_message = -> do
      code_string = expression.respond_to?(:call) ? _callable_to_source_string(expression) : expression
      error = "`#{code_string}` didn't change"
      error = "#{error}. It was already #{to.inspect}." if before == to
      error = "#{message}.\n#{error}" if message
      error
    end
    refute_equal before, after, rich_message

    unless to == UNTRACKED
      rich_message = -> do
        error = "Expected change to #{to.inspect}, got #{after.inspect}\n"
        error = "#{message}.\n#{error}" if message
        error
      end
      assert to === after, rich_message
    end

    retval
  end

  def assert_no_changes(expression, message = nil, from: UNTRACKED, &block)
    exp = _expression_to_callable(expression, block.binding)

    before = exp.call
    retval = _assert_nothing_raised_or_warn("assert_no_changes", &block)

    unless from == UNTRACKED
      rich_message = -> do
        error = "Expected initial value of #{from.inspect}, got #{before.inspect}"
        error = "#{message}.\n#{error}" if message
        error
      end
      assert from === before, rich_message
    end

    after = exp.call

    rich_message = -> do
      code_string = expression.respond_to?(:call) ? _callable_to_source_string(expression) : expression
      error = "`#{code_string}` changed."
      error = "#{message}.\n#{error}" if message
      error = "#{error}\n#{diff before, after}" if Minitest::VERSION > "6"
      error
    end

    if before.nil?
      assert_nil after, rich_message
    else
      assert_equal before, after, rich_message
    end

    retval
  end

  private
    # Rails also logs a hint through the test's tagged logger; there is
    # none here, and the exception is the same.
    def _assert_nothing_raised_or_warn(assertion, &block)
      assert_nothing_raised(&block)
    end

    def _callable_to_source_string(callable)
      if defined?(RubyVM::InstructionSequence) && callable.is_a?(Proc)
        iseq = RubyVM::InstructionSequence.of(callable)
        source =
          if iseq.script_lines
            iseq.script_lines.join("\n")
          elsif File.readable?(iseq.absolute_path)
            File.read(iseq.absolute_path)
          end

        return callable unless source

        location = iseq.to_a[4][:code_location]
        return callable unless location

        lines = source.lines[(location[0] - 1)..(location[2] - 1)]
        lines[-1] = lines[-1].byteslice(...location[3])
        lines[0] = lines[0].byteslice(location[1]...)
        source = lines.join.strip

        source = source.sub(/^->\s*/, "")

        if source.start_with?("{")
          source.delete_suffix!("}")
          source.delete_prefix!("{")
          source.strip!
          if !source.include?("\n") && !source.start_with?("|")
            return source
          end
        end
      end

      callable
    end

    def _expression_to_callable(callable_or_ruby_source, binding)
      return callable_or_ruby_source if callable_or_ruby_source.respond_to?(:call)

      case callable_or_ruby_source
      when String, Symbol
        -> { eval(callable_or_ruby_source.to_s, binding) }
      else
        raise ArgumentError, "The expression must be a callable object like a Proc, or a String of Ruby code. Got #{callable_or_ruby_source.inspect}"
      end
    end
end

# The port answers to Rails' own name: app test code includes it
# explicitly (Core's TestCase and its RowLevelAccess::TestHelper:
# `include ActiveSupport::Testing::Assertions`). TestBase already has it
# (RoundhouseMinitestAssertions, prepended), so a test class's include is
# a no-op, as natively ActiveSupport::TestCase already includes it.
module ActiveSupport
  module Testing
    Assertions = RoundhouseActiveSupportAssertions unless const_defined?(:Assertions, false)
  end
end

# ---- The Rails test helpers this tree models ------------------------
#
# TestBase's job, query, event and error helpers compute their answer
# over the tree's own state (`ActiveJob.performed`, `Db.capture_sql`,
# `ActiveSupport.event_reporter`); here they ASSERT it the way Rails
# does, so a pass counts what Rails counts and a mismatch is a Minitest
# failure with Rails' message rather than a RuntimeError.
module RoundhouseRailsTestHelpers
  # ActiveJob::TestHelper (activejob/lib/active_job/test_helper.rb): a
  # block form also counts its `_assert_nothing_raised_or_warn`.
  def assert_enqueued_jobs(number, only: [], &block)
    actual =
      if block
        capture_enqueued_jobs(only) { _assert_nothing_raised_or_warn("assert_enqueued_jobs", &block) }.length
      else
        capture_enqueued_jobs(only).length
      end
    assert_equal number, actual, "#{number} jobs expected, but #{actual} were enqueued"
  end

  def assert_no_enqueued_jobs(only: [], &block)
    assert_enqueued_jobs 0, only: only, &block
  end

  # TestBase's narrowing stands (only the job class is matched, see its
  # note); the assertion and its message are Rails'.
  def assert_enqueued_with(job: "", args: nil, &block)
    name = job.to_s
    jobs =
      if block
        capture_enqueued_jobs([]) { _assert_nothing_raised_or_warn("assert_enqueued_with", &block) }
      else
        capture_enqueued_jobs([])
      end
    matching = name.empty? ? jobs : jobs.select { |enqueued| enqueued.to_s == name }
    shown = job.is_a?(String) && !name.empty? && Object.const_defined?(name) ? Object.const_get(name) : job
    shown = nil if name.empty?
    expected = { job: shown, args: args }.compact
    message = +"No enqueued job found with #{expected}"
    if jobs.empty?
      message << "\n\nNo jobs were enqueued"
    elsif matching.empty?
      message << "\n\nNo jobs of class #{expected[:job]} were enqueued, job classes enqueued: "
      message << jobs.map(&:to_s).join(", ")
    end
    assert !matching.empty?, message
    nil
  end

  def perform_enqueued_jobs(only: [], &block)
    return super(only: only) if block.nil?
    super(only: only) { _assert_nothing_raised_or_warn("perform_enqueued_jobs", &block) }
  end

  # ActiveRecord::Assertions::QueryAssertions
  # (activerecord/lib/active_record/testing/query_assertions.rb).
  def assert_queries_count(count = nil, include_schema: false, &block)
    result, queries = capture_queries(include_schema) { _assert_nothing_raised_or_warn("assert_queries_count", &block) }
    if count
      assert_equal count, queries.size, "#{queries.size} instead of #{count} queries were executed#{queries.empty? ? '' : ". Queries:\n\n#{queries.join("\n\n")}"}"
    else
      assert_operator queries.size, :>=, 1, "1 or more queries expected, but none were executed"
    end
    result
  end

  def assert_no_queries(include_schema: false, &block)
    assert_queries_count(0, include_schema: include_schema, &block)
  end

  def assert_queries_match(match, count: nil, include_schema: false, &block)
    result, queries = capture_queries(include_schema) { _assert_nothing_raised_or_warn("assert_queries_match", &block) }
    matched_queries = queries.select { |query| match === query }
    if count
      assert_equal count, matched_queries.size, "#{matched_queries.size} instead of #{count} matching queries were executed#{queries.empty? ? '' : ". Queries:\n\n#{queries.join("\n\n")}"}"
    else
      assert_operator matched_queries.size, :>=, 1, "1 or more matching queries expected, but none were executed#{queries.empty? ? '' : ". Queries:\n\n#{queries.join("\n\n")}"}"
    end
    result
  end

  def assert_no_queries_match(match, include_schema: false, &block)
    assert_queries_match(match, count: 0, include_schema: include_schema, &block)
  end

  # ---- ActiveSupport::Testing::EventReporterAssertions (8.2) -------
  #
  # Ported from active_support/testing/event_reporter_assertions.rb over
  # the tree's `ActiveSupport.event_reporter` (runtime/active_support_reporters.rb),
  # which delivers each event hash to a subscriber's `emit`.
  module EventCollector
    @subscribed = false
    @mutex = Mutex.new

    class Event
      attr_reader :event_data

      def initialize(event_data)
        @event_data = event_data
      end

      def inspect
        "#{event_data[:name]} (payload: #{event_data[:payload].inspect}, tags: #{event_data[:tags].inspect})"
      end

      def matches?(name, payload, tags)
        return false unless name.to_s == event_data[:name]

        if payload && payload.is_a?(Hash)
          return false unless matches_hash?(payload, :payload)
        end

        return false unless matches_hash?(tags, :tags)
        true
      end

      private
        def matches_hash?(expected_hash, event_key)
          expected_hash.all? do |k, v|
            if v.is_a?(Regexp)
              event_data.dig(event_key, k).to_s.match?(v)
            else
              event_data.dig(event_key, k) == v
            end
          end
        end
    end

    class << self
      def emit(event)
        event_recorders&.each do |events|
          events << Event.new(event)
        end
        true
      end

      def record
        subscribe
        events = []
        event_recorders << events
        begin
          yield
          events
        ensure
          event_recorders.delete_if { |r| events.equal?(r) }
        end
      end

      private
        def subscribe
          return if @subscribed

          @mutex.synchronize do
            unless @subscribed
              if defined?(ActiveSupport.event_reporter) && ActiveSupport.event_reporter
                ActiveSupport.event_reporter.subscribe(self)
                @subscribed = true
              else
                raise Minitest::Assertion, "No event reporter is configured"
              end
            end
          end
        end

        def event_recorders
          Thread.current[:active_support_event_reporter_assertions] ||= []
        end
    end
  end

  def assert_no_event_reported(name = nil, payload: {}, tags: {}, &block)
    events = EventCollector.record(&block)

    if name.nil?
      assert_predicate(events, :empty?)
    else
      matching_event = events.find { |event| event.matches?(name, payload, tags) }
      if matching_event
        message = "Expected no '#{name}' event to be reported, but found:\n  " \
          "#{matching_event.inspect}"
        flunk(message)
      end
      assert(true)
    end
  end

  def assert_event_reported(name, payload: nil, tags: {}, &block)
    events = EventCollector.record(&block)

    if events.empty?
      flunk("Expected an event to be reported, but there were no events reported.")
    elsif (event = events.find { |event| event.matches?(name, payload, tags) })
      assert(true)
      event.event_data
    else
      message = "Expected an event to be reported matching:\n  " \
        "name: #{name.inspect}\n  " \
        "payload: #{payload.inspect}\n  " \
        "tags: #{tags.inspect}\n" \
        "but none of the #{events.size} reported events matched:\n  " \
        "#{events.map(&:inspect).join("\n  ")}"
      flunk(message)
    end
  end

  def assert_events_reported(expected_events, &block)
    events = EventCollector.record(&block)

    if events.empty? && expected_events.size > 0
      flunk("Expected #{expected_events.size} events to be reported, but there were no events reported.")
    end

    events_copy = events.dup

    expected_events.each do |expected_event|
      name = expected_event[:name]
      payload = expected_event[:payload] || {}
      tags = expected_event[:tags] || {}

      matching_event_index = events_copy.find_index { |event| event.matches?(name, payload, tags) }

      if matching_event_index
        events_copy.delete_at(matching_event_index)
      else
        message = "Expected an event to be reported matching:\n  " \
          "name: #{name.inspect}\n  " \
          "payload: #{payload.inspect}\n  " \
          "tags: #{tags.inspect}\n" \
          "but none of the #{events.size} reported events matched:\n  " \
          "#{events.map(&:inspect).join("\n  ")}"

        flunk(message)
      end
    end

    assert(true)
  end

  def with_debug_event_reporting(&block)
    ActiveSupport.event_reporter.with_debug(&block)
  end

  # ---- ActiveSupport::Testing::ErrorReporterAssertions (8.2) -------
  module ErrorCollector
    @subscribed = false
    @mutex = Mutex.new

    Report = Struct.new(:error, :handled, :severity, :context, :source, keyword_init: true)
    class Report
      alias_method :handled?, :handled
    end

    class << self
      def record
        subscribe
        recorders = (Thread.current[:active_support_error_reporter_assertions] ||= [])
        reports = []
        recorders << reports
        begin
          yield
          reports
        ensure
          recorders.delete_if { |r| reports.equal?(r) }
        end
      end

      def report(error, **kwargs)
        report = Report.new(error: error, **kwargs)
        Thread.current[:active_support_error_reporter_assertions]&.each do |reports|
          reports << report
        end
        true
      end

      private
        def subscribe
          return if @subscribed
          @mutex.synchronize do
            return if @subscribed

            if defined?(ActiveSupport.error_reporter) && ActiveSupport.error_reporter
              ActiveSupport.error_reporter.subscribe(self)
              @subscribed = true
            else
              raise Minitest::Assertion, "No error reporter is configured"
            end
          end
        end
    end
  end

  def assert_no_error_reported(&block)
    reports = ErrorCollector.record do
      _assert_nothing_raised_or_warn("assert_no_error_reported", &block)
    end
    assert_predicate(reports, :empty?)
  end

  def assert_error_reported(error_class = StandardError, &block)
    reports = ErrorCollector.record do
      _assert_nothing_raised_or_warn("assert_error_reported", &block)
    end

    if reports.empty?
      assert(false, "Expected a #{error_class.name} to be reported, but there were no errors reported.")
    elsif (report = reports.find { |r| error_class === r.error })
      self.assertions += 1
      report
    else
      message = "Expected a #{error_class.name} to be reported, but none of the " \
        "#{reports.size} reported errors matched:  \n" \
        "#{reports.map { |r| r.error.class.name }.join("\n  ")}"
      assert(false, message)
    end
  end

  def capture_error_reports(error_class = StandardError, &block)
    reports = ErrorCollector.record(&block)
    reports.select { |r| error_class === r.error }
  end
end

# ActionDispatch::Assertions::ResponseAssertions#assert_response and
# #assert_redirected_to (actionpack/lib/action_dispatch/testing/assertions/response.rb):
# one `assert` (a status class) or `assert_equal` (a code); a redirect
# whose location is not `===` the expected argument asserts again over
# both as absolute URLs. Prepended to RequestDispatch, which the
# integration test includes below TestBase, so it stands over
# RequestDispatch's own versions.
module RoundhouseResponseAssertions
  GENERIC_RESPONSE_CODES = { success: "2XX", missing: "404", redirect: "3XX", error: "5XX" }.freeze

  def assert_response(type, message = nil)
    __roundhouse_assert_response(type, message, @__response)
  end

  # Rails' `(url_options, options = {}, message = nil)`; the hand-written
  # two-argument form (`assert_redirected_to "/articles/1", res`) passes
  # the response itself second.
  def assert_redirected_to(url_options = {}, options = {}, message = nil)
    response = @__response
    if options.respond_to?(:status) && options.respond_to?(:location)
      response = options
      options = {}
    end
    options, message = {}, options unless options.is_a?(Hash)

    __roundhouse_assert_response(options[:status] || :redirect, message, response)
    location = __roundhouse_redirection(response.location)
    return true if url_options === location

    redirect_expected = __roundhouse_redirection(url_options)
    message ||= "Expected response to be a redirect to <#{redirect_expected}> but was a redirect to <#{location}>"
    assert_operator redirect_expected, :===, location, message
  end

  private
    def __roundhouse_assert_response(type, message, response)
      message ||= __roundhouse_response_message(type, response)
      if (range = RequestDispatch::STATUS_RANGES[type])
        assert range.include?(response.status), message
      else
        assert_equal __roundhouse_status_code(type), response.status, message
      end
    end

    # AssertionResponse#code: a Symbol through the status table, a code
    # as given.
    def __roundhouse_status_code(type)
      return type unless type.is_a?(Symbol)
      code = ActionController::STATUS_CODES[type]
      raise ArgumentError, "Invalid response name: #{type}" if code.nil?
      code
    end

    # `_compute_redirect_to_location`: a URL with a scheme (or `//`) as
    # written, a path under this request's origin. Rails' controller
    # answers the absolute form in `Location`; this tree's keeps the path.
    # The origin is the request's `protocol + host_with_port`, which
    # RequestDispatch builds (`https://`, the scheme's standard port
    # dropped) when it is present.
    def __roundhouse_redirection(fragment)
      case fragment
      when Regexp then fragment
      when /\A([a-z][a-z\d\-+.]*:|\/\/).*/i then fragment.to_str.delete("\0\r\n")
      when /\A\// then
        if respond_to?(:redirection_url_for_assertion, true)
          redirection_url_for_assertion(fragment).delete("\0\r\n")
        else
          "http://#{respond_to?(:host) ? host : "www.example.com"}#{fragment}".delete("\0\r\n")
        end
      when String then "http://#{respond_to?(:host) ? host : "www.example.com"}#{fragment}".delete("\0\r\n")
      else fragment
      end
    end

    def __roundhouse_response_message(expected, response)
      lambda do
        message = +"Expected response to be a <#{__roundhouse_code_with_name(expected)}>, " \
          "but was a <#{__roundhouse_code_with_name(response.status)}>"
        if response.redirect? && !response.location.to_s.empty?
          message << " redirect to <#{__roundhouse_redirection(response.location)}>"
        end
        body = response.body.to_s
        message << "\nResponse body: #{body}" if body.size <= 500
        message
      end
    end

    # AssertionResponse#code_and_name: a name as given beside its code, a
    # code beside its status name.
    def __roundhouse_code_with_name(code_or_name)
      if code_or_name.is_a?(Symbol)
        code = GENERIC_RESPONSE_CODES[code_or_name] || __roundhouse_status_code(code_or_name)
        "#{code}: #{code_or_name}"
      else
        "#{code_or_name}: #{__roundhouse_status_name(code_or_name)}"
      end
    end

    # Rack::Utils::HTTP_STATUS_CODES, where Rack is loaded; otherwise
    # the status table's own name, titled as Rack spells it.
    def __roundhouse_status_name(code)
      return Rack::Utils::HTTP_STATUS_CODES[code] if defined?(Rack::Utils::HTTP_STATUS_CODES)
      symbol = ActionController::STATUS_CODES.key(code)
      return nil if symbol.nil?
      symbol == :ok ? "OK" : symbol.to_s.split("_").map(&:capitalize).join(" ")
    end
end

module RoundhouseMinitestAssertions
  include RoundhouseActiveSupportAssertions
  include RoundhouseRailsTestHelpers
end

if defined?(Mocha::API)
  # Present in every released mocha the adapter supports; a gem that
  # only stands in for mocha (no such files) keeps its own behavior.
  %w[mocha/integration/assertion_counter mocha/expectation_error_factory].each do |feature|
    require feature if $LOAD_PATH.resolve_feature_path(feature)
  end
  # mocha/minitest's adapter: an unmet expectation is a Minitest failure.
  Mocha::ExpectationErrorFactory.exception_class = Minitest::Assertion if defined?(Mocha::ExpectationErrorFactory)
end

# A `MochaStub` (lower::mocha's slot for an app method's stub) verified
# as mocha 2.5 verifies the expectation it stands for (Mockery#verify,
# Expectation#verified?): each expectation examined counts one assertion
# on the test, mock by mock and newest first within one, and the first
# unmet one stops the walk and fails the test with mocha's message. A
# call past an exact count is mocha's unexpected invocation: it fails
# the test there, at the call.
if defined?(MochaStub)
  class MochaStub
    def self.file_any_instance(name, stub)
      stub.__roundhouse_any_instance!
      ANY_INSTANCE[name] = stub
      stub
    end

    def self.verify_all!(test = nil)
      pending = PENDING.dup
      PENDING.clear
      owners = pending.group_by(&:__roundhouse_owner)
      unmet = nil
      owners.each_value do |stubs|
        stubs.reverse_each do |stub|
          test&.assert(true)
          next if stub.__roundhouse_met?
          unmet = stub
          break
        end
        break if unmet
      end
      return nil if unmet.nil?

      raise Minitest::Assertion, "not all expectations were satisfied\n#{__roundhouse_mockery_inspect(pending)}"
    end

    # Mockery#mocha_inspect over the filed expectations.
    def self.__roundhouse_mockery_inspect(stubs)
      unsatisfied, satisfied = stubs.partition { |stub| !stub.__roundhouse_met? }
      message = +""
      message << "unsatisfied expectations:\n- #{unsatisfied.reverse.map(&:__roundhouse_inspect).join("\n- ")}\n" unless unsatisfied.empty?
      message << "satisfied expectations:\n- #{satisfied.reverse.map(&:__roundhouse_inspect).join("\n- ")}\n" unless satisfied.empty?
      message
    end

    def record!
      @seen += 1
      if @expected >= 0 && @seen > @expected
        # Mocha 3 also counts the unexpected invocation of a matching
        # expectation (Mock#raise_unexpected_invocation_error); 2.5 does not.
        if defined?(Mocha::Hooks) && !Mocha::Hooks.instance_method(:mocha_setup).arity.zero?
          Thread.current[:roundhouse_current_test]&.assert(true)
        end
        raise Minitest::Assertion, "unexpected invocation: #{__roundhouse_signature.sub("(any_parameters)", "()")}\n" \
          "#{MochaStub.__roundhouse_mockery_inspect(PENDING)}"
      end
      nil
    end

    def __roundhouse_any_instance!
      @any_instance = true
    end

    def __roundhouse_owner
      [@any_instance ? :any_instance : :instance, @name.split("#").first]
    end

    def __roundhouse_met?
      @expected < 0 || @seen == @expected
    end

    def __roundhouse_inspect
      expected = @expected.zero? ? "expected never" : "expected exactly #{__roundhouse_times(@expected)}"
      "#{expected}, invoked #{__roundhouse_times(@seen)}: #{__roundhouse_signature}"
    end

    def __roundhouse_signature
      klass, method = @name.split("#", 2)
      owner = @any_instance ? "#<AnyInstance:#{klass}>" : "#<#{klass}>"
      "#{owner}.#{method}(any_parameters)"
    end

    def __roundhouse_times(number)
      case number
      when 0 then "never"
      when 1 then "once"
      when 2 then "twice"
      else "#{number} times"
      end
    end
  end
end

RequestDispatch.prepend(RoundhouseResponseAssertions) if defined?(RequestDispatch)

# Prepend to the framework base, not each authored test class. Authored helper
# methods/overrides and setup/teardown super chains retain their precedence.
TestBase.prepend(RoundhouseMinitestAssertions)

# Minitest::Test::LifecycleHooks (minitest 5.27): the four hooks around
# setup and teardown, empty at the bottom of the chain so a module's
# override reaches them with `super`. Core's TestAccessBlocker, a Module
# its test helpers include, defines `before_setup` (block, then `super()`)
# and `after_teardown` (`super()`, then unblock); without these its
# `super` had no method to reach. Included after TestBase's own modules,
# each passes on to one of theirs below it (ActiveSupport's TimeHelpers
# travels back in `after_teardown`).
module RoundhouseMinitestLifecycleHooks
  def before_setup
    super if defined?(super)
  end

  def after_setup
    super if defined?(super)
  end

  def before_teardown
    super if defined?(super)
  end

  def after_teardown
    super if defined?(super)
  end
end
TestBase.include(RoundhouseMinitestLifecycleHooks)

# ---- ActiveSupport::Testing::SetupAndTeardown (ActiveSupport 8.2) ----
#
# The `setup` and `teardown` class macros that app code calls as it runs
# (`ActiveSupport::TestCase.class_eval { setup do … end }` in a support
# file, `base.setup do … end` in a helper's `included` hook); a test
# class's own `setup do` blocks are compiled (`__roundhouse_before_setup`).
# As with ActiveSupport::Callbacks' `set_callback(:setup, :before, …)` and
# `set_callback(:teardown, :after, …)`, a class's chain holds what it and
# its ancestors registered, in registration order (`prepend: true` puts a
# callback first). Re-registering a method name moves it. Setup callbacks
# run in order, after the fixtures and before `setup`; a `throw :abort`
# skips the rest of them. Teardown callbacks run in reverse order, after
# `teardown`. A filter is a block (instance_exec'd, given the test when
# it takes an argument), a method name, or an object answering
# `before_setup(test)` / `after_teardown(test)`. `if:` and `unless:` take
# the same. Other options, `around` and string filters are refused.
#
# A class's statements below its `setup do`/`teardown do` blocks follow
# `after_compiled_blocks`: natively a callback they register runs after
# those blocks' (setup) or before them (teardown). The blocks here are
# compiled methods the runner calls after the chain (setup) or before it
# (teardown), so such a callback (not `prepend: true`) is refused by name
# when its test runs rather than run out of order.
module RoundhouseTestCallbacks
  @registered = 0

  def self.next_registration
    @registered += 1
  end

  # A test class-body statement (`use_gcs`, carried as written): one
  # that raises is named and the class goes on loading, as a file
  # program's statement does (`RoundhouseAutoload.file_statement`).
  def self.class_body_statement(klass, site)
    yield
  rescue StandardError, NotImplementedError => e
    $stderr.puts "roundhouse: error[test_class_statement_not_run]: #{klass.name}'s class-body statement at #{site} did not run: #{e.class}: #{e.message}"
    nil
  end

  def self.after_compiled_blocks(klass, chain)
    blocks = klass.instance_variable_defined?(:@__roundhouse_after_compiled_blocks) ? klass.instance_variable_get(:@__roundhouse_after_compiled_blocks) : klass.instance_variable_set(:@__roundhouse_after_compiled_blocks, [])
    blocks << chain
    nil
  end

  module ClassMethods
    def setup(*filters, &block)
      RoundhouseTestCallbacks.register(self, :setup, filters, block)
    end

    def teardown(*filters, &block)
      RoundhouseTestCallbacks.register(self, :teardown, filters, block)
    end

    def around(*)
      raise NotImplementedError, "roundhouse: #{name}.around: ActiveSupport::Testing::SetupAndTeardown's around callbacks are not ported"
    end
  end

  def self.register(klass, chain, filters, block)
    filters = filters.dup
    options = filters.last.is_a?(Hash) ? filters.pop : {}
    unknown = options.keys - %i[if unless prepend]
    unless unknown.empty?
      raise NotImplementedError, "roundhouse: #{klass.name}.#{chain} #{unknown.map(&:inspect).join(', ')}: callback options other than if:, unless: and prepend: are not ported"
    end
    filters.unshift(block) if block
    filters.each do |filter|
      raise ArgumentError, "Passing string to define a callback is not supported. See the `.set_callback` documentation to see supported values." if filter.is_a?(String)
      entry = {chain: chain, filter: filter, if: Array(options[:if]), unless: Array(options[:unless]),
               prepend: options[:prepend] ? true : false, order: next_registration,
               after_compiled_blocks: klass.instance_variable_defined?(:@__roundhouse_after_compiled_blocks) && klass.instance_variable_get(:@__roundhouse_after_compiled_blocks).include?(chain)}
      entry[:owner] = klass
      (klass.instance_variable_defined?(:@__roundhouse_test_callbacks) ? klass.instance_variable_get(:@__roundhouse_test_callbacks) : klass.instance_variable_set(:@__roundhouse_test_callbacks, [])) << entry
    end
    nil
  end

  # The chain `klass` runs, in order.
  def self.callbacks(klass, chain)
    entries = klass.ancestors.flat_map do |mod|
      mod.instance_variable_defined?(:@__roundhouse_test_callbacks) ? mod.instance_variable_get(:@__roundhouse_test_callbacks) : []
    end
    entries = entries.select { |entry| entry.fetch(:chain) == chain }
    latest = {}
    entries.each do |entry|
      filter = entry.fetch(:filter)
      next unless filter.is_a?(Symbol)
      latest[filter] = entry.fetch(:order) if latest.fetch(filter, 0) < entry.fetch(:order)
    end
    entries = entries.reject { |entry| entry.fetch(:filter).is_a?(Symbol) && latest.fetch(entry.fetch(:filter)) != entry.fetch(:order) }
    entries.sort_by { |entry| entry.fetch(:prepend) ? [0, -entry.fetch(:order)] : [1, entry.fetch(:order)] }
  end

  def self.refuse_misordered(entries, chain)
    entry = entries.find { |e| e.fetch(:after_compiled_blocks, false) && !e.fetch(:prepend) }
    return unless entry
    raise NotImplementedError, "roundhouse: #{entry.fetch(:owner).name}.#{chain} #{entry.fetch(:filter).inspect}, registered below the class's " \
      "compiled `#{chain} do` blocks, would run #{chain == :setup ? 'before' : 'after'} them here: that order is not ported"
  end

  def self.run_setup(test)
    entries = callbacks(test.class, :setup)
    refuse_misordered(entries, :setup)
    catch(:abort) do
      entries.each do |entry|
        call(test, entry.fetch(:filter), "before_setup") if applies?(test, entry)
      end
    end
    nil
  end

  def self.run_teardown(test)
    entries = callbacks(test.class, :teardown)
    refuse_misordered(entries, :teardown)
    entries.reverse_each do |entry|
      call(test, entry.fetch(:filter), "after_teardown") if applies?(test, entry)
    end
    nil
  end

  def self.applies?(test, entry)
    entry.fetch(:if).all? { |condition| call(test, condition, nil) } &&
      entry.fetch(:unless).none? { |condition| call(test, condition, nil) }
  end

  def self.call(test, filter, object_method)
    case filter
    when Symbol
      test.send(filter)
    when Proc
      if filter.arity > 1
        test.instance_exec(test, nil, &filter)
      elsif filter.arity > 0
        test.instance_exec(test, &filter)
      else
        test.instance_exec(&filter)
      end
    else
      unless object_method
        raise NotImplementedError, "roundhouse: callback condition #{filter.inspect}: only method names and procs are ported"
      end
      filter.public_send(object_method, test)
    end
  end
end
TestBase.extend(RoundhouseTestCallbacks::ClassMethods)

# Rack's multipart round trip for the rack-test gem's own
# `Rack::Test::UploadedFile`: rack-test encodes the file as a multipart
# part and Rails parses it back into an `ActionDispatch::Http::UploadedFile`
# (the uploaded name, the declared type, the bytes). The harness hands
# params to the controller directly, so the round trip happens here, on
# the params tree, Hash values and Array elements alike.
#
# Only for the gem's class: where the gem is not loaded, test_helper.rb's
# own `Rack::Test::UploadedFile` is already an
# `ActionDispatch::Http::UploadedFile` and passes through unchanged.
module RoundhouseRackTestUploads
  def dispatch_request(method, path, params, headers = {}, as = nil)
    super(method, path, RoundhouseRackTestUploads.multipart(params), headers, as)
  end

  def self.multipart(value)
    case value
    when Hash
      value.transform_values { |v| multipart(v) }
    when Array
      value.map { |v| multipart(v) }
    else
      return value if value.is_a?(ActionDispatch::Http::UploadedFile)
      return value unless defined?(::Rack::Test::UploadedFile) && value.is_a?(::Rack::Test::UploadedFile)
      bytes = String.new(encoding: Encoding::BINARY)
      value.append_to(bytes)
      ActionDispatch::Http::UploadedFile.new(bytes, value.original_filename, value.content_type)
    end
  end
end
RequestDispatch.prepend(RoundhouseRackTestUploads) if defined?(RequestDispatch) && defined?(ActionDispatch::Http::UploadedFile)

module RoundhouseTestRunner
  # Whether a test file's driver runs: when the file is the program
  # (`ruby test/models/x_test.rb`), or when the program is not an emitted
  # test file (a loader, such as rake's, that requires each one). A
  # subclass's file requires its parent's; the parent's own run is its
  # own file's, as Minitest runs a class once (the subclass's driver lists
  # the tests it inherits).
  def self.drive?(file)
    program = File.expand_path($PROGRAM_NAME)
    program == File.expand_path(file) || !program.end_with?("_test.rb")
  end

  # The emitted tree's root: this file is its test/minitest_runner.rb.
  TREE = File.expand_path("..", __dir__)
  BACKTRACE_FRAMES = 12

  def self.capture(errors, phase)
    yield
  rescue NoMemoryError, SignalException, SystemExit
    raise
  rescue Exception => error
    kind = if error.is_a?(Minitest::Skip) || error.is_a?(TestSkipped)
      "skip"
    elsif error.is_a?(Minitest::UnexpectedError)
      "error"
    elsif error.is_a?(Minitest::Assertion) ||
        (defined?(Mocha::ExpectationError) && error.is_a?(Mocha::ExpectationError))
      "fail"
    else
      "error"
    end
    original = error.is_a?(Minitest::UnexpectedError) ? error.error : error
    trace = inner_backtrace(original, caller(1))
    errors << {"phase" => phase, "status" => kind,
               "class" => original.class.name, "message" => original.message,
               "site" => site(trace), "backtrace" => trace.first(BACKTRACE_FRAMES).map { |frame| relative(frame) }}
  end

  # The frames between the raise and this runner: the error's backtrace
  # without the stack it shares with capture's caller (the runner, the
  # batch loader, the test file's driver), and without capture's own
  # frame, which only says that a block was yielded. What remains ends
  # at the runner's block, which names the phase's call (test.setup,
  # test.teardown). A backtrace from elsewhere (another thread's, or one
  # an exception was built with) shares no frames and stays whole.
  def self.inner_backtrace(error, outer)
    trace = error.backtrace || []
    shared = 0
    shared += 1 while shared < outer.length && shared < trace.length && trace[-1 - shared] == outer[-1 - shared]
    return trace if shared.zero?
    inner = trace[0, trace.length - shared]
    inner = inner[0...-1] if inner.last.to_s.include?("capture'")
    inner
  end

  # The first frame inside the emitted tree, relative to its root, as
  # the batch loader's boot and load rows name theirs; else the first
  # frame. "" when the error carries no backtrace.
  def self.site(trace)
    relative(trace.find { |frame| tree_file(frame) } || trace.first)
  end

  # A frame with its file relative to the tree root when it is in the
  # tree; other frames (gems, Ruby's own) as they are.
  def self.relative(frame)
    frame = frame.to_s
    file = tree_file(frame)
    file ? file + frame[frame.index(/:\d+:/)..] : frame
  end

  # The frame's file relative to the tree root, or nil outside it. A file
  # loaded by a relative path (`ruby -Itest test/models/x_test.rb` from
  # the root) is named relative to the working directory.
  def self.tree_file(frame)
    file = frame[/\A(.+?):\d+:/, 1]
    return nil if file.nil? || file.start_with?("<")
    full = File.expand_path(file)
    full.start_with?(TREE + "/") ? full.delete_prefix(TREE + "/") : nil
  end

  def self.run_one(klass, name)
    errors = []
    test = nil
    capture(errors, "initialize") { test = klass.new }
    Thread.current[:roundhouse_current_test] = test
    if test
      # Live: a phase's failure is visible to the next (mocha_verify's
      # passed? check), as Minitest's failures list is.
      # A test class that does not descend from TestBase (a bare
      # Minitest::Test the emitter kept) has no such writer.
      test.__roundhouse_errors = errors if test.respond_to?(:__roundhouse_errors=)
      capture(errors, "setup") do
        yield if block_given?
        # ActiveRecord::TestFixtures#before_setup's setup_fixtures, first,
        # where the tree carries it (the PostgreSQL tree's
        # runtime/active_record_test_fixtures.rb): the fixtures, loaded
        # once, and the test's transaction.
        if test.respond_to?(:__roundhouse_setup_active_record_fixtures)
          test.__roundhouse_setup_active_record_fixtures(name)
        end
        test.__roundhouse_setup_fixtures
        # Minitest's before_setup hook (`RoundhouseMinitestLifecycleHooks`),
        # where a test's own modules define it; the fixture and callback
        # phases its native `super` chain reaches are the runner's own
        # steps around it.
        test.before_setup if test.respond_to?(:before_setup)
        # ActiveSupport's before_setup: run_callbacks :setup.
        RoundhouseTestCallbacks.run_setup(test) if test.class.is_a?(RoundhouseTestCallbacks::ClassMethods)
        test.__roundhouse_before_setup
        test.setup
        test.after_setup if test.respond_to?(:after_setup)
      end
      capture(errors, "test") { test.public_send(name) } if errors.empty?
      capture(errors, "teardown") { test.before_teardown } if test.respond_to?(:before_teardown)
      # Minitest's before_teardown, where mocha/minitest verifies, and
      # only a test that passed (`return unless passed?`): the stub slots'
      # and mocha's expectations (`__roundhouse_verify_stubs`, the helper's).
      if errors.empty? && test.respond_to?(:__roundhouse_verify_stubs)
        capture(errors, "teardown") { test.__roundhouse_verify_stubs }
      end
      capture(errors, "teardown") { test.teardown }
      # ActiveSupport's after_teardown: run_callbacks :teardown, its
      # failure recorded and the rest of after_teardown run.
      capture(errors, "teardown") { RoundhouseTestCallbacks.run_teardown(test) } if test.class.is_a?(RoundhouseTestCallbacks::ClassMethods)
      # Minitest's after_teardown hook, where something defines it
      # (ActiveSupport's TimeHelpers travels back there).
      capture(errors, "teardown") { test.after_teardown } if test.respond_to?(:after_teardown)
      # mocha/minitest's after_teardown unstubs last, whatever happened.
      capture(errors, "teardown") { test.mocha_teardown } if test.respond_to?(:mocha_teardown)
      # TestFixtures#after_teardown's teardown_fixtures, last.
      if test.respond_to?(:__roundhouse_teardown_fixtures)
        capture(errors, "teardown") { test.__roundhouse_teardown_fixtures(name) }
      end
    end
    # Keep all phase failures. Error wins over failure/skip; a teardown error
    # cannot turn into a pass or disappear behind an earlier skip.
    statuses = errors.map { |error| error.fetch("status") }
    status = %w[error fail skip].find { |kind| statuses.include?(kind) } || "pass"
    result = {"schema" => "roundhouse-test-result/v1", "id" => "#{klass.name}##{name}", "status" => status,
     "minitest_version" => Gem.loaded_specs.fetch("minitest").version.to_s,
     "minitest_runtime_version" => Minitest::VERSION,
     "assertions" => test ? test.assertions : 0, "exceptions" => errors,
     "assertion_count_scope" => "minitest"}
    # The fixture sets this process refused (error[fixture_set_not_loaded]),
    # beside a result whose database lacked their rows.
    not_loaded = fixture_sets_not_loaded
    result["fixture_sets_not_loaded"] = not_loaded unless not_loaded.empty?
    result
  end

  def self.fixture_sets_not_loaded
    return [] unless defined?(ActiveRecord::TestFixtures) && ActiveRecord::TestFixtures.respond_to?(:fixture_sets_not_loaded)
    ActiveRecord::TestFixtures.fixture_sets_not_loaded.keys.sort
  end

  def self.run(klass, methods, &reset)
    results = methods.map do |name|
      result = run_one(klass, name, &reset)
      if (path = ENV["ROUNDHOUSE_TEST_RESULTS"]) && !path.empty?
        File.open(path, "a") { |file| file.puts(JSON.generate(result)) }
      end
      result.fetch("exceptions").each do |error|
        puts "#{error.fetch('status').upcase} #{result.fetch('id')}: #{error.fetch('message')}"
      end
      result
    end
    failed = results.count { |result| %w[fail error].include?(result.fetch("status")) }
    skipped = results.count { |result| result.fetch("status") == "skip" }
    raise "#{klass.name}: #{failed} of #{methods.length} tests failed" if failed > 0
    puts "#{klass.name}: #{skipped} tests skipped" if skipped > 0
    puts "#{klass.name}: #{methods.length - skipped} tests passed"
  end
end
