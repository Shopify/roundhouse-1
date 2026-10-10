// File.atomic_write (runtime/spinel/active_support_file_atomic.rb)
// against the Rails native Core runs (Shopify/rails 5813559f4aed,
// 8.2.0.alpha; tests/support/rails_oracle.rb).
// tests/active_support_file_atomic_runtime.rb runs the same calls through
// the pinned activesupport and through the port and requires every
// observation equal.
//
// Rails' :load_active_support initializer loads it with active_support/all,
// so Core calls it with no require (r8 peek: 14 tests raised NoMethodError
// `atomic_write` for class File); the emitted PostgreSQL tree loads the port
// where a call names it (project.rs::BOOT_LOADED_CORE_EXTENSIONS).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use std::path::Path;
use std::process::Command;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn file_atomic_write_answers_like_rails() {
    let out = Command::new("ruby")
        .arg(root().join("tests/active_support_file_atomic_runtime.rb"))
        .arg("compare")
        .current_dir(root())
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("Native Rails and emitted File.atomic_write agree"),
        "port diverged from Rails\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    for expected in [
        "new file returns block value: :written",
        "overwrite keeps mode: \"640\"",
        "raise inside block: \"ArgumentError: \\\"boom\\\"\"",
        "content after raise: \"two\"",
        "no temp files left: [\"verdict.json\"]",
    ] {
        assert!(stdout.contains(expected), "missing {expected:?}:\n{stdout}");
    }
}

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    concurrent-ruby (1.3.5)\n    i18n (1.15.2)\n      concurrent-ruby (~> 1.0)\n\n\
DEPENDENCIES\n  i18n\n";

fn app(model: &str) -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\ndatabase_adapter: postgresql\n")
        .write("Gemfile", "source \"https://rubygems.org\"\ngem \"rails\"\ngem \"i18n\"\n")
        .write("Gemfile.lock", LOCK)
        .write("config.ru", "")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_file_atomic_unused\n")
        .write("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table \"probes\" do |t|\n    t.string \"title\"\n  end\nend\n")
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("app/models/snapshot.rb", model)
}

/// App code that calls File.atomic_write (Core's Verdict flag snapshots)
/// loads the port at boot; app code that does not, does not, though a
/// gem's require of the feature still finds it under runtime/features.
#[test]
fn app_code_calling_atomic_write_loads_the_port_at_boot() {
    let (tree, _) = app("class Snapshot\n  def self.save(path, data)\n    ::File.atomic_write(path) { |f| f.write(data) }\n  end\nend\n")
        .emit(BuildTarget::Ruby);
    let facades = std::fs::read_to_string(tree.join("runtime/gem_facades.rb")).unwrap();
    assert!(facades.contains("require_relative \"active_support_file_atomic\"\n"), "port not loaded at boot:\n{facades}");
    assert!(tree.join("runtime/active_support_file_atomic.rb").is_file());
    let feature = std::fs::read_to_string(tree.join("runtime/features/active_support/core_ext/file/atomic.rb")).unwrap();
    assert!(feature.contains("active_support_file_atomic"), "{feature}");

    let (tree, _) = app("class Snapshot\n  # File.atomic_write(path)\n  def self.save(path) = path\nend\n").emit(BuildTarget::Ruby);
    let facades = std::fs::read_to_string(tree.join("runtime/gem_facades.rb")).unwrap();
    assert!(!facades.contains("require_relative \"active_support_file_atomic\""), "loaded without a call:\n{facades}");
}
