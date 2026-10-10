# Native Rails oracle (the Rails native Core runs; tests/support/
# rails_oracle.rb) for a native gem's Rails feature requires on the
# emitted PostgreSQL tree (project.rs::FRAMEWORK_FEATURE_PORTS): Core's
# job-iteration requires "active_job", responders "action_controller",
# identifications "active_support/multibyte/unicode", and so on, as they
# load from the gem home. Natively Rails loaded each before any gem; on
# the tree each answers from the runtime ports (runtime/features).
#
#   ruby tests/framework_feature_requires.rb native APP_DIR GEM_LIB
#   ruby tests/framework_feature_requires.rb emitted TREE_DIR GEM_LIB
#   ruby tests/framework_feature_requires.rb refusals TREE_DIR
#
# (each through `run MODE DIR ...`, which sets the mode's environment).
#
# `native` boots Rails with Core's config/application.rb framework
# requires, `emitted` the tree's boot.rb; each then requires GEM_LIB's
# probe_gem.rb (not compiled: a gem from the gem home) and prints what it
# observes, which must be equal. `refusals` prints what the tree answers
# for each feature it does not port (project.rs::framework_feature_refusal),
# and for active_support/all and core_ext/hash, which it ports in part.
require_relative "support/rails_oracle"
require "fileutils"
require "stringio"
require "tmpdir"

# Each mode in its own Ruby: the emitted tree with the pinned gem home
# (i18n), and jemalloc, which that gem home's native extensions need.
if ARGV.first == "run"
  _, mode, dir, *rest = ARGV
  env = (mode == "native" ? {} : RailsOracle.core_gems_env).merge("RAILS_ENV" => "test")
  exec(env, RbConfig.ruby, File.expand_path(__FILE__), mode, dir, *rest, chdir: dir)
end

MODE, ROOT, GEM_LIB = ARGV
abort "usage: #{$PROGRAM_NAME} native APP GEM_LIB | emitted TREE GEM_LIB | refusals TREE" unless %w[native emitted refusals].include?(MODE) && ROOT

if MODE == "native"
  RailsOracle.activate("railties", "activerecord", "actionpack", "actionview", "activemodel", "activejob")
  # actionmailer is in the checkout, not registered as a gem.
  $LOAD_PATH.unshift(File.join(RailsOracle.source, "actionmailer/lib"))
  # Core's config/application.rb, before Bundler.require.
  require "rails"
  require "active_record/railtie"
  require "action_controller/railtie"
  require "action_view/railtie"
  require "action_mailer/railtie"
  require "active_job/railtie"
  require "i18n"
else
  require File.join(ROOT, "boot")
end

if MODE == "refusals"
  # A framework-named feature whose file exists and requires a missing one.
  inner = Dir.mktmpdir
  FileUtils.mkdir_p(File.join(inner, "active_support"))
  File.write(File.join(inner, "active_support/probe_inner.rb"), "require \"probe_gem_inner_dependency\"\n")
  $LOAD_PATH.push(inner)
  %w[
    active_support/all active_support/core_ext/hash
    active_support/core_ext/string/filters active_support/core_ext action_dispatch/testing/integration rails/no_such_feature.rb
    active_model_serializers_probe probe_missing/active_support active_support/probe_inner
  ].each do |feature|
    require feature
    puts "#{feature}: loaded"
  rescue LoadError => e
    puts "#{feature}: #{e.class}: #{e.message}"
  end
  # An engine's route set is recorded, never drawn: drawing it or
  # answering a route from it is refused by name.
  module Refused
    class Engine < Rails::Engine
      isolate_namespace(Refused)
      routes { raise "an engine's route block ran at boot" }
    end
  end
  [
    -> { Refused::Engine.routes.draw {} },
    -> { Refused::Engine.routes.url_helpers },
    -> { Refused::Engine.routes.recognize_path("/") },
    -> { Refused.railtie_routes_url_helpers },
  ].each do |call|
    call.call
    puts "routes: answered"
  rescue NotImplementedError => e
    puts "routes: #{e.class}: #{e.message}"
  end
  exit
end

$LOAD_PATH.unshift(GEM_LIB)
require "probe_gem"
ProbeGem.report.each { |key, value| puts "#{key}: #{value.inspect}" }
# The tree's answers are its ports: no released Rails gem from the gem
# path (RubyGems would activate one for a feature the tree lacks).
if MODE == "emitted"
  released = $LOADED_FEATURES.grep(%r{/gems/(?:#{%w[activesupport activemodel activerecord activejob actionpack actionview actionmailer railties].join("|")})-\d})
  abort "the gem home's Rails loaded on the tree:\n#{released.join("\n")}" if released.any?
end
