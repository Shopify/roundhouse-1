//! A class whose parent is a natively loaded gem's class includes that
//! gem's nested modules and calls the base's class macro.
//!
//! Core's `PanamaMetadata::Schema < Hashie::Dash` (panama_metadata, a gem
//! source carried as app code) does `include Hashie::Extensions::Dash::
//! PropertyTranslation`, `include Hashie::Extensions::IgnoreUndeclared`,
//! then `property :id, from: "schema_id"`. hashie is only a transitive
//! dependency, so analysis knows none of its modules and records each
//! include as unresolved: a check-time ledger entry in the class's
//! `unknown_calls`. The body replays verbatim (the base is the gem's), and
//! the replay emitted those records as `raise "roundhouse: include not
//! supported"` after the includes had already run, so boot died in the
//! class body. Each include must run once, where it is written.
//!
//! The fixture is a small gem `dashkit` installed into a scratch gem home
//! with hashie's shape (a `Hash` subclass with a `property` macro, an
//! extension whose `included` hook extends the includer and wraps
//! `property`, and one that filters `initialize`). Nothing tells analysis
//! about it: no RBI, no `gem_home:`; the process loads it (`ruby -rdashkit`),
//! as Core's boot loads hashie from the gem home. The emitted tree must
//! print what CRuby prints.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::path::{Path, PathBuf};
use std::process::Command;

use roundhouse::project::BuildTarget;

const GEM_LIB: &str = r##"module Dashkit
  class Dash < Hash
    def self.properties
      @properties ||= superclass.respond_to?(:properties) ? superclass.properties.dup : {}
    end

    def self.property(name, **options)
      properties[name] = options
      define_method(name) { self[name] }
      name
    end

    def initialize(attributes = {})
      super()
      attributes.each do |key, value|
        raise NoMethodError, "The property '#{key}' is not defined for #{self.class.name}." unless self.class.properties.key?(key.to_sym)
        self[key.to_sym] = value
      end
    end
  end

  module Extensions
    module Dash
      module PropertyTranslation
        def self.included(base)
          base.extend(ClassMethods)
        end

        module ClassMethods
          def translations
            @translations ||= {}
          end

          def property(name, **options)
            from = options.delete(:from)
            translations[from.to_s] = name if from
            super(name, **options)
          end
        end

        def initialize(attributes = {})
          super(attributes.to_h { |key, value| [self.class.translations.fetch(key.to_s, key), value] })
        end
      end
    end

    module IgnoreUndeclared
      def initialize(attributes = {})
        super(attributes.select { |key, _| self.class.properties.key?(key.to_sym) || self.class.translations.key?(key.to_s) })
      end
    end
  end
end
"##;

const SCHEMA: &str = r#"module Panama
  class Schema < Dashkit::Dash
    include Dashkit::Extensions::Dash::PropertyTranslation
    include Dashkit::Extensions::IgnoreUndeclared
    property :id, from: "schema_id"
    property :description, from: "schema_description"
  end
end
"#;

const SCRIPT: &str = r##"
s = Panama::Schema.new("schema_id" => 7, "schema_description" => "orders", "unknown" => 1)
puts [s.id, s.description, s.keys].inspect
puts Panama::Schema.properties.keys.inspect
puts Panama::Schema.translations.inspect
puts Panama::Schema.ancestors.take(4).inspect
"##;

fn gem_home(root: &Path) -> PathBuf {
    let home = root.join("gem-home");
    let gem = home.join("gems/dashkit-1.0.0");
    std::fs::create_dir_all(gem.join("lib")).unwrap();
    std::fs::create_dir_all(home.join("specifications")).unwrap();
    std::fs::write(gem.join("lib/dashkit.rb"), GEM_LIB).unwrap();
    std::fs::write(
        home.join("specifications/dashkit-1.0.0.gemspec"),
        "Gem::Specification.new do |s|\n  s.name = \"dashkit\"\n  s.version = \"1.0.0\"\n  s.summary = \"fixture\"\n  s.authors = [\"x\"]\n  s.files = [\"lib/dashkit.rb\"]\n  s.require_paths = [\"lib\"]\nend\n",
    )
    .unwrap();
    home
}

/// The fixture gem home ahead of the interpreter's own, so the emitted
/// tree's other gems (sqlite3, pg) still load.
fn gem_path(home: &Path) -> String {
    let out = emit_and_run::ruby().args(["-e", "print Gem.path.join(File::PATH_SEPARATOR)"]).output().expect("ruby");
    format!("{}:{}", home.display(), String::from_utf8_lossy(&out.stdout))
}

fn lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes).lines().map(str::to_string).collect()
}

