# Native ActiveSupport 8.1.4 oracle for LogSubscriber::TestHelper in the shape
# Core's query_error_log_subscriber_test uses it: a subscriber attached at boot,
# Active Record's per-thread instrumenter cached by SQL that ran before setup,
# then setup swapping in a fresh notifier and a MockLogger.
require "json"
require "logger"

if ARGV.first == "compare"
  require "open3"
  outputs = ["native", "emitted"].map do |mode|
    stdout, stderr, status = Open3.capture3(RbConfig.ruby, __FILE__, mode)
    abort "#{mode}: #{stderr}" unless status.success?
    stdout
  end
  abort "log subscriber test helper differs:\n#{outputs.join("\n")}" unless outputs.uniq.length == 1
  puts outputs.first
  puts "Native ActiveSupport and emitted LogSubscriber::TestHelper agree"
  exit
end

if ARGV.first == "native"
  gem "activesupport", "8.1.4"
  require "active_support"
  require "active_support/log_subscriber"
  require "active_support/log_subscriber/test_helper"
  # AbstractAdapter#instrumenter, verbatim.
  def active_record_instrumenter
    ActiveSupport::IsolatedExecutionState[:active_record_instrumenter] ||= ActiveSupport::Notifications.instrumenter
  end
else
  require_relative "../runtime/spinel/db_pg_cruby"
  require_relative "../runtime/spinel/active_support_log_subscriber_test_helper"
  def active_record_instrumenter
    Db.active_record_instrumenter
  end
end

class StatementFailure < StandardError; end

class ProbeQueryErrorSubscriber < ActiveSupport::LogSubscriber
  subscribe_log_level :sql, :error

  class << self
    def logger
      @logger ||= Logger.new(IO::NULL)
    end
  end

  def sql(event)
    return unless (exception = event.payload[:exception_object]).is_a?(StatementFailure)

    error("#{exception.message.strip}\nSQL:\n#{event.payload[:sql]}\n")
  end

  def logger
    self.class.logger
  end
end

def execute(sql)
  active_record_instrumenter.instrument("sql.active_record", sql: sql, name: "SQL") do
    raise StatementFailure, "relation does not exist" if sql.include?("missing")
    [1]
  end
end

ActiveSupport::LogSubscriber.logger = Logger.new(IO::NULL)
ProbeQueryErrorSubscriber.attach_to :active_record
execute("SELECT 0") # boot/fixture SQL caches the instrumenter before any test

class ProbeTest
  include ActiveSupport::LogSubscriber::TestHelper
  attr_reader :logger_under_test

  def set_logger(logger)
    ProbeQueryErrorSubscriber.logger = logger
  end

  def run(sql)
    setup
    @logger_under_test = @logger
    begin
      execute(sql)
    rescue StatementFailure
    end
    [@logger.logged(:error).size, @logger.logged(:error).first, @logger.error?, @logger.debug?,
     ActiveSupport::LogSubscriber.colorize_logging]
  ensure
    teardown
  end
end

observed = {}
failing = ProbeTest.new
observed[:failing] = failing.run("SELECT * FROM missing")
observed[:working] = ProbeTest.new.run("SELECT 1")
observed[:after_teardown] = [ProbeQueryErrorSubscriber.instance_variable_get(:@logger).nil?,
                             failing.logger_under_test.flush_count]
# A thread whose first SQL runs after setup binds its instrumenter to the
# test notifier, where the boot-time subscriber is not attached.
fresh = ProbeTest.new
fresh_logged = nil
Thread.new do
  fresh.setup
  begin
    execute("SELECT * FROM missing")
  rescue StatementFailure
  end
  fresh_logged = fresh.instance_variable_get(:@logger).logged(:error).size
  fresh.teardown
end.join
observed[:fresh_thread] = fresh_logged
mock = ActiveSupport::LogSubscriber::TestHelper::MockLogger.new(Logger::WARN)
mock.info { "block" }
mock.warn(" padded ")
mock.flush
observed[:mock] = [mock.logged(:info), mock.logged(:warn), mock.info?, mock.warn?, mock.flush_count]

expected = {
  failing: [1, "relation does not exist\nSQL:\nSELECT * FROM missing", true, true, false],
  working: [0, nil, true, true, false],
  after_teardown: [true, 0],
  fresh_thread: 0,
  mock: [["block"], ["padded"], false, true, 1]
}
abort "unexpected contract: #{observed.inspect}" unless observed == expected
puts JSON.generate(observed)
