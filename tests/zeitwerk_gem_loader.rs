//! An in-repo gem's own Zeitwerk loader, compiled into the generated
//! autoload instead of run.
//!
//! Core's `gems/functions/lib/functions.rb` (required at boot from the
//! essentials initializers) runs `Zeitwerk::Loader.for_gem`, ignores
//! `File.join(__dir__, "tapioca")` and calls `setup`. The emitted tree
//! does not carry Zeitwerk: the gem's files are registered by the
//! generated `app/models.rb` like the app's, minus what the loader
//! ignores, and the constant holds a handle whose `eager_load` loads
//! them. Native Ruby with the real Zeitwerk on the same source is the
//! oracle.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::path::Path;

const ENTRY: &str = r#"require "zeitwerk"

module Fn
  ZeitwerkLoader = Zeitwerk::Loader.for_gem #: Zeitwerk::Loader
  private_constant(:ZeitwerkLoader)
  ZeitwerkLoader.ignore(File.join(__dir__, "tapioca"))
  ZeitwerkLoader.setup

  TOPIC = "fn/1.0"

  class Error < StandardError
  end

  def self.loader
    ZeitwerkLoader
  end
end
"#;

const WIDGET: &str = r#"module Fn
  class Widget
    def self.hi
      "hi #{Fn::TOPIC}"
    end
  end
end
"#;

const GADGET: &str = r#"module Fn
  module Parts
    class Gadget
      LOADED = true
    end
  end
end
"#;

/// Natively a NameError if Zeitwerk managed it: the superclass is
/// tapioca's, which the app does not carry.
const IGNORED: &str = r#"module Tapioca
  class FnCompiler < Tapioca::Dsl::Compiler
  end
end
"#;

const REPORT: &str = r#"out = []
out << Fn::Widget.hi
out << defined?(Tapioca).inspect
out << Fn::Error.superclass
out << Fn::Parts.const_defined?(:Gadget, false)
out << Fn::Parts.autoload?(:Gadget).nil?
out << Fn.loader.eager_load.inspect
out << Fn::Parts::Gadget::LOADED
out << defined?(Tapioca).inspect
puts out.join("|")
"#;

