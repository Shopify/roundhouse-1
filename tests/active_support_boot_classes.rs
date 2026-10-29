//! What `require "active_support"` defines before Bundler.require, on the
//! PostgreSQL CRuby tree: ActiveSupport::Autoload, ActiveSupport::Deprecation
//! and ActiveSupport::Logger. runtime/gem_facades.rb loads their ports before
//! the native gems, so a gem that extends ActiveSupport::Autoload, builds its
//! own deprecator or subclasses ActiveSupport::Logger as it loads finds them.
//! Before, the deprecation port loaded in boot.rb after the gems, the
//! autoload module had no port, and the logger loaded only where app code
//! reached it: in a large Rails monolith's gem probe, those three accounted
//! for 9 of its 51 Rails-dependent gems.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

/// A gem shaped like several: an autoloaded constant, its own deprecator,
/// and a logger subclass, each at load.
const GEM: &str = r#"require "active_support/dependencies/autoload"
require "active_support/deprecation"

module Gadget
  extend ActiveSupport::Autoload

  autoload :Widget

  DEPRECATOR = ActiveSupport::Deprecation.new("2.0", "Gadget")

  class Log < ActiveSupport::Logger
  end
end
"#;

#[test]
fn a_gem_finds_active_support_s_boot_classes_as_it_loads() {
    let (dir, _) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\ndatabase_adapter: postgresql\n")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_active_support_boot_classes\n")
        .write("lib/oracle.rb", "class Oracle\nend\n")
        .emit(BuildTarget::Ruby);
    let facades = std::fs::read_to_string(dir.join("runtime/gem_facades.rb")).expect("gem_facades.rb");
    let inflector = facades.find("require_relative \"active_support_inflector\"\n").unwrap_or_else(|| panic!("{facades}"));
    for port in ["active_support_dependencies_autoload", "active_support_deprecation", "active_support_logger"] {
        let at = facades.find(&format!("require_relative \"{port}\"\n")).unwrap_or_else(|| panic!("{port}:\n{facades}"));
        assert!(inflector < at, "{port} loads after the Inflector:\n{facades}");
    }
    for feature in ["active_support/dependencies/autoload", "active_support/deprecation", "active_support/deprecator"] {
        assert!(dir.join(format!("runtime/features/{feature}.rb")).exists(), "runtime/features/{feature}.rb");
    }
    let gem = dir.join("tmp-gem/lib");
    std::fs::create_dir_all(gem.join("gadget")).unwrap();
    std::fs::write(gem.join("gadget.rb"), GEM).unwrap();
    std::fs::write(gem.join("gadget/widget.rb"), "module Gadget\n  class Widget\n  end\nend\n").unwrap();
    let out = emit_and_run::ruby()
        .current_dir(&*dir)
        .args([
            "-I", gem.to_str().unwrap(),
            "-e",
            // runtime/gem_facades.rb's order, from the pieces this needs.
            "require_relative 'runtime/active_support_lazy_load_hooks'\n\
             $LOAD_PATH.unshift(File.expand_path('runtime/features'))\n\
             require_relative 'runtime/framework_feature_refusal'\n\
             require_relative 'runtime/module_delegate'\n\
             require_relative 'runtime/active_support_inflector'\n\
             require_relative 'runtime/rails'\n\
             require_relative 'runtime/active_support_dependencies_autoload'\n\
             require_relative 'runtime/active_support_deprecation'\n\
             require_relative 'runtime/active_support_logger'\n\
             require 'gadget'\n\
             p [Gadget.autoload?(:Widget), Gadget::Widget.name, Gadget::DEPRECATOR.gem_name, Gadget::Log.ancestors.include?(::Logger)]\n\
             p $LOADED_FEATURES.grep(%r{/activesupport[-/]}).empty?\n",
        ])
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stdout, "[\"gadget/widget\", \"Gadget::Widget\", \"Gadget\", true]\ntrue\n");
}

/// A gem's ActiveSupport::CurrentAttributes subclass (dux-event-reporter-rails'
/// shape): required by name or reached through Active Support's autoload,
/// its attributes, defaults, `set` with a block, `resets` and `reset`, per
/// execution context. Before, `require "active_support/current_attributes"`
/// was refused by name and the gem did not load.
#[test]
fn a_gem_s_current_attributes_subclass_works_as_rails_does() {
    let (dir, _) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\ndatabase_adapter: postgresql\n")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_active_support_boot_classes\n")
        .write("lib/oracle.rb", "class Oracle\nend\n")
        .emit(BuildTarget::Ruby);
    for feature in ["active_support/current_attributes", "active_support/callbacks", "active_support/execution_context"] {
        assert!(dir.join(format!("runtime/features/{feature}.rb")).exists(), "runtime/features/{feature}.rb");
    }
    let out = emit_and_run::ruby()
        .current_dir(&*dir)
        .args([
            "-e",
            "require_relative 'runtime/active_support_lazy_load_hooks'\n\
             $LOAD_PATH.unshift(File.expand_path('runtime/features'))\n\
             require_relative 'runtime/framework_feature_refusal'\n\
             require_relative 'runtime/module_delegate'\n\
             require_relative 'runtime/active_support_inflector'\n\
             require_relative 'runtime/active_support_core_ext'\n\
             AUTOLOADED = ActiveSupport::CurrentAttributes.name\n\
             require 'active_support/current_attributes'\n\
             RESETS = []\n\
             class Current < ActiveSupport::CurrentAttributes\n\
               attribute :user, :request_id\n\
               attribute :tags, default: -> { [] }\n\
               resets { RESETS << :reset }\n\
               def greeting = \"hi #{user}\"\n\
             end\n\
             Current.user = 'ada'\n\
             Current.tags << :a\n\
             inside = Current.set(user: 'bob') { [Current.user, Current.greeting] }\n\
             p [AUTOLOADED, Current.user, Current.request_id, Current.tags, inside, Current.user]\n\
             ActiveSupport::ExecutionContext.nestable = true\n\
             ActiveSupport::ExecutionContext.push\n\
             p Current.user\n\
             ActiveSupport::ExecutionContext.pop\n\
             p Current.user\n\
             Current.reset\n\
             p [Current.user, Current.tags, RESETS]\n\
             p $LOADED_FEATURES.grep(%r{/activesupport[-/]}).empty?\n",
        ])
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        stdout,
        "[\"ActiveSupport::CurrentAttributes\", \"ada\", nil, [:a], [\"bob\", \"hi bob\"], \"ada\"]\n\
         nil\n\
         \"ada\"\n\
         [nil, [], [:reset]]\n\
         true\n"
    );
}
