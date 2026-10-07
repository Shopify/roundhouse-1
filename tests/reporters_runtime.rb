# Native ActiveSupport oracle (the Rails native Core runs; tests/support/
# rails_oracle.rb) for `Rails.event` (EventReporter) and
# `Rails.error` (ErrorReporter) against runtime/ruby/active_support_reporters.rb.
# Each mode prints one line per observation; `compare` requires them equal.
require "json"
require_relative "support/rails_oracle"

if ARGV.first == "compare"
  require "open3"
  outputs = ["native", "emitted"].map do |mode|
    stdout, stderr, status = Open3.capture3(RbConfig.ruby, __FILE__, mode)
    abort "#{mode}: #{stderr}" unless status.success?
    stdout
  end
  unless outputs.uniq.length == 1
    a, b = outputs.map(&:lines)
    diff = a.zip(b).each_with_index.reject { |(x, y), _| x == y }.first(5)
    abort "reporters differ:\n" + diff.map { |(x, y), i| "line #{i + 1}\n  native:  #{x}  emitted: #{y}" }.join("\n")
  end
  puts outputs.first
  puts "Native ActiveSupport and emitted Rails.event/Rails.error agree (#{RailsOracle.label})"
  exit
end

if ARGV.first == "native"
  RailsOracle.activate("activesupport")
  require "active_support"
  require "active_support/event_reporter"
  require "active_support/error_reporter"
  require "active_support/execution_context"
  require "active_support/core_ext/hash/keys" # loaded by every Rails app
  # railties lib/rails.rb, verbatim.
  module Rails
    class << self
      def error
        ActiveSupport.error_reporter
      end

      def event
        ActiveSupport.event_reporter
      end
    end
  end
else
  require_relative "../runtime/ruby/rails"
end

def show(label, value)
  puts "#{label}: #{value.inspect}"
end

def outcome
  yield.inspect
rescue Exception => e
  "#{e.class}: #{e.message.lines.first.to_s.strip}"
end

class EventSink
  attr_reader :events

  def initialize
    @events = []
  end

  def emit(event)
    @events << event
  end
end

class ErrorSink
  attr_reader :reports

  def initialize
    @reports = []
  end

  def report(error, handled:, severity:, context:, source:)
    @reports << [error.class.name, error.message, handled, severity, context, source]
  end
end

class RaisingSink
  def emit(_event)
    raise "sink failed"
  end

  def report(_error, handled:, severity:, context:, source:)
    raise "subscriber failed"
  end
end

class FatalLog
  attr_reader :lines

  def initialize
    @lines = []
  end

  def fatal(message)
    @lines << message.lines.first.strip
  end
end

OrderPlaced = Struct.new(:id)

def event_view(event)
  loc = event[:source_location]
  [event[:name], event[:payload], event[:tags], event[:context],
   event[:timestamp].is_a?(Integer),
   loc && [File.basename(loc[:filepath]), loc[:lineno], loc[:label]]]
end

errors = ErrorSink.new
Rails.error.subscribe(errors)
events = EventSink.new
Rails.event.subscribe(events)

# notify: names, payload shapes, caller location.
Rails.event.notify("order.placed", id: 1, "total" => 9)
Rails.event.notify(:order_paid, { "id" => 2, amount: 3 })
Rails.event.notify(OrderPlaced.new(4))
Rails.event.notify("bare")
events.events.each { |e| show("event", event_view(e)) }
events.events.clear

# Payload filter from ActiveSupport.filter_parameters.
ActiveSupport.filter_parameters = [:password, "card.code", /secr/]
Rails.event.reload_payload_filter
Rails.event.notify("login", password: "p", user: { password: "q", name: "n" }, card: { code: 1, kind: "v" },
  list: [{ password: "x" }, 3], secret_key: "s", mypassword_hint: "h")
show("filtered", events.events.last[:payload])
ActiveSupport.filter_parameters = []
Rails.event.reload_payload_filter
events.events.clear

