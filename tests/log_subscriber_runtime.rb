# Native ActiveSupport oracle for reusable subscriber lifecycle and logging.
require "json"
require "logger"
require "stringio"

if ARGV.first == "compare"
  require "open3"
  outputs = ["native", "emitted"].map do |mode|
    stdout, stderr, status = Open3.capture3(RbConfig.ruby, __FILE__, mode)
    abort "#{mode}: #{stderr}" unless status.success?
    stdout
  end
  abort "log subscriber differs:\n#{outputs.join("\n")}" unless outputs.uniq.length == 1
  puts outputs.first
  puts "Native ActiveSupport and emitted log subscriber agree"
  exit
end

if ARGV.first == "native"
  gem "activesupport", "8.1.4"
  require "active_support"
  require "active_support/log_subscriber"
else
  require_relative "../runtime/spinel/active_support_log_subscriber"
end

class ContractLogSubscriber < ActiveSupport::LogSubscriber
  subscribe_log_level :query, :error

  def query(event)
    error("#{event.payload[:sql]}:#{event.payload[:exception_object].message}") if event.payload[:exception_object]
  end
end

output = StringIO.new
logger = Logger.new(output)
logger.formatter = ->(severity, time, progname, message) { "#{severity}:#{message}\n" }
ActiveSupport::LogSubscriber.logger = logger
ContractLogSubscriber.attach_to(:contract)
notifications = ActiveSupport::Notifications
notifications.instrument("query.contract", sql: "SELECT 1")
begin
  notifications.instrument("query.contract", sql: "SELECT missing") { raise "missing relation" }
rescue RuntimeError
end
observed = { errors: output.string.lines }
logger.level = Logger::FATAL
begin
  notifications.instrument("query.contract", sql: "suppressed") { raise "suppressed" }
rescue RuntimeError
end
observed[:level_filter] = output.string.lines.length == 1
logger.level = Logger::DEBUG

class ContractLogSubscriber
  def added(event)
    info(event.payload[:message])
  end
end
notifications.instrument("added.contract", message: "added after attach")
observed[:method_added] = output.string.lines.last
ContractLogSubscriber.detach_from(:contract)
notifications.instrument("added.contract", message: "must not log")
observed[:detached] = output.string.lines.length == 2

old_notifier = notifications.notifier
notifications.notifier = ActiveSupport::Notifications::Fanout.new
ContractLogSubscriber.attach_to(:contract)
notifications.instrument("added.contract", message: "new notifier")
ContractLogSubscriber.detach_from(:contract)
notifications.notifier = old_notifier
observed[:notifier_swap] = output.string.lines.last

expected = {
  errors: ["ERROR:SELECT missing:missing relation\n"],
  level_filter: true,
  method_added: "INFO:added after attach\n",
  detached: true,
  notifier_swap: "INFO:new notifier\n"
}
abort "unexpected contract: #{observed.inspect}" unless observed == expected

if ARGV.first == "emitted"
  # Handler failures require the framework error reporter. Until that exists,
  # reject the path by name instead of pretending that reporting happened.
  class ReportingGapSubscriber < ActiveSupport::LogSubscriber
    def unsupported(event)
      raise "handler failed"
    end
  end
  ReportingGapSubscriber.attach_to(:gap)
  begin
    notifications.instrument("unsupported.gap")
    abort "missing handler error-reporting refusal"
  rescue NotImplementedError => error
    abort error.message unless error.message.include?("handler error reporting is not implemented")
  ensure
    ReportingGapSubscriber.detach_from(:gap)
  end
end
puts JSON.generate(observed)
