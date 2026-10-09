//! A gem's Zeitwerk loader that collapses directories, compiled.
//!
//! database_documentation 3.2.0's entry file runs
//! `loader.collapse("#{__dir__}/**/concerns")`, so its
//! `model_metadata/concerns/metadata.rb` is
//! `DatabaseDocumentation::ModelMetadata::Metadata`, the concern 260 Core
//! models include and whose `database_documentation` macro their bodies
//! call. The compile refused the call (`Zeitwerk::Loader#collapse` is not
//! modeled) and mapped the file by its path, so the constant was no
//! autoload of it: a model's include raised NameError at load (and the
//! slicer, reading the same path, never carried the file). A collapsed
//! directory now adds no namespace, as Zeitwerk's: the file is the
//! autoload of the parent namespace's constant, the include is kept, and
//! the macro runs. Native Ruby with the real Zeitwerk is the oracle.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const ENTRY: &str = r##"require "zeitwerk"

loader = Zeitwerk::Loader.for_gem
loader.collapse("#{__dir__}/**/concerns")
loader.setup

module G
  LOG = []
end
"##;

/// The concern, under a collapsed directory: `G::M::Meta`.
const META: &str = r##"require "active_support/concern"

module G
  module M
    module Meta
      extend ActiveSupport::Concern

      class_methods do
        def documented(text)
          G::LOG << "#{name}:#{table_name}"
          @doc = Note.new(text)
        end

        def doc
          @doc
        end
      end
    end
  end
end
"##;

/// A sibling that requires the concern by its real path, as
/// database_documentation's annotator.rb does (`require_relative
/// "concerns/metadata"`).
const NOTE: &str = r#"require_relative "concerns/meta"

module G
  module M
    class Note
      attr_reader :text

      def initialize(text)
        @text = text
      end
    end
  end
end
"#;

/// A collapsed directory right under the gem's namespace: `G::Top`.
const TOP: &str = r#"module G
  module Top
    def self.hi = "top"
  end
end
"#;

/// The app's model, as Core's models are: includes the concern and calls
/// its macro in its body, which reads the model's `table_name` as
/// database_documentation's `ModelAnnotation.new(table_name: table_name,
/// ...)` does (the lowered model synthesizes it, so the replay answers it).
const WIDGET: &str = r#"class Widget < ApplicationRecord
  self.table_name = "articles"

  include G::M::Meta

  documented "a widget"
end
"#;

/// The concern is an autoload of `G::M` before anything loads it (Rails
/// autoloads the model on its first reference, as the oracle's
/// `autoload :Widget` does).
const REPORT: &str = r#"out = []
out << G::M.autoload?(:Meta).nil?
out << Widget.doc.text
out << Widget.include?(G::M::Meta)
out << G::LOG.inspect
out << G::M.const_defined?(:Concerns, false)
out << defined?(G::M::Concerns).inspect
out << G::Top.hi
out << G::M::Note.name
puts out.join("|")
"#;

const FILES: [(&str, &str); 4] = [
    ("gems/g/lib/g.rb", ENTRY),
    ("gems/g/lib/g/m/concerns/meta.rb", META),
    ("gems/g/lib/g/m/note.rb", NOTE),
    ("gems/g/lib/g/concerns/top.rb", TOP),
];

#[test]
fn a_collapsed_directory_adds_no_namespace_and_its_concern_is_included() {
    // Native: the gem's lib on the load path, the real Zeitwerk; the model
    // is the app file, loaded after the gem.
    let native_root = std::env::temp_dir().join(format!("roundhouse-zeitwerk-collapse-native-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&native_root);
    for (path, text) in FILES.iter().chain([("app/models/widget.rb", WIDGET)].iter()) {
        let file = native_root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let native = emit_and_run::ruby()
        .arg("-I")
        .arg(native_root.join("gems/g/lib"))
        .arg("-e")
        .arg(format!(
            "require \"active_record\"\nrequire \"g\"\nclass ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\nautoload :Widget, {:?}\n{REPORT}",
            native_root.join("app/models/widget.rb").display().to_string()
        ))
        .output()
        .expect("spawn ruby");
    let _ = std::fs::remove_dir_all(&native_root);
    assert!(native.status.success(), "native oracle failed:\n{}", String::from_utf8_lossy(&native.stderr));
    let expected = String::from_utf8_lossy(&native.stdout).into_owned();
    assert_eq!(expected.trim(), "false|a widget|true|[\"Widget:articles\"]|false|nil|top|G::M::Note", "native oracle");

    // Bundler.require loads the gem's entry at boot, as the oracle requires it.
    let mut overlay = emit_and_run::real_blog()
        .edit(
            "Gemfile.lock",
            "GEM\n  remote: https://rubygems.org/\n",
            "PATH\n  remote: gems/g\n  specs:\n    g (0.1.0)\n\nGEM\n  remote: https://rubygems.org/\n",
        )
        .edit("Gemfile", "gem \"propshaft\"\n", "gem \"propshaft\"\ngem \"g\", path: \"gems/g\"\n")
        .write("app/models/widget.rb", WIDGET);
    for (path, text) in FILES {
        overlay = overlay.write(path, text);
    }
    let run = overlay.run_ruby(REPORT);
    assert!(run.errors.is_empty(), "{}", run.errors.join("\n"));
    assert!(run.success, "emitted program failed in {}:\n{}", run.emitted.display(), run.stderr);
    assert_eq!(run.stdout, expected, "{}", run.stderr);
    // The collapse is applied, not named as a statement the tree skips.
    assert!(!run.stderr.contains("collapse"), "{}", run.stderr);
    let models = std::fs::read_to_string(run.emitted.join("app/models.rb")).unwrap();
    assert!(!models.contains("Concerns"), "a collapsed directory is registered as a namespace:\n{models}");
}

/// The collapse a compile cannot read stays a named refusal at its line.
#[test]
fn a_collapse_of_a_computed_path_is_refused_at_its_line() {
    let root = std::env::temp_dir().join(format!("roundhouse-zeitwerk-collapse-refused-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let entry = "module G\nend\n\nloader = Zeitwerk::Loader.for_gem\nloader.collapse(\"#{Dir.pwd}/concerns\")\nloader.collapse(\"#{__dir__}/**/concerns\") { }\nloader.setup\n";
    for (path, text) in [
        ("db/schema.rb", "ActiveRecord::Schema.define do\nend\n"),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
        ("Gemfile.lock", "PATH\n  remote: gems/g\n  specs:\n    g (0.1.0)\n\nGEM\n  remote: https://rubygems.org/\n  specs:\n"),
        ("gems/g/lib/g.rb", entry),
    ] {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_roundhouse")).arg("check").arg(&root).output().unwrap();
    let _ = std::fs::remove_dir_all(&root);
    let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    for expected in [
        "g.rb:5:1: error[file_statement_unsupported]: file-level statement `Zeitwerk::Loader#collapse` does not run",
        "`Zeitwerk::Loader#collapse` of a path that is not a literal under `__dir__` is not modeled",
        "g.rb:6:1: error[file_statement_unsupported]: file-level statement `Zeitwerk::Loader#collapse` does not run",
        "`Zeitwerk::Loader#collapse` with a block is not modeled",
    ] {
        assert!(text.contains(expected), "missing {expected}:\n{text}");
    }
}
