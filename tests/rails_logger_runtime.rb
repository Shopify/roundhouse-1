# Native Rails oracle (the Rails native Core runs; tests/support/
# rails_oracle.rb) for `Rails.logger` on the ruby-family trees
# (runtime/spinel/rails_configuration.rb) against railties' bootstrap
# `initialize_logger` (railties 8.2.0.alpha application/bootstrap.rb:34-64).
#
#   ruby tests/rails_logger_runtime.rb compare
#
# For each scenario, makes an app root (a temp dir) and runs this script
# twice from it: `native` boots a minimal Rails app in the test
# environment whose config.root is the dir; `emitted` loads the runtime
# as the emitted tree does (runtime/ruby/rails.rb, whose Rails.root is the
# tree root ".", then the Railtie port and rails_configuration.rb) with
# Rails.env "test". Each then turns Kernel#warn and Warning.warn into
# recorders, logs through Rails.logger in every call form, and prints one
# line per observation, the log file's bytes last; `compare` requires the
# two equal.
#
# Scenarios:
#   default     no logger configuration: log/test.log at :debug
#   info        config.log_level = :info (natively the application's
#               config; emitted, the options store a railtie writes)
#   unwritable  log/ is a file, so the log file cannot be opened: Rails
#               logs its error to STDERR and keeps going
#
# `emitted ROOT refusal` (not compared) sets a config.logger that is no
# ::Logger and logs: the refusal is named on stderr and the default device
# is used.
require "fileutils"
require "tmpdir"
require_relative "support/rails_oracle"

SCENARIOS = %w[default info unwritable].freeze

if ARGV.first == "compare"
  require "open3"
  failures = []
  SCENARIOS.each do |scenario|
    outputs = ["native", "emitted"].map do |mode|
      root = File.realpath(Dir.mktmpdir("rails-logger-oracle"))
      begin
        stdout, stderr, status = Open3.capture3(RbConfig.ruby, File.expand_path(__FILE__), mode, root, scenario, chdir: root)
        abort "#{mode} #{scenario}: #{stderr}" unless status.success?
        stdout
      ensure
        FileUtils.rm_rf(root)
      end
    end
    native, emitted = outputs.map(&:lines)
    if outputs.uniq.length == 1
      puts native.map { |line| "#{scenario}: #{line}" }
    else
      diff = native.zip(emitted).each_with_index.reject { |(x, y), _| x == y }.first(8)
      failures << "#{scenario} (native #{native.length} lines, emitted #{emitted.length}):\n" +
        diff.map { |(x, y), i| "line #{i + 1}\n  native:  #{x.inspect}\n  emitted: #{y.inspect}" }.join("\n")
    end
  end
  abort "Rails.logger differs:\n#{failures.join("\n")}" unless failures.empty?
  puts "Native Rails and the emitted Rails.logger agree (#{RailsOracle.label})"
  exit
end

MODE, ROOT, SCENARIO = ARGV.first(3)
abort "usage: #{$PROGRAM_NAME} compare | native|emitted ROOT SCENARIO" unless %w[native emitted].include?(MODE) && ROOT && SCENARIO

File.write(File.join(ROOT, "log"), "") if SCENARIO == "unwritable"

# The logger Rails falls back to is built on STDERR; reopening it on a
# file, after the libraries load and before the logger is built, keeps its
# lines apart from the process's own stderr.
def capture_stderr
  return unless SCENARIO == "unwritable"

  STDERR.reopen(File.join(ROOT, "stderr.txt"), "w")
  STDERR.sync = true
end

if MODE == "native"
  ENV["RAILS_ENV"] = "test"
  RailsOracle.activate("railties")
  require "rails"
  module LoggerOracle
    class Application < Rails::Application
      config.root = ROOT
      config.eager_load = false
      config.log_level = :info if SCENARIO == "info"
    end
  end
  capture_stderr
  # The oracle's boot loads a second copy of a few activesupport files
  # (already-initialized-constant warnings); they are not the logger's.
  verbose, $VERBOSE = $VERBOSE, nil
  Rails.application.initialize!
  $VERBOSE = verbose
else
  require_relative "../runtime/ruby/rails"
  # In a tree the ports sit beside rails.rb and require it by that name;
  # here rails.rb is already loaded from runtime/ruby.
  $LOADED_FEATURES << File.expand_path("../runtime/spinel/rails.rb", __dir__)
  require_relative "../runtime/spinel/rails_railtie"
  require_relative "../runtime/spinel/rails_configuration"
  Rails.env_name = "test"
  Rails.configuration.log_level = :info if SCENARIO == "info"
  Rails.configuration.logger = Object.new if SCENARIO == "refusal"
  capture_stderr
end

WARNINGS = []
module Warning
  def self.warn(message, **)
    WARNINGS << [:warning, message]
  end
end
module Kernel
  def warn(*messages, **)
    WARNINGS << [:kernel, messages]
  end
end

# Each mode runs in its own app root, shown as ROOT.
def show(label, value)
  puts "#{label}: #{value.inspect.gsub(ROOT, "ROOT")}"
end

logger = Rails.logger
show "memoized", Rails.logger.equal?(logger)
show "level", logger.level
show "predicates", [logger.debug?, logger.info?, logger.warn?, logger.error?, logger.fatal?]
show "formatter", logger.formatter.class.name
show "outputs to log/test.log", ActiveSupport::Logger.logger_outputs_to?(logger, File.join(ROOT, "log/test.log"))
show "outputs to STDERR", ActiveSupport::Logger.logger_outputs_to?(logger, STDERR)
logger.debug("debug line")
logger.info("info line")
logger.warn("x")
logger.error("error line")
logger.fatal("fatal line")
logger.unknown("unknown line")
logger.info { "info block" }
called = false
logger.debug { called = true; "debug block" }
show "debug block called", called
logger.warn("progname") { "warn block with a progname" }
logger.add(::Logger::ERROR, "added")
logger.add(::Logger::WARN) { "added block" }
logger.info([1, "two"])
logger.info(nil)
logger << "raw\n"
show "silence", logger.silence { logger.warn("silenced"); logger.error("not silenced"); :answered }
logger.level = :warn
show "level after level=", logger.level
logger.info("dropped after level=")
logger.warn("kept after level=")
logger.level = :debug
logger.debug("debug after level=")
show "warnings", WARNINGS
replacement = ::Logger.new(nil)
Rails.logger = replacement
show "assigned", Rails.logger.equal?(replacement)
Rails.logger = nil
show "assigned nil", Rails.logger
if SCENARIO == "unwritable"
  show "stderr", File.read(File.join(ROOT, "stderr.txt"))
else
  show "log", File.read(File.join(ROOT, "log/test.log"))
end
