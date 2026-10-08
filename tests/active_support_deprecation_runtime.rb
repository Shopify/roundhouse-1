# Native Rails oracle (the Rails native Core runs; tests/support/
# rails_oracle.rb) for ActiveSupport::Deprecation, its Deprecators
# collection, `Rails.deprecator`, `ActiveSupport.deprecator` and
# `Rails.application.deprecators` against
# runtime/spinel/active_support_deprecation.rb.
#
#   ruby tests/active_support_deprecation_runtime.rb compare
#
# runs this script twice: `native` boots a minimal Rails app whose test
# environment writes `config.active_support.deprecation = BEHAVIOR` (Core's
# config/environments/test.rb writes :silence); `emitted` loads the runtime
# as the emitted PostgreSQL tree does (runtime/ruby/rails.rb, the port) and
# applies the same key as boot.rb does after the environment program. Each
# prints one line per observation; `compare` requires them equal.
require "json"
require "stringio"
require_relative "support/rails_oracle"

if ARGV.first == "compare"
  require "open3"
  outputs = ["native", "emitted"].map do |mode|
    env = mode == "emitted" ? RailsOracle.core_gems_env : {}
    stdout, stderr, status = Open3.capture3(env, RbConfig.ruby, File.expand_path(__FILE__), mode)
    abort "#{mode}: #{stderr}" unless status.success?
    stdout
  end
  native, emitted = outputs.map(&:lines)
  equal = native.zip(emitted).count { |x, y| x == y }
  unless outputs.uniq.length == 1
    diff = native.zip(emitted).each_with_index.reject { |(x, y), _| x == y }.first(8)
    abort "deprecations differ (#{equal}/#{native.length} observations equal, emitted has #{emitted.length}):\n" +
      diff.map { |(x, y), i| "line #{i + 1}\n  native:  #{x}  emitted: #{y}" }.join("\n")
  end
  puts outputs.first
  puts "observations: #{equal}/#{native.length} equal"
  puts "Native Rails and emitted ActiveSupport::Deprecation agree (#{RailsOracle.label})"
  exit
end

MODE = ARGV.first
abort "usage: #{$PROGRAM_NAME} compare | native | emitted" unless %w[native emitted].include?(MODE)

if MODE == "native"
  RailsOracle.activate("railties")
  require "rails"
  require "logger"
  module DeprecationOracle
    class Application < Rails::Application
      config.load_defaults 8.1
      config.root = Dir.pwd
      config.eager_load = false
      config.logger = Logger.new(nil)
      # Core's config/environments/test.rb.
      config.active_support.deprecation = :silence
    end
  end
  Rails.application.initialize!
else
  require_relative "../runtime/ruby/rails"
  require_relative "../runtime/spinel/active_support_notifications"
  require_relative "../runtime/spinel/active_support_deprecation"
  # boot.rb, after the environment program (project::deprecation_behavior_block).
  Rails::Application.apply_deprecation_behavior(report_deprecations: nil, deprecation: :silence, disallowed_deprecation: nil, disallowed_deprecation_warnings: nil)
end

def show(label, value)
  puts "#{label}: #{value.inspect}"
end

def outcome
  yield.inspect
rescue Exception => e
  "#{e.class}: #{e.message}"
end

# The stderr a block writes.
def stderr_of
  saved = $stderr
  $stderr = StringIO.new
  yield
  $stderr.string
ensure
  $stderr = saved
end

# A deprecation message names its caller by file and line; this file is
# the same in both modes.
def site(text)
  text.to_s.gsub(__FILE__, "<THIS>")
end

# The framework deprecators boot registers: railties and Active Support
# first, as Rails' railties initializers run.
names = []
Rails.application.deprecators.each { |d| names << d.gem_name }
show "boot deprecators", names.first(2)
show "boot railties", Rails.application.deprecators[:railties].equal?(Rails.deprecator)
show "boot active_support", Rails.application.deprecators[:active_support].equal?(ActiveSupport.deprecator)
show "memoized", Rails.application.deprecators.equal?(Rails.application.deprecators)
show "Rails.deprecator", [Rails.deprecator.gem_name, Rails.deprecator.deprecation_horizon]
show "ActiveSupport.deprecator", [ActiveSupport.deprecator.gem_name, ActiveSupport.deprecator.deprecation_horizon, ActiveSupport.deprecator.equal?(ActiveSupport::Deprecation._instance)]
show "framework silenced by config", stderr_of { Rails.deprecator.warn("framework") }
show "framework behavior is silence", Rails.deprecator.behavior.size

