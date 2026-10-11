# Native Rails oracle (the Rails native Core runs; tests/support/
# rails_oracle.rb) for Core's components/platform/essentials/config/
# initializers/backtrace_silencers.rb (copied verbatim to tests/support/
# core_backtrace_silencers.rb) and the cleaners it leaves behind:
# `Rails.backtrace_cleaner`, `ActiveRecord::LogSubscriber.backtrace_cleaner`
# (its `on_load(:active_record)` block), and QueryErrorLogSubscriber's
# `Rails.backtrace_cleaner.dup` + `add_silencer` (Core, development_support/
# query_error_log_subscriber.rb:36-42).
#
#   ruby tests/backtrace_silencers_runtime.rb compare APP_ROOT TREE_ROOT
#
# APP_ROOT is a probe app whose config/initializers holds that file;
# TREE_ROOT is its emitted PostgreSQL Ruby tree (tests/backtrace_cleaner_runtime.rs
# packages it). `native` boots APP_ROOT as a Rails app (railties and
# activerecord, RAILS_ENV=test) and loads ActiveRecord::Base, which fires
# `:active_record`; `emitted` runs TREE_ROOT's boot.rb, whose initializer
# programs run the file (ingest::initializer_programs). Each runs once
# without and once with ENV["BACKTRACE"] (the file's line 8) and prints one
# line per observation, over the same backtrace written under its own
# Rails root, gem home and stdlib; `compare` requires the lines equal and
# the emitted boot to name no statement it could not run.
require "fileutils"
require_relative "support/rails_oracle"

if ARGV.first == "compare"
  require "open3"
  app_root, tree_root = ARGV[1, 2].map { |dir| File.realpath(dir) }
  outputs = [nil, "1"].flat_map do |backtrace|
    [["native", app_root, {}], ["emitted", tree_root, RailsOracle.core_gems_env]].map do |mode, root, env|
      env = env.merge("BACKTRACE" => backtrace, "RAILS_ENV" => "test", "DATABASE_URL" => nil, "ROUNDHOUSE_DATABASE_URL" => nil)
      stdout, stderr, status = Open3.capture3(env, RbConfig.ruby, File.expand_path(__FILE__), mode, root, chdir: root)
      abort "#{mode} (BACKTRACE=#{backtrace.inspect}) failed:\n#{stdout}\n#{stderr}" unless status.success?
      not_run = stderr.lines.grep(/initializer_statement_not_run|initializer_unsupported/)
      abort "#{mode} did not run every statement:\n#{not_run.join}" unless not_run.empty?
      [mode, backtrace, stdout]
    end
  end
  outputs.each_slice(2) do |(_, backtrace, native), (_, _, emitted)|
    next if native == emitted
    pairs = native.lines.zip(emitted.lines).each_with_index.reject { |(x, y), _| x == y }
    abort "BACKTRACE=#{backtrace.inspect}: #{pairs.size} of #{native.lines.size} observations differ " \
      "(emitted has #{emitted.lines.size}):\n" +
      pairs.first(8).map { |(x, y), i| "line #{i + 1}\n  native:  #{x}  emitted: #{y}" }.join("\n")
  end
  outputs.each_slice(2) { |(_, backtrace, native), _| puts "== BACKTRACE=#{backtrace.inspect}", native }
  puts "observations: #{outputs.each_slice(2).sum { |(_, _, native), _| native.lines.size }} equal"
  puts "Native Rails and the emitted tree agree on Core's backtrace_silencers.rb (#{RailsOracle.label})"
  exit
end

MODE, ROOT = ARGV.first(2)
abort "usage: #{$PROGRAM_NAME} compare APP_ROOT TREE_ROOT | native ROOT | emitted ROOT" unless %w[native emitted].include?(MODE) && ROOT

