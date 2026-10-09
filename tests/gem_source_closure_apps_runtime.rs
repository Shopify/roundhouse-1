//! A gem source whose class body instantiates its own nested subclasses
//! (the harness's bin/gem-sources.rb closure, e2e round 6). apps_runtime
//! 0.8.0's direct_database/database/placeholders.rb ends `class
//! Placeholders` with `STATIC = Static.new.freeze` and `INCREMENTING =
//! Incrementing.new.freeze`, after the nested `class Static < Placeholders`
//! and `class Incrementing < Placeholders`. In one Ruby file that runs in
//! source order. The emitted tree splits the nested classes into their own
//! files: each requires placeholders.rb for its superclass, and
//! placeholders.rb required them for its constants, a circular require
//! Ruby warns about, which Core's StrictWarning raises at boot.
//!
//! The emitted class body ran `private_class_method :new` ahead of the
//! deferred `STATIC = Static.new.freeze` the source writes above it, so
//! the subclass's inherited `new` was already private and boot raised
//! NoMethodError. A visibility override now follows the deferred
//! constants the source has above it.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    probe_runtime (0.8.0)\n      activesupport\n\nDEPENDENCIES\n  activesupport\n  probe_runtime\n";

const ENTRY: &str = "require_relative \"probe_runtime/database/placeholders\"\n";

const PLACEHOLDERS: &str = r##"# frozen_string_literal: true

module ProbeRuntime
  class Database
    class Placeholders
      class << self
        def static
          STATIC
        end

        def incrementing
          INCREMENTING
        end
      end

      def render(start_index:, count:)
        raise NotImplementedError, "#{self.class}#render must be implemented"
      end

      class Static < Placeholders
        def render(start_index:, count:)
          return "?" if count == 1
          return "" if count.zero?

          ("?, " * (count - 1)) << "?"
        end
      end

      class Incrementing < Placeholders
        def render(start_index:, count:)
          return "$#{start_index}" if count == 1

          placeholders = String.new
          count.times do |offset|
            placeholders << ", " unless offset.zero?
            placeholders << "$#{start_index + offset}"
          end
          placeholders
        end
      end

      STATIC = Static.new.freeze
      INCREMENTING = Incrementing.new.freeze
      private_constant :Static, :Incrementing, :STATIC, :INCREMENTING
      private_class_method :new
    end
  end
end
"##;

/// Core's components/platform/essentials/lib/strict_warnings.rb, cut
/// down: a warning raises, except one from an installed gem's file
/// (Core skips its BUNDLE_PATH; minitest 6 warns about its own require)
/// or a method redefinition, which Core skips too.
const STRICT: &str = "module StrictWarning\n  def warn(message, category: nil, **kwargs)\n    return super if Gem.path.any? { |dir| message.start_with?(dir) }\n    return super if message.include?(\"method redefined; discarding\") || message.include?(\"previous definition of \")\n    raise message\n  end\nend\nWarning.singleton_class.prepend(StrictWarning)\n$VERBOSE = true\n";

const APPLICATION: &str = "require_relative \"boot\"\n\nrequire \"rails\"\nrequire \"active_record/railtie\"\n\nBundler.require(*Rails.groups)\n\nmodule PlaceholdersProbeApp\n  class Application < Rails::Application\n  end\nend\n";

const CARRIED: &str = "test_paths:\n  - test\ngem_sources:\n  probe_runtime: vendor/gems/probe_runtime-0.8.0\n";

const PLACEHOLDERS_TEST: &str = r##"require "test_helper"

class PlaceholdersTest < ActiveSupport::TestCase
  test "the class body's own nested subclasses answer its constants" do
    placeholders = ProbeRuntime::Database::Placeholders
    assert_equal "?, ?, ?", placeholders.static.render(start_index: 1, count: 3)
    assert_equal "$2, $3", placeholders.incrementing.render(start_index: 2, count: 2)
    assert_raises(NoMethodError) { placeholders.new }
  end
end
"##;

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", CARRIED)
        .write("Gemfile", "source \"https://rubygems.org\"\n\ngem \"activesupport\"\ngem \"probe_runtime\", \"~> 0.8\"\n")
        .write("Gemfile.lock", LOCK)
        .write("vendor/gems/probe_runtime-0.8.0/lib/probe_runtime.rb", ENTRY)
        .write("vendor/gems/probe_runtime-0.8.0/lib/probe_runtime/database/placeholders.rb", PLACEHOLDERS)
        .write("config/boot.rb", "require \"bundler/setup\"\nrequire_relative \"../lib/strict\"\n")
        .write("lib/strict.rb", STRICT)
        .write("config/application.rb", APPLICATION)
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("config/environment.rb", "require_relative \"application\"\n\nRails.application.initialize!\n")
        .write("test/models/placeholders_test.rb", PLACEHOLDERS_TEST)
}

#[test]
fn a_class_body_that_instantiates_its_nested_subclasses_boots_without_a_circular_require() {
    let run = app().run_test("test/models/placeholders_test.rb");
    run.assert_passes();
    // minitest 6 warns about its own require; only the emitted tree's count.
    let ours = run.stderr.lines().filter(|l| l.contains("circular require") && l.contains("probe_runtime")).count();
    assert_eq!(ours, 0, "{}", run.stderr);
}

/// The gem's own layout: database.rb requires database/placeholders.rb,
/// so `Database` and `Placeholders` each keep a file, and the nested
/// subclasses are written apart from the class whose body builds them.
const DATABASE: &str = "require_relative \"database/placeholders\"\n\nmodule ProbeRuntime\n  class Database\n    def self.placeholders\n      Placeholders.static\n    end\n  end\nend\n";

#[test]
fn the_gems_layout_boots_without_a_circular_require() {
    let run = app()
        .write("vendor/gems/probe_runtime-0.8.0/lib/probe_runtime.rb", "require_relative \"probe_runtime/database\"\n")
        .write("vendor/gems/probe_runtime-0.8.0/lib/probe_runtime/database.rb", DATABASE)
        .run_test("test/models/placeholders_test.rb");
    run.assert_passes();
    let ours = run.stderr.lines().filter(|l| l.contains("circular require") && l.contains("probe_runtime")).count();
    assert_eq!(ours, 0, "{}", run.stderr);
}