# Tags and context.
Rails.event.tagged("graphql", { region: "ca" }, shard: 3) do
  Rails.event.tagged(:inner, OrderPlaced.new(7)) { Rails.event.notify("tagged") }
  Rails.event.notify("outer")
end
Rails.event.notify("untagged")
Rails.event.set_context(request_id: "r1", "shop_id" => 5)
Rails.event.set_context(request_id: "r2")
Rails.event.notify("in_context")
show("context", Rails.event.context)
Rails.event.clear_context
Rails.event.notify("cleared")
events.events.each { |e| show("tags/context", [e[:name], e[:tags], e[:context]]) }
events.events.clear

# Debug events.
show("debug_mode?", Rails.event.debug_mode?)
show("debug off", Rails.event.debug("dbg", a: 1))
Rails.event.with_debug do
  show("debug_mode? in with_debug", Rails.event.debug_mode?)
  Rails.event.debug("dbg", a: 1)
  Rails.event.debug("dbg.block", a: 1) { { b: 2 } }
end
show("debug_mode? after", Rails.event.debug_mode?)
events.events.each { |e| show("debug event", event_view(e)) }
events.events.clear

# Filtered subscription and unsubscribe.
only_paid = EventSink.new
Rails.event.subscribe(only_paid) { |event| event[:name].end_with?(".paid") }
Rails.event.notify("a.paid")
Rails.event.notify("a.placed")
show("filtered subscriber", only_paid.events.map { |e| e[:name] })
Rails.event.unsubscribe(only_paid)
Rails.event.notify("b.paid")
show("after unsubscribe", only_paid.events.length)
show("subscribers", Rails.event.subscribers.length)
show("subscribe without emit", outcome { Rails.event.subscribe(Object.new) })
events.events.clear

# Subscriber errors and unexpected arguments go to Rails.error.
Rails.event.subscribe(RaisingSink.new)
Rails.event.notify("boom")
Rails.event.raise_on_error = true
show("raise_on_error", outcome { Rails.event.notify("boom") })
show("raise_on_error args", outcome { Rails.event.notify("x", { a: 1 }, b: 2) })
Rails.event.raise_on_error = false
Rails.event.unsubscribe(RaisingSink)
Rails.event.notify("x", { a: 1 }, b: 2)
Rails.event.notify(OrderPlaced.new(1), c: 3)
errors.reports.each { |r| show("event error", r) }
errors.reports.clear
show("events after errors", events.events.map { |e| [e[:name], e[:payload].is_a?(OrderPlaced) ? :object : e[:payload]] })
Rails.event.unsubscribe(events)

# report: severity, context, middleware, source, idempotence, cause chain.
Rails.error.report(ArgumentError.new("plain"))
Rails.error.report(KeyError.new("unhandled"), handled: false)
Rails.error.set_context(shop_id: 9)
Rails.error.add_middleware(->(error, context:, handled:, severity:, source:) { context.merge(mw: [error.message, handled, severity, source]) })
Rails.error.report(RuntimeError.new("ctx"), severity: :info, context: { job: "j" }, source: "app.job")
Rails.error.report(RuntimeError.new("nil ctx"), context: nil)
twice = RuntimeError.new("twice")
Rails.error.report(twice)
Rails.error.report(twice)
inner = begin
  begin
    raise IOError, "inner"
  rescue IOError
    raise TypeError, "outer"
  end
rescue TypeError => e
  e
end
Rails.error.report(inner)
Rails.error.report(inner.cause)
fresh = StandardError.new("no backtrace")
Rails.error.report(fresh)
show("backtrace ensured", [fresh.backtrace.nil?, File.basename(fresh.backtrace.first.to_s.split(":").first.to_s)])
frozen = StandardError.new("frozen").freeze
Rails.error.report(frozen)
Rails.error.report(frozen)
show("bad severity", outcome { Rails.error.report(RuntimeError.new("s"), severity: :fatal) })
show("not an exception", outcome { Rails.error.report("text") })
errors.reports.each { |r| show("report", r) }
errors.reports.clear
ActiveSupport::ExecutionContext.clear

