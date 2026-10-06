# Native Rails 8.1.4 oracle for `config.filter_parameters` against the
# emitted app (tests/filter_parameters_runtime.rs).
#
# `native APP_DIR` boots a real Rails 8.1.4 application from APP_DIR
# (its config/application.rb and config/initializers/*.rb, run by
# railties itself) and prints the observations below. The emitted side
# loads this file after the emitted main.rb has booted, with no
# arguments, and prints the same observations; the Rust test requires
# the two outputs equal.
if ARGV.first == "native"
  gem "railties", "8.1.4"
  require "active_support/parameter_filter"
  ENV["RAILS_ENV"] = "test"
  require File.join(File.expand_path(ARGV[1]), "config/application")
  Rails.application.initialize!
end

# A Proc prints by shape: its inspect carries a source location, which
# differs between the two trees by construction.
def describe_filter(filter)
  filter.is_a?(Proc) ? "Proc(arity=#{filter.arity}, lambda=#{filter.lambda?})" : filter.inspect
end

class FilterOracleSink
  attr_reader :events

  def initialize
    @events = []
  end

  def emit(event)
    @events << event
  end
end

# What `active_support.set_filter_parameters` handed ActiveSupport
# (activesupport railtie.rb:82-87), in order.
puts "filters: #{ActiveSupport.filter_parameters.map { |f| describe_filter(f) }.join(" ")}"

payload = {
  "password" => "p", "email" => "e@example.com", "name" => "n",
  "card" => { "code" => "123", "number" => "4242" }, "code" => "c",
  "nickname" => "abc", "otp_attempt" => "9", "card_number" => "1111",
  "items" => [{ "secret" => "s", "title" => "t" }], "public_key" => "k",
}

# The event reporter's payload filter (event_reporter.rb:548-553), built
# from ActiveSupport.filter_parameters after the railtie reloaded it.
sink = FilterOracleSink.new
Rails.event.subscribe(sink)
Rails.event.notify("filter_oracle.observed", payload)
puts "event payload: #{sink.events.last[:payload].inspect}"

puts "parameter filter: #{ActiveSupport::ParameterFilter.new(ActiveSupport.filter_parameters).filter(payload).inspect}"
