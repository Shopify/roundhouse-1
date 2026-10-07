# Native Rails oracle (the Rails native Core runs; tests/support/
# rails_oracle.rb) for `Rails.backtrace_cleaner` (railties
# Rails::BacktraceCleaner over ActiveSupport::BacktraceCleaner) against
# runtime/spinel/active_support_backtrace_cleaner.rb.
#
#   ruby tests/backtrace_cleaner_runtime.rb compare
#
# makes an app root (a temp dir, by its realpath) with a file under
# components/ that answers real Thread::Backtrace::Locations, then runs
# this script twice from that root: `native` boots a minimal Rails app
# whose config.root is the dir; `emitted` loads the runtime as the
# emitted PostgreSQL tree does (runtime/ruby/rails.rb, whose Rails.root
# is the tree root ".", then the port). Each prints one line per
# observation; `compare` requires them equal.
#
# Frames under the gem directories and the stdlib are built from each
# process's own Gem.path, Gem.default_dir and rubylibdir, which is what
# the cleaner reads; what is compared is the cleaned result.
require "json"
require "fileutils"
require "tmpdir"
require_relative "support/rails_oracle"

FRAMES_RB = <<~RUBY
  module Frames
    def self.locations
      caller_locations(0)
    end

    def self.raised
      raise ArgumentError, "boom"
    rescue ArgumentError => e
      e
    end

    def self.first_frame(cleaner, kind = :silent)
      inner_first_frame(cleaner, kind)
    end

    def self.inner_first_frame(cleaner, kind)
      cleaner.first_clean_frame(kind)
    end

    def self.first_location(cleaner, kind = :silent)
      inner_first_location(cleaner, kind)
    end

    def self.inner_first_location(cleaner, kind)
      cleaner.first_clean_location(kind)
    end
  end
RUBY
FRAMES_PATH = "components/shop/lib/frames.rb"

if ARGV.first == "compare"
  require "open3"
  root = File.realpath(Dir.mktmpdir("backtrace-cleaner-oracle"))
  begin
    FileUtils.mkdir_p(File.join(root, File.dirname(FRAMES_PATH)))
    File.write(File.join(root, FRAMES_PATH), FRAMES_RB)
    outputs = ["native", "emitted"].map do |mode|
      stdout, stderr, status = Open3.capture3(RbConfig.ruby, File.expand_path(__FILE__), mode, root, chdir: root)
      abort "#{mode}: #{stderr}" unless status.success?
      stdout
    end
    native, emitted = outputs.map(&:lines)
    equal = native.zip(emitted).count { |x, y| x == y }
    unless outputs.uniq.length == 1
      diff = native.zip(emitted).each_with_index.reject { |(x, y), _| x == y }.first(8)
      abort "backtrace cleaners differ (#{equal}/#{native.length} observations equal, emitted has #{emitted.length}):\n" +
        diff.map { |(x, y), i| "line #{i + 1}\n  native:  #{x}  emitted: #{y}" }.join("\n")
    end
    puts outputs.first
    puts "observations: #{equal}/#{native.length} equal"
    puts "Native Rails and emitted Rails.backtrace_cleaner agree (#{RailsOracle.label})"
  ensure
    FileUtils.rm_rf(root)
  end
  exit
end

MODE, ROOT = ARGV.first(2)
abort "usage: #{$PROGRAM_NAME} compare | native ROOT | emitted ROOT" unless %w[native emitted].include?(MODE) && ROOT

if MODE == "native"
  RailsOracle.activate("railties")
  require "rails"
  require "logger"
  module CleanerOracle
    class Application < Rails::Application
      config.load_defaults 8.1
      config.root = ROOT
      config.eager_load = false
      config.logger = Logger.new(nil)
    end
  end
  Rails.application.initialize!
else
  require_relative "../runtime/ruby/rails"
  require_relative "../runtime/spinel/active_support_backtrace_cleaner"
