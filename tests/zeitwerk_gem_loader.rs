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

    // The app's Gemfile lists the gem, so Bundler.require loads its
    // entry at boot (`host-ext` => `host/ext`), as the oracle requires it.
    let mut overlay = emit_and_run::real_blog()
        .edit(
            "Gemfile.lock",
            "GEM\n  remote: https://rubygems.org/\n",
            "PATH\n  remote: gems/host-ext\n  specs:\n    host-ext (0.1.0)\n\nGEM\n  remote: https://rubygems.org/\n",
        )
        .edit("Gemfile", "gem \"propshaft\"\n", "gem \"propshaft\"\ngem \"host-ext\", path: \"gems/host-ext\"\n");
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
    let entry = "module Host\nend\n\nLOADER = Zeitwerk::Loader.for_gem_extension(Host)\nLOADER.do_not_eager_load(\"#{__dir__}/x\")\nLOADER.inflector.inflect(\"a\" => name)\nLOADER.eager_load_dir(\"#{__dir__}/ext\")\nOTHER = Zeitwerk::Loader.for_gem_extension(Host.itself)\n";
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
        "ext.rb:5:1: error[file_statement_unsupported]: file-level statement `Zeitwerk::Loader#do_not_eager_load` does not run",
        "`Zeitwerk::Loader#do_not_eager_load` is not modeled",
        "ext.rb:6:1: error[file_statement_unsupported]: file-level statement `Zeitwerk::Inflector#inflect` does not run",
        "`Zeitwerk::Inflector#inflect` of anything but one literal",
        "ext.rb:7:1: error[file_statement_unsupported]: file-level statement `Zeitwerk::Loader#eager_load_dir` does not run",
        "`Zeitwerk::Loader#eager_load_dir` before `setup` raises Zeitwerk::SetupRequired",
        "ext.rb:8:9: error[ingest_unsupported]: `Zeitwerk::Loader.for_gem_extension` of anything but a constant",
    ] {
        assert!(text.contains(expected), "missing {expected}:\n{text}");
    }
}

/// dux-event-reporter 0.4.1's shape (`lib/dux/event_reporter.rb`): the
/// gem's namespace module builds its own loader with
/// `Zeitwerk::Loader.new`, tags it, roots it at the gem's
/// `event_reporter/` directory as that module (`push_dir(dir,
/// namespace: self)`), ignores the version file the entry requires
/// itself, and sets it up.
const DUX_ENTRY: &str = r#"require "zeitwerk"

module Dux
  module EventReporter
    LOADER = Zeitwerk::Loader.new
    private_constant :LOADER

    LOADER.tag = "dux-event-reporter"
    LOADER.push_dir(File.expand_path("event_reporter", __dir__), namespace: self)
    LOADER.ignore(File.expand_path("event_reporter/version.rb", __dir__))
    LOADER.setup
  end
end

require_relative "event_reporter/version"

module Dux
  module EventReporter
    def self.loader
      LOADER
    end
  end
end
"#;

/// Zeitwerk would expect `Dux::EventReporter::Version` here, which is
/// why the gem ignores the file.
const DUX_VERSION: &str = r#"module Dux
  module EventReporter
    VERSION = "0.4.1"
  end
end
"#;

const DUX_FOO: &str = r##"module Dux
  module EventReporter
    class Foo
      def self.hi = "foo #{VERSION}"
    end
  end
end
"##;

const DUX_REPORT: &str = r#"out = []
out << Dux::EventReporter.autoload?(:Foo).nil?
out << Dux::EventReporter::Foo.hi
out << Dux::EventReporter::VERSION
out << defined?(Dux::EventReporter::Version).inspect
out << Dux::EventReporter.loader.eager_load.inspect
puts out.join("|")
"#;

/// Every `RoundhouseAutoload.loader("Name")` an emitted file holds,
/// with the file.
fn emitted_loader_handles(root: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rb") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let mut rest = text.as_str();
            while let Some(at) = rest.find("RoundhouseAutoload.loader(\"") {
                let tail = &rest[at + "RoundhouseAutoload.loader(\"".len()..];
                let end = tail.find('"').unwrap();
                out.push((tail[..end].to_string(), path.display().to_string()));
                rest = &tail[end..];
            }
        }
    }
    out
}

/// The files the generated `LOADERS` lists for `name`, `None` when it
/// has no entry.
fn loaders_entry(models: &str, name: &str) -> Option<Vec<String>> {
    let table = &models[models.find("  LOADERS = {\n")?..];
    let table = &table[..table.find("  }.freeze\n")?];
    let key = format!("    {name:?} => [\n");
    let body = &table[table.find(&key)? + key.len()..];
    let body = &body[..body.find("    ].freeze,")?];
    Some(body.lines().map(|l| l.trim().trim_end_matches(',').trim_matches('"').to_string()).collect())
}