# handle / record / unexpected.
show("handle value", Rails.error.handle { 42 })
show("handle default", Rails.error.handle { raise ArgumentError, "h1" })
show("handle fallback", Rails.error.handle(ArgumentError, fallback: -> { :fell_back }) { raise ArgumentError, "h2" })
show("handle unlisted", outcome { Rails.error.handle(ArgumentError) { raise KeyError, "h3" } })
show("handle exception", outcome { Rails.error.handle { raise NotImplementedError, "h4" } })
show("handle options", Rails.error.handle(KeyError, severity: :error, context: { k: 1 }, source: "s") { raise KeyError, "h5" })
show("record value", Rails.error.record { :ok })
show("record raise", outcome { Rails.error.record(ArgumentError, context: { r: 1 }) { raise ArgumentError, "r1" } })
show("record unlisted", outcome { Rails.error.record(KeyError) { raise ArgumentError, "r2" } })
show("unexpected string", Rails.error.unexpected("u1"))
show("unexpected error", Rails.error.unexpected(KeyError.new("u2"), context: { u: 2 }))
Rails.error.debug_mode = true
show("unexpected debug", outcome { Rails.error.unexpected("u3") })
debug_error = begin
  Rails.error.unexpected(KeyError.new("u4"))
rescue Exception => e
  e
end
show("unexpected debug cause", [debug_error.class.ancestors.include?(StandardError), debug_error.cause.class.name, debug_error.cause.message])
show("UnexpectedError constant", debug_error.is_a?(ActiveSupport::ErrorReporter::UnexpectedError))
Rails.error.debug_mode = false
errors.reports.each { |r| show("handled report", r) }
errors.reports.clear

# disable, set_context with a block, subscriber failures, unsubscribe.
Rails.error.disable(errors) { Rails.error.report(RuntimeError.new("disabled")) }
Rails.error.disable(ErrorSink) { Rails.error.report(RuntimeError.new("disabled by class")) }
Rails.error.set_context(a: 1)
Rails.error.set_context(a: 2, b: 3) { Rails.error.report(RuntimeError.new("block ctx")) }
Rails.error.report(RuntimeError.new("after block ctx"))
show("execution context", ActiveSupport::ExecutionContext.to_h)
ActiveSupport::ExecutionContext.clear
raising = RaisingSink.new
Rails.error.subscribe(raising)
show("subscriber raises", outcome { Rails.error.report(RuntimeError.new("r")) })
log = FatalLog.new
Rails.error.logger = log
Rails.error.report(RuntimeError.new("logged"))
show("fatal log", log.lines)
Rails.error.logger = nil
Rails.error.unsubscribe(raising)
show("subscribe without report", outcome { Rails.error.subscribe(Object.new) })
show("middleware without call", outcome { Rails.error.add_middleware(Object.new) })
errors.reports.each { |r| show("late report", r) }
errors.reports.clear
Rails.error.unsubscribe(errors)
Rails.error.report(RuntimeError.new("nobody"))
show("after unsubscribe", errors.reports.length)