end
require File.join(ROOT, FRAMES_PATH)

GEM_HOME = Gem.path.first
DEFAULT_GEMS = Gem.default_dir
RUBYLIBDIR = RbConfig::CONFIG["rubylibdir"]

# An unfiltered frame keeps this process's gem directory, which the two
# modes need not share; it is printed by name.
PLACES = { GEM_HOME => "<GEM_HOME>", DEFAULT_GEMS => "<DEFAULT_GEMS>", RUBYLIBDIR => "<RUBYLIBDIR>" }
  .sort_by { |path, _| -path.length }

def show(label, value)
  text = value.inspect
  PLACES.each { |path, name| text = text.gsub(path, name) }
  puts "#{label}: #{text}"
end

# The error's class and message, without an inspected receiver (a
# FrozenError's message shows the cleaner's procs and their source lines).
def outcome
  yield.inspect
rescue Exception => e
  "#{e.class}: #{e.message.lines.first.to_s.strip.sub(/: #<.*/, "")}"
end

FRAMES = [
  "#{ROOT}/app/models/shop.rb:10:in 'Shop#save'",
  "#{ROOT}/components/platform/essentials/lib/development_support/query_error_log_subscriber.rb:24:in 'QueryErrorLogSubscriber#sql'",
  "#{ROOT}/components/platform/essentials/app/models/x.rb:3:in 'block in <class:X>'",
  "#{ROOT}/components/platform/essentials/lib/patches/active_record/retry.rb:7:in 'Retry#execute'",
  "#{ROOT}/lib/tasks/foo.rb:1:in '<top (required)>'",
  "#{ROOT}/test/models/shop_test.rb:5:in 'ShopTest#test_save'",
  "#{ROOT}/config/initializers/x.rb:2:in '<main>'",
  "#{ROOT}/vendor/plugins/x/lib/x.rb:1:in 'X.go'",
  "#{ROOT}/bin/rails:4:in '<main>'",
  "bin/rails:4:in '<main>'",
  "#{ROOT}/app/views/shops/show.html.erb:3:in '_app_views_shops_show_html_erb__1234_5678'",
  "#{ROOT}/app/views/shops/_row.html.erb:1:in 'block in _app_views_shops__row_html_erb___9876_5432'",
  "#{ROOT}_sibling/app/models/shop.rb:1:in 'Shop#x'",
  "components/shop/lib/relative.rb:1:in 'Relative.go'",
  "app/models/relative.rb:1:in 'Relative.go'",
  "#{GEM_HOME}/gems/activerecord-8.1.4/lib/active_record/base.rb:1:in 'ActiveRecord::Base.x'",
  "#{GEM_HOME}/bundler/gems/rails-5813559f4aed/activesupport/lib/active_support/notifications.rb:2:in 'instrument'",
  "#{GEM_HOME}/gems/pg-1.6.2-x86_64-linux/lib/pg.rb:3:in 'PG.connect'",
  "#{DEFAULT_GEMS}/gems/minitest-5.25.0/lib/minitest.rb:4:in 'Minitest.run'",
  "#{RUBYLIBDIR}/set.rb:5:in 'Set#each'",
  "<internal:kernel>:90:in 'Kernel#tap'",
  "<internal:numeric>:238:in 'Integer#times'",
  "/nix/store/abc-foo-1.0/lib/ruby/gems/4.0.0/gems/foo-1.0/lib/foo.rb:1:in 'Foo.x'",
  "/somewhere/else/lib/x.rb:1:in 'X.y'",
  "-e:1:in '<main>'",
  "pry (0.10.4) lib/pry.rb:1:in 'Pry.start'",
].freeze

cleaner = Rails.backtrace_cleaner
show "class", cleaner.class.name
show "superclasses", [cleaner.class, cleaner.class.superclass, cleaner.class.superclass.superclass].map(&:name)
show "memoized", Rails.backtrace_cleaner.equal?(cleaner)
show "root", Rails::BacktraceCleaner.root == "#{ROOT}/"
show "root frozen", Rails::BacktraceCleaner.root.frozen?