if MODE == "native"
  RailsOracle.activate("railties", "activerecord")
  require "rails"
  require "active_record/railtie"
  require "logger"
  module SilencersOracle
    class Application < Rails::Application
      config.load_defaults 8.1
      config.root = ROOT
      config.eager_load = false
      config.logger = Logger.new(nil)
    end
  end
  Rails.application.initialize!
  # Loading the base fires `:active_record` (activerecord base.rb), which
  # runs the railtie's hook and then the initializer's.
  ActiveRecord::Base
else
  require "./boot"
end

GEM_HOME = Gem.path.first
DEFAULT_GEMS = Gem.default_dir
RUBYLIBDIR = RbConfig::CONFIG["rubylibdir"]
APP = Rails::BacktraceCleaner.root.chomp("/")

# A frame the cleaners keep unfiltered shows this process's root or gem
# directory, which the two modes do not share; it is printed by name.
PLACES = { APP => "<ROOT>", GEM_HOME => "<GEM_HOME>", DEFAULT_GEMS => "<DEFAULT_GEMS>", RUBYLIBDIR => "<RUBYLIBDIR>" }
  .sort_by { |path, _| -path.length }

def show(label, value)
  text = value.inspect
  PLACES.each { |path, name| text = text.gsub(path, name) }
  puts "#{label}: #{text}"
end

def outcome
  yield.inspect
rescue Exception => e
  "#{e.class}: #{e.message.lines.first.to_s.strip.sub(/: #<.*/, "")}"
end

ESSENTIALS = "#{APP}/components/platform/essentials"
# A query's backtrace as Core logs it: application frames under
# components/, Core's Active Record patches, Rails and other gems from the
# gem home and from per-gem /nix/store paths, the stdlib, and each frame a
# silencer in the file names.
FRAMES = [
  "#{ESSENTIALS}/lib/development_support/query_error_log_subscriber.rb:24:in 'QueryErrorLogSubscriber#sql'",
  "#{GEM_HOME}/gems/activesupport-8.1.4/lib/active_support/log_subscriber.rb:120:in 'ActiveSupport::LogSubscriber#call'",
  "/nix/store/0a1b2c3d-activesupport-8.1.4/lib/ruby/gems/4.0.0/gems/activesupport-8.1.4/lib/active_support/notifications/fanout.rb:110:in 'block in finish'",
  "#{GEM_HOME}/bundler/gems/rails-5eb8c4c45bee/activerecord/lib/active_record/connection_adapters/abstract_adapter.rb:1150:in 'ActiveRecord::ConnectionAdapters::AbstractAdapter#log'",
  "/nix/store/4e5f6a7b-pg-1.6.2/lib/ruby/gems/4.0.0/gems/pg-1.6.2/lib/pg/connection.rb:200:in 'PG::Connection#exec_params'",
  "#{ESSENTIALS}/lib/patches/active_record/retry.rb:31:in 'Patches::ActiveRecord::Retry#execute'",
  "#{ESSENTIALS}/lib/patches/active_record/deadline.rb:18:in 'Patches::ActiveRecord::Deadline#raw_execute'",
  "#{ESSENTIALS}/lib/patches/active_record/mysql_monitoring.rb:9:in 'block in exec_query'",
  "#{ESSENTIALS}/lib/patches/active_record/strict_loading.rb:12:in 'Patches::ActiveRecord::StrictLoading#load_target'",
  "#{ESSENTIALS}/lib/semian/active_record_adapter.rb:44:in 'Semian::ActiveRecordAdapter#query'",
  "#{ESSENTIALS}/lib/instrumentation/sql.rb:7:in 'Instrumentation::Sql.measure'",
  "#{ESSENTIALS}/lib/middleware/request_tagging.rb:15:in 'Middleware::RequestTagging#call'",
  "#{ESSENTIALS}/lib/podding/router.rb:3:in 'Podding::Router#route'",
  "#{ESSENTIALS}/app/utils/mutation_guard.rb:22:in 'MutationGuard.guard'",
  "#{ESSENTIALS}/lib/patches/job_callback_order.rb:5:in 'block in run_callbacks'",
  "#{APP}/components/platform/pagination/app/models/pagination/paginator.rb:40:in 'Pagination::Paginator#page'",
  "#{APP}/components/platform/graph_model/app/graphql/graph_api/field.rb:88:in 'GraphApi::Field#resolve'",
  "#{APP}/components/platform/graph_model/app/graphql/graph_api/active_record_loader.rb:12:in 'GraphApi::ActiveRecordLoader#perform'",
  "#{APP}/components/platform/graph_model/app/models/graph_api/query.rb:30:in 'GraphApi::Query#execute'",
  "#{APP}/components/shop_identity/app/models/concerns/accelerated_belongs_to.rb:8:in 'AcceleratedBelongsTo#reader'",
  "#{APP}/components/shop_identity/app/models/shop.rb:210:in 'Shop#domain'",
  "#{APP}/components/shop_identity/app/controllers/shops_controller.rb:12:in 'ShopsController#show'",
  "#{APP}/app/models/order.rb:3:in 'Order#total'",
  "#{APP}/config/initializers/filter_query_logging.rb:4:in 'block in <main>'",
  "#{APP}/test/support/helpers/pitchfork_test_helper.rb:61:in 'block in PitchforkTestHelper#run'",
  "#{APP}/test/models/shop_test.rb:9:in 'ShopTest#test_domain'",
  "#{APP}/bin/rails:4:in '<main>'",
  "#{DEFAULT_GEMS}/gems/minitest-5.25.0/lib/minitest.rb:4:in 'Minitest.run'",
  "#{RUBYLIBDIR}/monitor.rb:201:in 'Monitor#synchronize'",
  "<internal:kernel>:90:in 'Kernel#tap'",
  "-e:1:in '<main>'",
  "pry (0.10.4) lib/pry.rb:1:in 'Pry.start'",
  "/somewhere/else/lib/x.rb:1:in 'X.y'",
].freeze

