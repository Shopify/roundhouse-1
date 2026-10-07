# Focused runtime contract against real ActiveSupport, without booting Core.
require "json"

if ARGV.first == "compare"
  require "open3"
  outputs = ["native", "emitted"].map do |mode|
    stdout, stderr, status = Open3.capture3(RbConfig.ruby, __FILE__, mode)
    abort "#{mode}: #{stderr}" unless status.success?
    stdout
  end
  abort "notification contract differs:\n#{outputs.join("\n")}" unless outputs.uniq.length == 1
  puts outputs.first
  puts "Native ActiveSupport and emitted notification runtime agree"
  exit
end

if ARGV.first == "native"
  # The ActiveSupport native Core runs (RH_RAILS_ORACLE overrides it).
  require_relative "support/rails_oracle"
  RailsOracle.activate("activesupport")
  require "active_support"
  require "active_support/notifications"
else
  require_relative "../runtime/spinel/active_support_notifications"
end

notifications = ActiveSupport::Notifications
observed = {}
timed = []
listener = notifications.subscribe("contract") do |name, started, finished, id, payload|
  timed << [name, started.is_a?(Time), finished.is_a?(Time), id.match?(/\A[0-9a-f]{20}\z/), payload[:value]]
end
result = notifications.instrument("contract", value: 1) { :result }
observed[:synchronous] = [result, timed]
notifications.unsubscribe(listener)

event = nil
notifications.subscribed(->(value) { event = value }, "contract") do
  notifications.instrument("contract", value: 2)
end
observed[:event] = [event.name, event.payload[:value], event.time.is_a?(Numeric),
                   event.end >= event.time, event.duration >= 0]

clock = []
notifications.subscribed(->(name, started, finished, id, payload) {
  clock << [started.is_a?(Float), finished.is_a?(Float), finished >= started]
}, "contract", monotonic: true) { notifications.instrument("contract") }
observed[:monotonic] = clock

failure = RuntimeError.new("expected failure")
failed_event = nil
begin
  notifications.subscribed(->(value) { failed_event = value }, "contract") do
    notifications.instrument("contract") { raise failure }
  end
rescue RuntimeError => error
  observed[:exception] = [error.equal?(failure), failed_event.payload[:exception_object].equal?(error),
                          failed_event.payload[:exception]]
end
notifications.instrument("contract", value: 3)
observed[:scoped_cleanup] = failed_event.payload[:exception_object].equal?(failure)

matches = []
regexp = notifications.subscribe(/contract/) { |value| matches << value.name }
notifications.instrument("contract.one")
notifications.unsubscribe("contract.one")
notifications.instrument("contract.one")
notifications.instrument("contract.two")
notifications.unsubscribe(regexp)
observed[:pattern_unsubscribe] = matches

ids = []
listener = notifications.subscribe("thread.contract") { |value| ids << value.transaction_id }
notifications.instrument("thread.contract")
Thread.new { notifications.instrument("thread.contract") }.join
notifications.unsubscribe(listener)
observed[:thread_identity] = ids.uniq.length == 2

# Delivery order: the name's String subscribers (a prepended one first),
# then Regexp and catch-all ones, whatever the subscription order.
order = []
subscriptions = [
  notifications.subscribe(/order\.contract/) { order << :regexp },
  notifications.subscribe("order.contract") { order << :first },
  notifications.subscribe("order.contract") { order << :second },
  notifications.subscribe("order.contract", prepend: true) { order << :prepended },
]
notifications.instrument("order.contract")
subscriptions.each { |subscription| notifications.unsubscribe(subscription) }
prepend_regexp = begin
  notifications.subscribe(/order/, prepend: true) { nil }
  :subscribed
rescue ArgumentError => error
  error.message
end
observed[:order] = [order, prepend_regexp]

expected = {
  synchronous: [:result, [["contract", true, true, true, 1]]],
  event: ["contract", 2, true, true, true],
  monotonic: [[true, true, true]],
  exception: [true, true, ["RuntimeError", "expected failure"]],
  scoped_cleanup: true,
  pattern_unsubscribe: ["contract.one", "contract.two"],
  thread_identity: true,
  order: [[:prepended, :first, :second, :regexp], "Cannot prepend Regex subscribers"]
}
abort "unexpected contract: #{observed.inspect}" unless observed == expected
puts JSON.generate(observed)
