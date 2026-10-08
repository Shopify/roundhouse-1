# Native Rails oracle (the Rails native Core runs; tests/support/
# rails_oracle.rb) for the core extensions Rails boot loads before any app
# file: `require "rails"` loads Class#class_attribute and Module#mattr_*,
# and the :load_active_support initializer active_support/all (with
# core_ext/numeric). App code calls them with no require of its own.
#
#   ruby tests/boot_loaded_core_extensions.rb run native APP_DIR FILE...
#   ruby tests/boot_loaded_core_extensions.rb run emitted TREE_DIR
#
# `native` boots Rails as Core's config/application.rb does, runs the
# bootstrap's `require "active_support/all"`, and requires the app FILEs
# (relative to APP_DIR, as Zeitwerk would load them); `emitted` requires
# the tree's boot.rb. Each prints `Quota.lines`.
require_relative "support/rails_oracle"

if ARGV.first == "run"
  _, mode, dir, *rest = ARGV
  env = (mode == "native" ? {} : RailsOracle.core_gems_env).merge("RAILS_ENV" => "test")
  exec(env, RbConfig.ruby, File.expand_path(__FILE__), mode, dir, *rest, chdir: dir)
end

MODE, ROOT, *FILES = ARGV
abort "usage: #{$PROGRAM_NAME} run native APP FILE... | run emitted TREE" unless %w[native emitted].include?(MODE) && ROOT

if MODE == "native"
  RailsOracle.activate("railties", "activerecord", "actionpack", "actionview", "activemodel", "activejob")
  require "rails"
  require "active_record/railtie"
  require "i18n"
  # Rails::Application::Bootstrap's :load_active_support initializer.
  require "active_support/all"
  FILES.each { |file| require File.join(ROOT, file) }
else
  require File.join(ROOT, "boot")
end

puts Quota.lines
