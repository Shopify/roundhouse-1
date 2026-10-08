# Native Rails oracle (tests/support/rails_oracle.rb) for an autoloaded
# file whose constant a gem already defined when the autoloaders were set
# up: Zeitwerk ignores the file as shadowed (Core's in-repo proto-shopify
# engine's app/gen/proto/shopify/common/country_code.rb, after the native
# proto gem's copy that shopify_spanner requires at boot).
#
#   ruby tests/shadowed_autoload_file.rb run native APP_DIR GEM_FILE
#   ruby tests/shadowed_autoload_file.rb run emitted TREE_DIR GEM_FILE
#
# SHADOW_EAGER=1 sets the native app's `config.eager_load`, as the
# emitted app's config/environments/test.rb sets it.
#
# Both require GEM_FILE first, as Bundler.require does before
# Application#initialize!. `native` then boots a Rails application rooted
# at APP_DIR (its Zeitwerk main autoloader on app/models, then
# config/initializers); `emitted` requires the tree's main.rb. Each prints
# what the constants hold and how many constants were initialized twice.
require_relative "support/rails_oracle"

if ARGV.first == "run"
  _, mode, dir, *rest = ARGV
  env = (mode == "native" ? {} : RailsOracle.core_gems_env).merge("RAILS_ENV" => "test")
  exec(env, RbConfig.ruby, File.expand_path(__FILE__), mode, dir, *rest, chdir: dir)
end

MODE, ROOT, GEM = ARGV
abort "usage: #{$PROGRAM_NAME} run native APP GEM_FILE | run emitted TREE GEM_FILE" unless %w[native emitted].include?(MODE) && ROOT && GEM

# The probe's own constants initialized twice (the oracle's environment
# may warn about its own).
REDEFINED = []
module CountRedefinitions
  def warn(message, *rest, **options)
    REDEFINED << message if message.match?(/already initialized constant (Codes|Listing|Widget)\b/)
    super
  end
end
Warning.extend(CountRedefinitions)

require GEM

if MODE == "native"
  RailsOracle.activate("railties")
  require "rails"

  class ShadowProbe < Rails::Application
    config.root = ROOT
    config.eager_load = ENV["SHADOW_EAGER"] == "1"
    config.logger = Logger.new(nil)
    config.secret_key_base = "shadowed-autoload-file"
  end
  ShadowProbe.initialize!
else
  require File.join(ROOT, "main")
end

puts "country: #{Codes::Country.origin} #{Codes::Country.constants.sort.inspect}"
puts "list: #{Widget.list}"
puts "widget: #{Widget.kind}"
puts "redefined: #{REDEFINED.size}"