#[test]
fn a_for_gem_loader_autoloads_its_files_and_leaves_ignored_paths_alone() {
    let files = [
        ("gems/fn/lib/fn.rb", ENTRY),
        ("gems/fn/lib/fn/widget.rb", WIDGET),
        ("gems/fn/lib/fn/parts/gadget.rb", GADGET),
        ("gems/fn/lib/tapioca/fn_compiler.rb", IGNORED),
    ];

    // Native: the gem's lib on the load path, the real Zeitwerk.
    let native_root = std::env::temp_dir().join(format!("roundhouse-zeitwerk-native-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&native_root);
    for (path, text) in files {
        let file = native_root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let native = emit_and_run::ruby()
        .arg("-I")
        .arg(native_root.join("gems/fn/lib"))
        .arg("-e")
        .arg(format!("require \"fn\"\n{REPORT}"))
        .output()
        .expect("spawn ruby");
    let _ = std::fs::remove_dir_all(&native_root);
    assert!(native.status.success(), "native oracle failed:\n{}", String::from_utf8_lossy(&native.stderr));
    let expected = String::from_utf8_lossy(&native.stdout).into_owned();
    assert_eq!(expected.trim(), "hi fn/1.0|nil|StandardError|true|false|nil|true|nil", "native oracle");

    let mut overlay = emit_and_run::real_blog().edit(
        "Gemfile.lock",
        "GEM\n  remote: https://rubygems.org/\n",
        "PATH\n  remote: gems/fn\n  specs:\n    fn (0.1.0)\n\nGEM\n  remote: https://rubygems.org/\n",
    );
    for (path, text) in files {
        overlay = overlay.write(path, text);
    }
    let run = overlay.run_ruby(REPORT);
    assert!(run.errors.is_empty(), "{}", run.errors.join("\n"));
    assert!(run.success, "emitted program failed in {}:\n{}", run.emitted.display(), run.stderr);
    assert_eq!(run.stdout, expected, "{}", run.stderr);
    // Carried, but not registered: an explicit require still finds it.
    assert!(Path::new(&run.emitted).join("app/models/tapioca/fn_compiler.rb").exists());
    let models = std::fs::read_to_string(run.emitted.join("app/models.rb")).unwrap();
    assert!(!models.contains("models/tapioca/fn_compiler"), "an ignored file is registered:\n{models}");
}

#[test]
fn an_unmodeled_loader_feature_is_refused_at_its_line() {
    let root = std::env::temp_dir().join(format!("roundhouse-zeitwerk-refused-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let entry = "module Fn\n  ZeitwerkLoader = Zeitwerk::Loader.for_gem\n  ZeitwerkLoader.inflector.camelize(\"html\", __dir__)\n  ZeitwerkLoader.ignore(\"#{Dir.pwd}/x\")\n  ZeitwerkLoader.setup\nend\n";
    for (path, text) in [
        ("db/schema.rb", "ActiveRecord::Schema.define do\nend\n"),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
        ("Gemfile.lock", "PATH\n  remote: gems/fn\n  specs:\n    fn (0.1.0)\n\nGEM\n  remote: https://rubygems.org/\n  specs:\n"),
        ("gems/fn/lib/fn.rb", entry),
    ] {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_roundhouse")).arg("check").arg(&root).output().unwrap();
    let _ = std::fs::remove_dir_all(&root);
    let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(
        text.contains("gems/fn/lib/fn.rb:3:3: error[ingest_unsupported]: `Zeitwerk::Inflector#camelize` is not modeled"),
        "{text}"
    );
    assert!(text.contains("gems/fn/lib/fn.rb:4:3: error[ingest_unsupported]: `Zeitwerk::Loader#ignore` of a path"), "{text}");
    assert_eq!(text.matches("error[ingest_unsupported]").count(), 2, "{text}");
}

/// graphql-shopify's shape: a file-level loader extending a namespace
/// the entry file does not own, with literal inflections, an ignored
/// directory and `eager_load_dir` calls. Zeitwerk skips an
/// `eager_load_dir` whose namespace is not defined yet (here `Host::Ext`,
/// whose file is the one being required), and loads one whose namespace
/// is.
const EXT_ENTRY: &str = r##"require "active_support/core_ext/string/inflections"
require "zeitwerk"

module Host
  LOG = []
end

LOADER = Zeitwerk::Loader.for_gem_extension(Host) #: Zeitwerk::Loader
LOADER.inflector.inflect(
  "html" => "HTML",
)
LOADER.ignore("#{__dir__}/ext/optional")
LOADER.setup
LOADER.eager_load_dir("#{__dir__}/ext/patches")
LOADER.eager_load_dir("#{__dir__}/tools")
LOADER.eager_load_dir("#{__dir__}/ext/optional")

module Host
  module Ext
    TOPIC = "ext"
  end
end

Host::LOG << :entry
"##;

const EXT_HTML: &str = r#"module Host
  module Ext
    module HTML
      NAME = "html"
    end
  end
end
"#;

const EXT_PATCH: &str = r##"module Host
  module Ext
    module Patches
      class Greeting
        ORDER = (Host::LOG << :greeting).size
        def self.hi = "hi #{Host::Ext::TOPIC}"
      end
    end
  end
end
"##;

const EXT_TOOL: &str = r#"module Host
  module Tools
    class Hammer
      ORDER = (Host::LOG << :hammer).size
      def self.hit = "bang"
    end
  end
end
"#;

/// Natively a NameError if loaded: the loader ignores the directory.
const EXT_OPTIONAL: &str = r#"module Host
  module Ext
    module Optional
      class Thing < Missing::Base
      end
    end
  end
end
"#;

const EXT_REPORT: &str = r#"out = []
out << Host::LOG.inspect
out << Host::Ext::HTML::NAME
out << Host::Tools::Hammer.hit
out << Host::Ext::Patches::Greeting.hi
out << Host::LOG.inspect
out << defined?(Host::Ext::Optional).inspect
out << defined?(Missing).inspect
puts out.join("|")
"#;

#[test]
fn a_gem_extension_loader_inflects_ignores_and_eager_loads_directories() {
    let files = [
        ("gems/host-ext/lib/host/ext.rb", EXT_ENTRY),
        ("gems/host-ext/lib/host/ext/html.rb", EXT_HTML),
        ("gems/host-ext/lib/host/ext/patches/greeting.rb", EXT_PATCH),
        ("gems/host-ext/lib/host/tools/hammer.rb", EXT_TOOL),
        ("gems/host-ext/lib/host/ext/optional/thing.rb", EXT_OPTIONAL),
    ];
    let native_root = std::env::temp_dir().join(format!("roundhouse-zeitwerk-ext-native-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&native_root);
    for (path, text) in files {
        let file = native_root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let native = emit_and_run::ruby()
        .arg("-I")
        .arg(native_root.join("gems/host-ext/lib"))
        .arg("-e")
        .arg(format!("require \"host/ext\"\n{EXT_REPORT}"))
        .output()
        .expect("spawn ruby");
    let _ = std::fs::remove_dir_all(&native_root);
    assert!(native.status.success(), "native oracle failed:\n{}", String::from_utf8_lossy(&native.stderr));
    let expected = String::from_utf8_lossy(&native.stdout).into_owned();
    assert_eq!(
        expected.trim(),
        "[:hammer, :entry]|html|bang|hi ext|[:hammer, :entry, :greeting]|nil|nil",
        "native oracle"
    );

    let mut overlay = emit_and_run::real_blog().edit(
        "Gemfile.lock",
        "GEM\n  remote: https://rubygems.org/\n",
        "PATH\n  remote: gems/host-ext\n  specs:\n    host-ext (0.1.0)\n\nGEM\n  remote: https://rubygems.org/\n",
    );
    for (path, text) in files {
        overlay = overlay.write(path, text);
    }
    let run = overlay.run_ruby(EXT_REPORT);
    assert!(run.errors.is_empty(), "{}", run.errors.join("\n"));
    assert!(run.success, "emitted program failed in {}:\n{}", run.emitted.display(), run.stderr);
    assert_eq!(run.stdout, expected, "{}", run.stderr);
    // The entry's Rails feature require is the runtime ports' (graphql-shopify's
    // `require "active_support/core_ext/class/attribute"`); it was kept as a
    // file statement and failed at load with a LoadError.
    assert!(!run.stderr.contains("file_statement_not_run"), "{}", run.stderr);
    let models = std::fs::read_to_string(run.emitted.join("app/models.rb")).unwrap();
    assert!(!models.contains("optional/thing"), "an ignored file is registered:\n{models}");
}

#[test]
fn unmodeled_file_level_loader_calls_are_refused_at_their_lines() {
    let root = std::env::temp_dir().join(format!("roundhouse-zeitwerk-file-refused-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let entry = "module Host\nend\n\nLOADER = Zeitwerk::Loader.for_gem_extension(Host)\nLOADER.collapse(\"#{__dir__}/x\")\nLOADER.inflector.inflect(\"a\" => name)\nLOADER.eager_load_dir(\"#{__dir__}/ext\")\nOTHER = Zeitwerk::Loader.for_gem_extension(Host.itself)\n";
    for (path, text) in [
        ("db/schema.rb", "ActiveRecord::Schema.define do\nend\n"),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
        ("Gemfile.lock", "PATH\n  remote: gems/host-ext\n  specs:\n    host-ext (0.1.0)\n\nGEM\n  remote: https://rubygems.org/\n  specs:\n"),
        ("gems/host-ext/lib/host/ext.rb", entry),
    ] {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_roundhouse")).arg("check").arg(&root).output().unwrap();
    let _ = std::fs::remove_dir_all(&root);
    let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    for expected in [
        "ext.rb:5:1: error[file_statement_unsupported]: file-level statement `Zeitwerk::Loader#collapse` does not run",
        "`Zeitwerk::Loader#collapse` is not modeled",
        "ext.rb:6:1: error[file_statement_unsupported]: file-level statement `Zeitwerk::Inflector#inflect` does not run",
        "`Zeitwerk::Inflector#inflect` of anything but one literal",
        "ext.rb:7:1: error[file_statement_unsupported]: file-level statement `Zeitwerk::Loader#eager_load_dir` does not run",
        "`Zeitwerk::Loader#eager_load_dir` before `setup` raises Zeitwerk::SetupRequired",
        "ext.rb:8:9: error[ingest_unsupported]: `Zeitwerk::Loader.for_gem_extension` of anything but a constant",
    ] {
        assert!(text.contains(expected), "missing {expected}:\n{text}");
    }
}
