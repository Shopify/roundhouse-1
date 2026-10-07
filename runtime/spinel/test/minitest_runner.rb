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
end

# Prepend to the framework base, not each authored test class. Authored helper
# methods/overrides and setup/teardown super chains retain their precedence.
TestBase.prepend(RoundhouseMinitestAssertions)

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
    errors << {"phase" => phase, "status" => kind,
               "class" => original.class.name, "message" => original.message}
  end

  def self.run_one(klass, name)
    errors = []
    test = nil
    capture(errors, "initialize") { test = klass.new }
    if test
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
