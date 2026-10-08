# Native oracle for config/boot.rb's requires of gem-home features on the
# emitted PostgreSQL tree (App::boot_native_requires, emitted first in
# runtime/gem_facades.rb). Core's config/boot.rb:85 `require
# "shopify/types/setup"` sets sorbet-runtime's default checked level
# before `require "rails"` and Bundler.require; a gem that evaluates a
# sig as it loads must find it set, and a later `require
# "shopify/types/setup"` (Core's gems/ci/lib/ci.rb:5,
# components/platform/essentials/lib/ci_environment.rb:4) is a no-op.
#
#   ruby tests/boot_native_requires.rb run native APP NATIVE_LIB GEM_LIB...
#   ruby tests/boot_native_requires.rb run emitted TREE NATIVE_LIB
#
# `native` runs the app's config/boot.rb require, then requires each
# GEM_LIB's gem in Gemfile order (Bundler.require); `emitted` boots the
# tree. Each prints what it observes, which must be equal.
require_relative "support/rails_oracle"

if ARGV.first == "run"
  _, mode, dir, *rest = ARGV
  exec(RailsOracle.core_gems_env.merge("RAILS_ENV" => "test"), RbConfig.ruby, File.expand_path(__FILE__), mode, dir, *rest, chdir: dir)
end

MODE, ROOT, NATIVE_LIB, *GEM_LIBS = ARGV
abort "usage: #{$PROGRAM_NAME} native APP NATIVE_LIB GEM_LIB... | emitted TREE NATIVE_LIB" unless %w[native emitted].include?(MODE) && NATIVE_LIB

$LOAD_PATH.unshift(NATIVE_LIB)
if MODE == "native"
  require "shopify/types/setup" # the app's config/boot.rb
  GEM_LIBS.each do |lib|
    $LOAD_PATH.unshift(lib)
    require File.basename(Dir[File.join(lib, "*.rb")].first, ".rb")
  end
else
  require File.join(ROOT, "boot")
end

puts "level: #{T::Private::RuntimeLevels.default_checked_level.inspect}"
puts "doubled: #{SiggedNative::DOUBLED}"
puts "cikit: #{Cikit.loaded.inspect}"
begin
  SiggedNative.double("x")
  puts "checked: no"
rescue TypeError => e
  puts "checked: #{e.class}"
end