#[test]
fn a_gem_parent_s_nested_module_includes_run_once_where_written() {
    let root = std::env::temp_dir().join(format!("roundhouse-gem-parent-include-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let gem_path = gem_path(&gem_home(&root));
    let native = emit_and_run::ruby()
        .env("GEM_PATH", &gem_path)
        .args(["-rdashkit", "-e"])
        .arg(format!("{SCHEMA}\n{SCRIPT}"))
        .output()
        .expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = lines(&native.stdout);
    assert_eq!(
        expected,
        [
            "[7, \"orders\", [:id, :description]]",
            "[:id, :description]",
            "{\"schema_id\" => :id, \"schema_description\" => :description}",
            "[Panama::Schema, Dashkit::Extensions::IgnoreUndeclared, Dashkit::Extensions::Dash::PropertyTranslation, Dashkit::Dash]",
        ],
        "native control"
    );

    let (tree, errors) = emit_and_run::empty_app().write("app/models/panama/schema.rb", SCHEMA).emit(BuildTarget::Ruby);
    // Analysis cannot resolve the gem's modules and says so, at check.
    assert!(errors.iter().any(|e| e.contains("Panama::Schema includes unresolved Dashkit::Extensions::Dash::PropertyTranslation")), "{errors:#?}");
    let emitted = std::fs::read_to_string(tree.join("app/models/panama/schema.rb")).expect("schema.rb");
    assert_eq!(emitted.matches("include Dashkit::Extensions::Dash::PropertyTranslation").count(), 1, "{emitted}");
    assert_eq!(emitted.matches("include Dashkit::Extensions::IgnoreUndeclared").count(), 1, "{emitted}");
    assert!(!emitted.contains("not supported"), "{emitted}");
    let run = Command::new("ruby")
        .current_dir(&tree)
        .env("GEM_PATH", &gem_path)
        .env("BLOG_DB", ":memory:")
        .args(["-rdashkit", "-e"])
        .arg(format!("require File.expand_path(\"main\", Dir.pwd)\nMain.configure_default_adapter!\n{SCRIPT}"))
        .output()
        .expect("spawn ruby");
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "emitted tree failed in {}: {stderr}\n{emitted}\nerrors: {errors:#?}", tree.display());
    assert_eq!(lines(&run.stdout), expected, "emitted Ruby diverges from CRuby\n{emitted}\nerrors: {errors:#?}");
    let _ = std::fs::remove_dir_all(&root);
}

/// As Core carries it: the class is a gem source's (`gem_sources:`), whose
/// entry requires the base gem from the gem home, a lock dependency.
#[test]
fn a_gem_source_s_class_on_a_required_gem_s_base_runs_as_in_cruby() {
    const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    dashkit (1.0.0)\n    panama (1.0.0)\n      dashkit (= 1.0.0)\n\nDEPENDENCIES\n  panama\n";
    let root = std::env::temp_dir().join(format!("roundhouse-gem-parent-include-source-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let home = gem_home(&root);
    let gem_path = gem_path(&home);
    let (tree, errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", &format!("gem_home: {}\ngem_sources:\n  panama: vendor/gems/panama-1.0.0\n", home.display()))
        .write("Gemfile.lock", LOCK)
        .write("vendor/gems/panama-1.0.0/lib/panama.rb", "require \"dashkit\"\nrequire \"panama/schema\"\n")
        .write("vendor/gems/panama-1.0.0/lib/panama/schema.rb", SCHEMA)
        .emit(BuildTarget::Ruby);
    let emitted = std::fs::read_to_string(tree.join("app/models/panama/schema.rb")).expect("schema.rb");
    assert!(!emitted.contains("not supported"), "{emitted}");
    let run = Command::new("ruby")
        .current_dir(&tree)
        .env("GEM_PATH", &gem_path)
        .env("BLOG_DB", ":memory:")
        .arg("-e")
        .arg(format!("require File.expand_path(\"main\", Dir.pwd)\nMain.configure_default_adapter!\n{SCRIPT}"))
        .output()
        .expect("spawn ruby");
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "emitted tree failed in {}: {stderr}\n{emitted}\nerrors: {errors:#?}", tree.display());
    assert_eq!(
        lines(&run.stdout),
        [
            "[7, \"orders\", [:id, :description]]",
            "[:id, :description]",
            "{\"schema_id\" => :id, \"schema_description\" => :description}",
            "[Panama::Schema, Dashkit::Extensions::IgnoreUndeclared, Dashkit::Extensions::Dash::PropertyTranslation, Dashkit::Dash]",
        ],
        "emitted Ruby diverges from CRuby\n{emitted}\nerrors: {errors:#?}"
    );
    assert!(!errors.iter().any(|e| e.contains("includes unresolved")), "{errors:#?}");
    let _ = std::fs::remove_dir_all(&root);
}