# Standalone reporters and ExecutionContext.
standalone = ActiveSupport::ErrorReporter.new(ErrorSink.new)
show("standalone", standalone.report(RuntimeError.new("solo")))
reporter = ActiveSupport::EventReporter.new(EventSink.new, raise_on_error: true)
show("standalone events", [reporter.subscribers.length, outcome { reporter.notify("x", { a: 1 }, b: 2) }])
ActiveSupport::ExecutionContext[:k] = 1
ActiveSupport::ExecutionContext.set(k: 2, "j" => 3) { show("set block", ActiveSupport::ExecutionContext.to_h) }
show("set restored", ActiveSupport::ExecutionContext.to_h)
ActiveSupport::ExecutionContext.push
show("push (not nestable)", ActiveSupport::ExecutionContext.to_h)
ActiveSupport::ExecutionContext.nestable = true
ActiveSupport::ExecutionContext[:outer] = 1
ActiveSupport::ExecutionContext.push
ActiveSupport::ExecutionContext[:inner] = 2
show("nested push", ActiveSupport::ExecutionContext.to_h)
ActiveSupport::ExecutionContext.pop
show("nested pop", ActiveSupport::ExecutionContext.to_h)

# --- 8.2 --------------------------------------------------------------------
# A new reporter starts in debug mode (Rails' boot then sets it from the
# log level).
fresh_sink = EventSink.new
fresh = ActiveSupport::EventReporter.new(fresh_sink)
show("fresh debug_mode?", fresh.debug_mode?)
fresh.debug("fresh.debug", a: 1)
show("fresh debug event", fresh_sink.events.map { |e| [e[:name], e[:payload]] })

# Subscriber filters see the name alone, before the payload is built.
seen = []
filtered_sink = EventSink.new
filtering = ActiveSupport::EventReporter.new
filtering.subscribe(filtered_sink) { |event| seen << event.keys; event[:name].start_with?("keep") }
filtering.notify("keep.one", a: 1)
filtering.notify("drop.one", a: 2)
show("filter sees", seen)
show("filter delivers", filtered_sink.events.map { |e| [e[:name], e[:payload]] })

# With no subscriber left nothing is resolved: the mixed-arguments
# error is not reported.
strict = ActiveSupport::EventReporter.new(raise_on_error: true)
show("no subscribers", outcome { strict.notify("x", { a: 1 }, b: 2) })
strict.subscribe(EventSink.new) { |event| event[:name] == "y" }
show("filtered out", outcome { strict.notify("x", { a: 1 }, b: 2) })
show("filtered in", outcome { strict.notify("y", { a: 1 }, b: 2) })
raising_filter = ActiveSupport::EventReporter.new
raising_filter.subscribe(EventSink.new) { |_event| raise "filter failed" }
show("raising filter", outcome { raising_filter.notify("z") })

# filter_payload: false skips the redaction; EventReporter.filter_parameters
# replaces ActiveSupport.filter_parameters for the reporter.
ActiveSupport.filter_parameters = [:password]
redacting_sink = EventSink.new
redacting = ActiveSupport::EventReporter.new(redacting_sink)
redacting.notify("f", password: "p", name: "n")
redacting.notify("f", { "password" => "p" }, filter_payload: false)
redacting.debug("f", password: "p", filter_payload: false)
redacting.debug("f", password: "p") { { token: "t" } }
ActiveSupport::EventReporter.filter_parameters = [:token]
redacting.reload_payload_filter
redacting.notify("f", password: "p", token: "t")
ActiveSupport::EventReporter.filter_parameters = []
redacting.reload_payload_filter
redacting.notify("f", password: "p", token: "t")
ActiveSupport::EventReporter.filter_parameters = nil
redacting.reload_payload_filter
redacting.notify("f", password: "p", token: "t")
ActiveSupport.filter_parameters = []
show("filter_payload", redacting_sink.events.map { |e| e[:payload] })

# Each error subscriber gets its own copy of the context.
class MutatingSink
  def report(_error, handled:, severity:, context:, source:)
    context[:mutated] = true
    context[:list] << :mutated
  end
end
copies = ErrorSink.new
copying = ActiveSupport::ErrorReporter.new(MutatingSink.new, copies)
shared = [1]
copying.report(RuntimeError.new("ctx"), context: { list: shared, "str" => +"s" })
show("context copies", [copies.reports.last[4], shared])
show("severities frozen", ActiveSupport::ErrorReporter::SEVERITIES.frozen?)