# A gem's deprecator, as shopify-i18n / country_db / hedwig build theirs.
gem = ActiveSupport::Deprecation.new("5.0.0", "ShopifyI18n")
show "gem fields", [gem.gem_name, gem.deprecation_horizon, gem.silenced, gem.debug, gem.disallowed_warnings]
show "default behavior size", gem.behavior.size
show "default stderr", site(stderr_of { gem.warn("old thing") })
show "default stderr answer", site(stderr_of { $answer = gem.warn("answered") } && $answer)
show "default message", site(stderr_of { gem.warn })
show "deprecation_warning", site(stderr_of { gem.deprecation_warning(:old_method) })
show "deprecation_warning symbol", site(stderr_of { gem.deprecation_warning(:old_method, :new_method) })
show "deprecation_warning string", site(stderr_of { gem.deprecation_warning(:old_method, "use the other") })
show "explicit callstack", stderr_of { gem.warn("given", caller_locations(0, 1)) }.include?("given")

# Registered after boot: the collection applies the app's :silence.
Rails.application.deprecators[:shopify_i18n] = gem
show "registered", Rails.application.deprecators[:shopify_i18n].equal?(gem)
show "registered silenced", stderr_of { gem.warn("now quiet") }
show "registered answer", outcome { gem.warn("quiet answer").to_s.start_with?("DEPRECATION WARNING: quiet answer") }

# silence / silenced
loud = ActiveSupport::Deprecation.new("2.0", "Loud")
show "silence answers block", loud.silence { :inside }
show "silenced in silence", loud.silence { loud.silenced }
show "nested silence", loud.silence { loud.silence { 1 }; loud.silenced }
show "after silence", loud.silenced
show "warn in silence", stderr_of { loud.silence { loud.warn("hidden") } }
show "silence restores on raise", outcome { loud.silence { raise ArgumentError, "x" } }
show "after raise", loud.silenced
loud.silenced = true
show "silenced= true", [loud.silenced, stderr_of { loud.warn("x") }, loud.warn("x")]
loud.silenced = false
show "silenced= false", site(stderr_of { loud.warn("loud again") })
show "begin/end silence", [loud.begin_silence, loud.silenced, loud.end_silence, loud.silenced]

# behaviors
show ":raise", site(outcome { ActiveSupport::Deprecation.new("1", "R").tap { |d| d.behavior = :raise }.warn("boom") })
raised = begin
  ActiveSupport::Deprecation.new("1", "R").tap { |d| d.behavior = :raise }.warn("trace")
rescue ActiveSupport::DeprecationException => e
  e