#[test]
fn a_new_loader_rooted_at_a_namespace_dir_autoloads_it_and_is_registered() {
    let files = [
        ("gems/dux-event-reporter/lib/dux/event_reporter.rb", DUX_ENTRY),
        ("gems/dux-event-reporter/lib/dux/event_reporter/version.rb", DUX_VERSION),
        ("gems/dux-event-reporter/lib/dux/event_reporter/foo.rb", DUX_FOO),
    ];
    let native_root = std::env::temp_dir().join(format!("roundhouse-zeitwerk-dux-native-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&native_root);
    for (path, text) in files {
        let file = native_root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let native = emit_and_run::ruby()
        .arg("-I")
        .arg(native_root.join("gems/dux-event-reporter/lib"))
        .arg("-e")
        .arg(format!("require \"dux/event_reporter\"\n{DUX_REPORT}"))
        .output()
        .expect("spawn ruby");
    let _ = std::fs::remove_dir_all(&native_root);
    assert!(native.status.success(), "native oracle failed:\n{}", String::from_utf8_lossy(&native.stderr));
    let expected = String::from_utf8_lossy(&native.stdout).into_owned();
    assert_eq!(expected.trim(), "false|foo 0.4.1|0.4.1|nil|nil", "native oracle");

    // The gem's lib is a support root; the first reference to
    // `Dux::EventReporter` loads the entry, where the oracle requires it.
    let mut overlay = emit_and_run::real_blog().edit(
        "Gemfile.lock",
        "GEM\n  remote: https://rubygems.org/\n",
        "PATH\n  remote: gems/dux-event-reporter\n  specs:\n    dux-event-reporter (0.4.1)\n\nGEM\n  remote: https://rubygems.org/\n",
    );
    for (path, text) in files {
        overlay = overlay.write(path, text);
    }
    let run = overlay.run_ruby(DUX_REPORT);
    assert!(run.errors.is_empty(), "{}", run.errors.join("\n"));
    assert!(run.success, "emitted program failed in {}:\n{}", run.emitted.display(), run.stderr);
    assert_eq!(run.stdout, expected, "{}", run.stderr);
    let models = std::fs::read_to_string(run.emitted.join("app/models.rb")).unwrap();
    let managed = loaders_entry(&models, "Dux::EventReporter::LOADER")
        .unwrap_or_else(|| panic!("no LOADERS entry for Dux::EventReporter::LOADER:\n{models}"));
    assert!(managed.iter().any(|f| f == "models/dux/event_reporter/foo"), "{managed:?}");
    // The ignored version file is the entry's own require, not the loader's.
    assert!(!managed.iter().any(|f| f.contains("version")), "{managed:?}");
    // A handle the tree holds is a loader the tree registers.
    let handles = emitted_loader_handles(&run.emitted);
    assert!(handles.iter().any(|(name, _)| name == "Dux::EventReporter::LOADER"), "{handles:?}");
    for (name, file) in &handles {
        assert!(loaders_entry(&models, name).is_some(), "{file} holds a handle for unregistered loader {name}");
    }
}

/// A `Zeitwerk::Loader.new` whose root this compile cannot read is
/// refused at its own line, so no handle names a loader the tree does
/// not register; `tag=` of a computed value and a `namespace:` Ruby
/// would reject are refused at theirs.
#[test]
fn a_new_loader_without_a_modeled_root_is_refused_at_its_line() {
    let root = std::env::temp_dir().join(format!("roundhouse-zeitwerk-new-refused-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let entry = "module Fn\n  BARE = Zeitwerk::Loader.new\n  BARE.setup\n  COMPUTED = Zeitwerk::Loader.new\n  COMPUTED.push_dir(Dir.pwd)\n  COMPUTED.setup\n  TAGGED = Zeitwerk::Loader.new\n  TAGGED.push_dir(File.join(__dir__, \"fn\"), namespace: self)\n  TAGGED.tag = name\n  TAGGED.setup\n  RELATIVE = Zeitwerk::Loader.new\n  RELATIVE.push_dir(File.join(__dir__, \"fn\"), namespace: Parts)\nend\n\nLOADER = Zeitwerk::Loader.new\nLOADER.push_dir(\"#{__dir__}/fn\", namespace: self)\n";
    for (path, text) in [
        ("db/schema.rb", "ActiveRecord::Schema.define do\nend\n"),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
        ("Gemfile.lock", "PATH\n  remote: gems/fn\n  specs:\n    fn (0.1.0)\n\nGEM\n  remote: https://rubygems.org/\n  specs:\n"),
        ("gems/fn/lib/fn.rb", entry),
        ("gems/fn/lib/fn/widget.rb", "module Fn\n  class Widget\n  end\nend\n"),
    ] {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_roundhouse")).arg("check").arg(&root).output().unwrap();
    let _ = std::fs::remove_dir_all(&root);
    let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    for expected in [
        "fn.rb:2:10: error[ingest_unsupported]: `Zeitwerk::Loader.new` with no `push_dir` on it later in the same body is not modeled",
        "fn.rb:4:14: error[ingest_unsupported]: `Zeitwerk::Loader.new` whose `push_dir` is not modeled: `Zeitwerk::Loader#push_dir` of a path that is not a literal under `__dir__` is not modeled",
        "fn.rb:9:3: error[ingest_unsupported]: `Zeitwerk::Loader#tag=` of anything but a literal string is not modeled",
        "fn.rb:11:14: error[ingest_unsupported]: `Zeitwerk::Loader.new` whose `push_dir` is not modeled: `Zeitwerk::Loader#push_dir` with a relative constant `namespace:` in a class body",
        "fn.rb:15:10: error[ingest_unsupported]: `Zeitwerk::Loader.new` whose `push_dir` is not modeled: `Zeitwerk::Loader#push_dir` with `namespace: self` at file level raises",
    ] {
        assert!(text.contains(expected), "missing {expected}:\n{text}");
    }
    // The calls on a refused loader never run (its constant raises
    // first), so they add nothing.
    for line in [3, 5, 6, 12, 16] {
        assert!(!text.contains(&format!("fn.rb:{line}:")), "line {line} reported:\n{text}");
    }
}
