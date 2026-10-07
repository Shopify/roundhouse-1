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
    let entry = "module Fn\n  ZeitwerkLoader = Zeitwerk::Loader.for_gem\n  ZeitwerkLoader.inflector.inflect(\"html\" => \"HTML\")\n  ZeitwerkLoader.ignore(\"#{Dir.pwd}/x\")\n  ZeitwerkLoader.setup\nend\n";
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
        text.contains("gems/fn/lib/fn.rb:3:3: error[ingest_unsupported]: `Zeitwerk::Loader#inflector` is not modeled"),
        "{text}"
    );
    assert!(text.contains("gems/fn/lib/fn.rb:4:3: error[ingest_unsupported]: `Zeitwerk::Loader#ignore` of a path"), "{text}");
    assert_eq!(text.matches("error[ingest_unsupported]").count(), 2, "{text}");
}