DEFAULT_CLEAN = cleaner.clean(FRAMES)
show "clean", DEFAULT_CLEAN
show "clean :silent", cleaner.clean(FRAMES, :silent)
show "clean :noise", cleaner.clean(FRAMES, :noise)
show "clean :all", cleaner.clean(FRAMES, :all)
show "clean :other", cleaner.clean(FRAMES, :other)
show "filter", cleaner.filter(FRAMES)
show "filter :noise", cleaner.filter(FRAMES, :noise)
show "clean empty", cleaner.clean([])
show "clean result class", cleaner.clean(FRAMES).class.name
FRAMES.each_with_index do |frame, i|
  show "clean_frame #{i}", cleaner.clean_frame(frame)
  show "clean_frame #{i} :noise", cleaner.clean_frame(frame, :noise)
  show "clean_frame #{i} :all", cleaner.clean_frame(frame, :all)
end

# QueryErrorLogSubscriber#backtrace_cleaner (Core, query_error_log_subscriber.rb:36-42).
subscriber = Rails.backtrace_cleaner.dup
show "dup class", subscriber.class.name
show "dup is a copy", !subscriber.equal?(cleaner)
show "add_silencer answers", subscriber.add_silencer { |line| !(line =~ /^components/) }.size
show "subscriber clean", subscriber.clean(FRAMES)
show "subscriber message", subscriber.clean(FRAMES).first(10).join("\n")
show "subscriber clean :noise", subscriber.clean(FRAMES, :noise)
show "original unchanged", cleaner.clean(FRAMES) == DEFAULT_CLEAN

# The dup's lists are its own, both ways.
other = cleaner.dup
cleaner_silencers = cleaner.instance_variable_get(:@silencers).size
cleaner_filters = cleaner.instance_variable_get(:@filters).size
show "default silencers", cleaner_silencers
show "default filters", cleaner_filters
show "remove_silencers! answers", other.remove_silencers!
show "without silencers", other.clean(FRAMES)
show "without silencers :noise", other.clean(FRAMES, :noise)
show "remove_filters! answers", other.remove_filters!
show "without filters or silencers", other.clean(FRAMES) == FRAMES
other.add_silencer { |line| line.include?("shop") }
show "silencer only", other.clean(FRAMES)
show "add_filter answers", other.add_filter { |line| line.sub(%r{\A.*/}, "") }.size
show "filter then silencer", other.clean(FRAMES)
show "original keeps its lists", [cleaner.instance_variable_get(:@silencers).size, cleaner.instance_variable_get(:@filters).size] == [cleaner_silencers, cleaner_filters]
show "original unchanged after", cleaner.clean(FRAMES) == DEFAULT_CLEAN

# Core's development_support/query_debug.rb:84-87, on a copy.
query_debug = cleaner.dup
query_debug.remove_silencers!
query_debug.add_silencer { |line| !(line =~ %r{^(components)/}) }
show "query_debug clean", query_debug.clean(FRAMES)

# Core's config/initializers/backtrace_silencers.rb:7-15, on a copy.
initializer = cleaner.dup
initializer.remove_silencers!
initializer.add_silencer { |line| line.match?(/\A\S+ \(\S+\) \S+/) }
initializer.add_silencer { |line| line.start_with?("/nix/store/") }
initializer.add_silencer { |line| line.start_with?("-e:") }
show "initializer clean", initializer.clean(FRAMES)
show "initializer clean :noise", initializer.clean(FRAMES, :noise)

# The plain ActiveSupport cleaner: gem filter and gem, core and stdlib
# silencers only.
plain = ActiveSupport::BacktraceCleaner.new
show "plain clean", plain.clean(FRAMES)
show "plain clean :noise", plain.clean(FRAMES, :noise)
show "plain clean :all", plain.clean(FRAMES, :all)
plain_copy = plain.clone
plain_copy.add_silencer { |line| true }
show "plain clone independent", [plain.clean(FRAMES).size == plain.clean(FRAMES, :all).size - plain.clean(FRAMES, :noise).size, plain_copy.clean(FRAMES)]

