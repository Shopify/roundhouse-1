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

  # ActiveSupport's names for the same primitive assertions.
  alias assert_not refute
  alias assert_not_equal refute_equal
  alias assert_not_nil refute_nil
  alias assert_not_empty refute_empty
  alias assert_not_includes refute_includes
  alias assert_not_same refute_same
  alias assert_no_match refute_match
  alias assert_raise assert_raises

  # Minitest's capture_io serializes global IO replacement with this hook.
  def _synchronize(&block)
    Minitest::Test.io_lock.synchronize(&block)
  end

  # mocha/minitest's lifecycle (Mocha::Integration::Minitest::Adapter),
  # which TestBase wires by hand: mocha_setup first in its setup,
  # mocha_verify in its teardown, mocha_teardown in that teardown's
  # ensure. These keep the adapter's conditions on the three calls.
  #
  # mocha_verify runs only when the test has passed so far
  # (`return unless passed?` in the adapter's before_teardown).
  #
  # Expectations count as assertions through Mocha's AssertionCounter,
  # as under the adapter; native Minitest's assertion count includes
  # them. Mocha 2 (Core's lock: 2.5.0) takes the counter in mocha_verify,
  # Mocha 3 in mocha_setup; the hook whose arity accepts it gets it.
  #
  # mocha_verify and mocha_teardown run only after this test's
  # mocha_setup. The runner's fixture reset runs before TestBase#setup,
  # as ActiveRecord::TestFixtures#before_setup runs before the adapter's
  # before_setup; when the reset raised, there is no Mockery to verify or
  # tear down, and Mockery.teardown popped a stack nothing had pushed:
  # every errored row carried a second NoMethodError ("undefined method
  # 'pop' for nil") from TestBase#teardown.
  def mocha_setup(*args)
    super(*__roundhouse_mocha_counter(:mocha_setup, args))
    @__roundhouse_mocha_set_up = true
  end

  def mocha_verify(*args)
    return unless @__roundhouse_mocha_set_up && __roundhouse_passed?
    super(*__roundhouse_mocha_counter(:mocha_verify, args))
  end

  def __roundhouse_mocha_counter(hook, args)
    return args unless args.empty? && Mocha::Hooks.instance_method(hook).arity != 0
    [Mocha::Integration::AssertionCounter.new(self)]
  end

  def mocha_teardown(*args)
    return unless @__roundhouse_mocha_set_up
    @__roundhouse_mocha_set_up = false
    super
  end

  # Minitest::Test#passed?: no failure, error or skip recorded so far.
  # The runner hands the test its live exception list.
  attr_writer :__roundhouse_errors

  def __roundhouse_passed?
    @__roundhouse_errors.nil? || @__roundhouse_errors.empty?
  end
end

# The adapter's include hook: an unmet expectation is a Minitest
# failure, as under mocha/minitest.
if defined?(Mocha::API)
  require "mocha/integration/assertion_counter"
  require "mocha/expectation_error_factory"
  Mocha::ExpectationErrorFactory.exception_class = Minitest::Assertion
end

# Prepend to the framework base, not each authored test class. Authored helper
# methods/overrides and setup/teardown super chains retain their precedence.
TestBase.prepend(RoundhouseMinitestAssertions)

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
    if test
      # Live: a phase's failure is visible to the next (mocha_verify's
      # passed? check), as Minitest's failures list is.
      test.__roundhouse_errors = errors
      capture(errors, "setup") do
        yield if block_given?
        test.__roundhouse_setup_fixtures
        # ActiveRecord::TestFixtures#setup_fixtures' transactions, where
        # the tree carries them (the PostgreSQL tree's
        # runtime/active_record_test_fixtures.rb).
        if test.respond_to?(:__roundhouse_setup_transactional_fixtures)
          test.__roundhouse_setup_transactional_fixtures(name)
        end
        test.__roundhouse_before_setup
        test.setup
      end
      capture(errors, "test") { test.public_send(name) } if errors.empty?
      capture(errors, "teardown") { test.teardown }
      # Minitest's after_teardown hook, where something defines it
      # (ActiveSupport's TimeHelpers travels back there).
      capture(errors, "teardown") { test.after_teardown } if test.respond_to?(:after_teardown)
      # TestFixtures#after_teardown's teardown_fixtures, last.
      if test.respond_to?(:__roundhouse_teardown_fixtures)
        capture(errors, "teardown") { test.__roundhouse_teardown_fixtures(name) }
      end
    end
    # Keep all phase failures. Error wins over failure/skip; a teardown error
    # cannot turn into a pass or disappear behind an earlier skip.
    statuses = errors.map { |error| error.fetch("status") }
    status = %w[error fail skip].find { |kind| statuses.include?(kind) } || "pass"
    {"schema" => "roundhouse-test-result/v1", "id" => "#{klass.name}##{name}", "status" => status,
     "minitest_version" => Gem.loaded_specs.fetch("minitest").version.to_s,
     "minitest_runtime_version" => Minitest::VERSION,
     "assertions" => test ? test.assertions : 0, "exceptions" => errors,
     "assertion_count_scope" => "minitest"}
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