def counts(cleaner)
  [cleaner.instance_variable_get(:@silencers).size, cleaner.instance_variable_get(:@filters).size]
end

rails = Rails.backtrace_cleaner
show "BACKTRACE", ENV["BACKTRACE"]
show "Rails cleaner class", rails.class.name
show "Rails cleaner silencers, filters", counts(rails)
show "Rails clean", rails.clean(FRAMES)
show "Rails clean :noise", rails.clean(FRAMES, :noise)
show "Rails clean :all", rails.clean(FRAMES, :all)
FRAMES.each_with_index do |frame, i|
  show "Rails clean_frame #{i}", rails.clean_frame(frame)
end

# The `on_load(:active_record)` block: a dup, plus the skipped_lines silencer.
log = ActiveRecord::LogSubscriber.backtrace_cleaner
show "LogSubscriber cleaner class", log.class.name
show "LogSubscriber cleaner is a copy", !log.equal?(rails)
show "LogSubscriber cleaner?", ActiveRecord::LogSubscriber.backtrace_cleaner?
show "LogSubscriber cleaner silencers, filters", counts(log)
show "LogSubscriber clean", log.clean(FRAMES)
show "LogSubscriber clean :noise", log.clean(FRAMES, :noise)
FRAMES.each_with_index do |frame, i|
  show "LogSubscriber clean_frame #{i}", log.clean_frame(frame)
end
show "LogSubscriber subclass reads it", outcome { Class.new(ActiveRecord::LogSubscriber).backtrace_cleaner.equal?(log) }

# QueryErrorLogSubscriber#backtrace_cleaner and the message's trace (line 24).
subscriber = Rails.backtrace_cleaner.dup
subscriber.add_silencer { |line| !(line =~ /^components/) }
show "subscriber silencers, filters", counts(subscriber)
show "subscriber clean", subscriber.clean(FRAMES)
show "subscriber message", subscriber.clean(FRAMES).first(10).join("\n")
show "Rails cleaner unchanged", counts(rails)
show "LogSubscriber cleaner unchanged", counts(log)
