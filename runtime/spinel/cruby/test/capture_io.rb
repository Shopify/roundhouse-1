
# Minitest's `capture_io`, for the CRuby tree only: it swaps the
# `$stdout`/`$stderr` globals for StringIOs around the block, which the
# Spinel tree cannot. `project.rs::apply_test_harness_methods` appends
# this to the CRuby harness; a target whose harness does not declare it
# is refused at the call.
require "stringio"

class TestBase
  def capture_io
    captured_stdout = StringIO.new
    captured_stderr = StringIO.new
    orig_stdout = $stdout
    orig_stderr = $stderr
    begin
      $stdout = captured_stdout
      $stderr = captured_stderr
      yield
    ensure
      $stdout = orig_stdout
      $stderr = orig_stderr
    end
    [captured_stdout.string, captured_stderr.string]
  end
end