# Thread::Backtrace::Location input.
locations = Frames.locations
show "location classes", locations.map { |l| l.class.name }.uniq
show "clean_locations", cleaner.clean_locations(locations).map(&:to_s)
show "clean_locations classes", cleaner.clean_locations(locations).map { |l| l.class.name }.uniq
show "clean_locations :noise", cleaner.clean_locations(locations, :noise).map(&:to_s)
show "clean_locations :all size", cleaner.clean_locations(locations, :all).size == locations.size
show "clean locations", cleaner.clean(locations)
show "clean locations classes", cleaner.clean(locations).map { |l| l.class.name }.uniq
show "clean_frame location", cleaner.clean_frame(locations.first)
show "subscriber clean locations", subscriber.clean(locations)
show "subscriber clean_locations", subscriber.clean_locations(locations).map(&:to_s)

error = Frames.raised
show "exception backtrace", cleaner.clean(error.backtrace)
show "exception backtrace_locations", cleaner.clean_locations(error.backtrace_locations).map(&:to_s)
show "subscriber exception", subscriber.clean(error.backtrace)

show "first_clean_frame", Frames.first_frame(cleaner)
show "first_clean_frame :noise", Frames.first_frame(cleaner, :noise).nil?
show "first_clean_frame :all", Frames.first_frame(cleaner, :all)
show "first_clean_location", Frames.first_location(cleaner).to_s
show "first_clean_location class", Frames.first_location(cleaner).class.name
show "first_clean_frame subscriber", Frames.first_frame(subscriber)
show "first_clean_frame silenced", Frames.first_frame(subscriber.dup.tap { |c| c.add_silencer { true } })

# ENV["BACKTRACE"] hands the input back, on the Rails cleaner only.
ENV["BACKTRACE"] = "1"
show "BACKTRACE clean is input", cleaner.clean(FRAMES).equal?(FRAMES)
show "BACKTRACE filter is input", cleaner.filter(FRAMES, :noise).equal?(FRAMES)
show "BACKTRACE clean_frame", cleaner.clean_frame(FRAMES.first)
show "BACKTRACE clean_frame location", cleaner.clean_frame(locations.first).class.name
show "BACKTRACE subscriber", subscriber.clean(FRAMES).equal?(FRAMES)
show "BACKTRACE clean_locations", cleaner.clean_locations(locations).size == locations.size
show "BACKTRACE plain", plain.clean(FRAMES) == plain.clean(FRAMES, :silent)
ENV.delete("BACKTRACE")
show "BACKTRACE unset", cleaner.clean(FRAMES) == DEFAULT_CLEAN

# freeze (Rails makes the cleaner shareable with config.ractor_shareable).
frozen = cleaner.dup.freeze
show "frozen", frozen.frozen?
show "frozen clean", frozen.clean(FRAMES) == DEFAULT_CLEAN
show "frozen remove_silencers!", outcome { frozen.remove_silencers! }
show "frozen remove_filters!", outcome { frozen.remove_filters! }
show "frozen dup", outcome { frozen.dup.frozen? }
show "frozen clone", outcome { frozen.clone.frozen? }

show "add_silencer without block", outcome { cleaner.dup.add_silencer.size }
show "nil block silencer", outcome { c = cleaner.dup; c.add_silencer; c.clean(FRAMES) }
show "clean nil", outcome { cleaner.clean(nil) }
show "clean_frame nil", outcome { cleaner.clean_frame(nil) }
show "clean_frame symbol", outcome { cleaner.clean_frame(:"#{ROOT}/app/x.rb:1") }
show "clean strings and locations", cleaner.clean([FRAMES[1], locations.first])
