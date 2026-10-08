//! `has_attached_class!` is a sorbet-runtime annotation, dropped with the
//! `extend T::Generic` that provides it.
//!
//! sorbet-runtime 0.6.13502 defines `T::Generic#has_attached_class!` with
//! an empty body (lib/types/generic.rb:23): the block never runs and the
//! call returns nil. The annotation pass dropped the `extend T::Generic`
//! (`library_class::is_sorbet_annotation_mixin`) and kept the call, so
//! Core's `CI::TestGroup::FilteredRunnableMethods` (gems/ci
//! test_group.rb:242) and `FeatureSet::ExtensionManagement::ExtensionMethods`
//! raised NoMethodError at load (r17-pre4 boot survey).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

const ORACLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");

// Core's shapes: a `Module` subclass with a fixed attached class
// (test_group.rb, feature_set.rb), a module extended onto records
// (inventory/records/extensions.rb), and a test helper with an upper
// bound and a sig (script_test_helper.rb).
const FILTERED: &str = r#"class FilteredMethods < Module
  extend T::Generic
  has_attached_class! { { fixed: T.anything } }

  def initialize(prefix)
    super()
    @prefix = prefix
  end

  def label
    "filtered:#{@prefix}"
  end
end
"#;

const EXTENSIONS: &str = r#"module RecordExtensions
  extend T::Generic

  has_attached_class!(:out)

  def ext
    :ext
  end
end
"#;

const HELPER: &str = r#"module ScriptHelper
  extend T::Sig
  extend T::Generic

  has_attached_class!(:out) { { upper: Object } }

  sig { returns(String) }
  def script_name
    "helper"
  end
end
"#;

const SCRIPT: &str = r#"p FilteredMethods.new("a").label
p FilteredMethods.ancestors.include?(Module)
o = Object.new
o.extend(RecordExtensions)
p o.ext
p Class.new { include ScriptHelper }.new.script_name
"#;

fn native(program: &str) -> String {
    let dir = std::env::temp_dir().join(format!("rh_sorbet_has_attached_class_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.rb"), format!("require {ORACLE:?}\nRailsOracle.activate(\"sorbet-runtime\": \"0.6.13502\")\nrequire \"sorbet-runtime\"\n{program}")).unwrap();
    let out = Command::new("ruby").arg("main.rb").current_dir(&dir).output().expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "native failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    stdout
}

#[test]
fn has_attached_class_is_dropped_as_the_annotation_sorbet_runtime_makes_it() {
    let expected = native(&format!("{FILTERED}{EXTENSIONS}{HELPER}{SCRIPT}"));
    assert_eq!(expected, "\"filtered:a\"\ntrue\n:ext\n\"helper\"\n", "native control");

    let overlay = || {
        emit_and_run::empty_app()
            .write("config/environments/development.rb", "Rails.application.configure do\n  config.eager_load = false\nend\n")
            .write("lib/filtered_methods.rb", FILTERED)
            .write("lib/record_extensions.rb", EXTENSIONS)
            .write("lib/script_helper.rb", HELPER)
    };
    let (tree, _) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    for file in ["app/models/filtered_methods.rb", "app/models/record_extensions.rb", "app/models/script_helper.rb"] {
        let text = std::fs::read_to_string(tree.join(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
        assert!(!text.contains("has_attached_class!"), "{file}:\n{text}");
        assert!(!text.contains("T::Generic"), "{file}:\n{text}");
    }
    let run = overlay().run_ruby(SCRIPT);
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, expected);
}

/// A model reaches the model lowering's own annotation list
/// (`model_to_library`), not the library-class walk's.
#[test]
fn a_models_has_attached_class_is_dropped_too() {
    let (tree, errors) = emit_and_run::empty_app()
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n")
        .write("app/models/gadget.rb", "class Gadget < ApplicationRecord\n  extend T::Generic\n  has_attached_class!(:out)\nend\n")
        .write("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table \"gadgets\" do |t|\n    t.string \"name\"\n  end\nend\n")
        .emit(roundhouse::project::BuildTarget::Ruby);
    let text = std::fs::read_to_string(tree.join("app/models/gadget.rb")).unwrap();
    assert!(!text.contains("has_attached_class!"), "{text}");
    assert!(!errors.iter().any(|e| e.contains("has_attached_class")), "{errors:?}");
}