end
show ":raise class", [raised.class.name, raised.class.superclass.name, raised.backtrace.is_a?(Array), site(raised.backtrace.first)]
debug = ActiveSupport::Deprecation.new("1", "Debug")
debug.debug = true
show ":stderr debug", site(stderr_of { debug.warn("with trace", caller_locations(0, 1)) })
show ":silence", stderr_of { ActiveSupport::Deprecation.new("1", "S").tap { |d| d.behavior = :silence }.warn("x") }
notified = []
subscriber = ActiveSupport::Notifications.subscribe("deprecation.shopify_money") do |*args|
  payload = args.last
  notified << [payload[:message].sub(/ \(called from .*/, ""), payload[:gem_name], payload[:deprecation_horizon], payload[:callstack].is_a?(Array)]
end
money = ActiveSupport::Deprecation.new("5.0.0", "Shopify/Money")
money.behavior = :notify
money.warn("cents")
ActiveSupport::Notifications.unsubscribe(subscriber)
show ":notify", notified
reported = []
reporter_subscriber = Object.new
reporter_subscriber.define_singleton_method(:report) { |error, **opts| reported << [error.class.name, error.message.sub(/ \(called from .*/, ""), opts[:handled], opts[:severity]] }
ActiveSupport.error_reporter.subscribe(reporter_subscriber)
ActiveSupport::Deprecation.new("1", "Rep").tap { |d| d.behavior = :report }.warn("reported")
ActiveSupport.error_reporter.unsubscribe(reporter_subscriber)
show ":report", reported
calls = []
two = ->(message, callstack) { calls << [:two, message.sub(/ \(called from .*/, ""), callstack.class.name] }
three = ->(message, callstack, deprecator) { calls << [:three, deprecator.gem_name] }
four = ->(message, callstack, horizon, gem_name) { calls << [:four, horizon, gem_name] }
splat = proc { |*args| calls << [:splat, args.size] }
callable = Object.new
callable.define_singleton_method(:call) { |message, callstack, deprecator| calls << [:object, deprecator.deprecation_horizon] }
multi = ActiveSupport::Deprecation.new("3.0", "Multi")
multi.behavior = [two, three, four, splat, callable, :silence]
multi.warn("many")
show "callables", calls
show "behavior size", multi.behavior.size
show "invalid behavior", outcome { multi.behavior = :nope }
show "invalid object", outcome { multi.behavior = 42 }

# disallowed warnings and allow
strict = ActiveSupport::Deprecation.new("1", "Strict")
strict.behavior = :silence
strict.disallowed_warnings = ["forbidden", :symbolic, /regexp \d/]
show "disallowed string", outcome { strict.warn("this is forbidden") }
show "disallowed symbol", outcome { strict.warn("symbolic use") }
show "disallowed regexp", outcome { strict.warn("regexp 7") }
show "allowed message", outcome { strict.warn("fine").to_s.start_with?("DEPRECATION WARNING: fine") }
show "allow all", outcome { strict.allow { strict.warn("forbidden in allow").to_s.start_with?("DEPRECATION") } }
show "allow rule", outcome { strict.allow(["forbidden"]) { strict.warn("forbidden again").to_s.start_with?("DEPRECATION") } }
show "allow other rule", outcome { strict.allow([/nothing/]) { strict.warn("forbidden still") } }
show "allow if false", outcome { strict.allow(:all, if: false) { strict.warn("forbidden no allow") } }
show "allow if proc", outcome { strict.allow(:all, if: -> { true }) { strict.warn("forbidden proc").to_s.start_with?("DEPRECATION") } }
show "after allow", outcome { strict.warn("forbidden after") }
strict.disallowed_warnings = :all
show "disallowed all", outcome { strict.warn("anything") }
strict.disallowed_behavior = :silence
show "disallowed behavior silence", outcome { strict.warn("anything quiet").to_s.start_with?("DEPRECATION") }
show "disallowed nil message", outcome { ActiveSupport::Deprecation.new.tap { |d| d.behavior = :silence; d.disallowed_warnings = ["x"] }.warn.to_s.start_with?("DEPRECATION") }

# A collection of its own.
collection = ActiveSupport::Deprecation::Deprecators.new
first = ActiveSupport::Deprecation.new("1", "First")
collection[:first] = first
collection.behavior = :raise
collection.debug = true
second = ActiveSupport::Deprecation.new("2", "Second")
collection[:second] = second
show "collection [] missing", collection[:none]
show "collection each enum", collection.each.class.name
show "collection each", collection.each.map(&:gem_name)
show "options applied later", [second.debug, outcome { second.warn("late") }.sub(/ \(called from .*/, "")]
show "collection silence", collection.silence { [first.silenced, second.silenced, first.warn("x")] }
show "after collection silence", [first.silenced, second.silenced]
show "collection silence raise", outcome { collection.silence { raise "inner" } }
collection.silenced = true
show "collection silenced", [first.silenced, second.silenced, first.warn("x")]
collection.disallowed_warnings = :all
collection.disallowed_behavior = :silence
show "collection disallowed", [first.disallowed_warnings, first.disallowed_behavior.size]

# Core's call shapes.
user = ActiveSupport::Deprecation.new
show "bare new", [user.gem_name, user.deprecation_horizon]
show "user stderr", site(stderr_of { user.warn("User#retail_role_id is deprecated. Use User#retail_role_ids instead.") })
Rails.application.deprecators[:shopify] = ActiveSupport::Deprecation.new("ASAP", "Shopify/shopify")
show "app deprecator silenced", stderr_of { Rails.application.deprecators[:shopify].warn("app") }
show "app deprecator in each", Rails.application.deprecators.each.map(&:gem_name).last(2)
show "app silence", Rails.application.deprecators.silence { Rails.application.deprecators[:shopify].silenced }
Rails.application.deprecators.debug = true
show "debug through app", Rails.application.deprecators[:shopify].debug
