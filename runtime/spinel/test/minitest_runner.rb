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
        test.__roundhouse_before_setup
        test.setup
      end
      capture(errors, "test") { test.public_send(name) } if errors.empty?
      capture(errors, "teardown") { test.teardown }
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
