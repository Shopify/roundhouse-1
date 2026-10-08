// A native gem's Rails feature requires on the PostgreSQL CRuby tree
// (project.rs::FRAMEWORK_FEATURE_PORTS, runtime/features), against the
// Rails native Core runs (Shopify/rails 5813559f4aed;
// tests/support/rails_oracle.rb). tests/framework_feature_requires.rb
// loads one gem natively, after Core's framework railties, and on the
// emitted tree, and requires every observation equal.
//
// Core's boot died in each of these gems (`file_statement_not_run`:
// "cannot load such file -- active_job", job-iteration; "--
// action_controller", responders and typed_parameters; "--
// active_support/multibyte/unicode", identifications): the tree had no
// file for the feature, where natively Rails had loaded it before any
// gem. The features it does not port fail by name instead
// (FRAMEWORK_FEATURES_NOT_PORTED).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use std::path::Path;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    concurrent-ruby (1.3.5)\n    i18n (1.15.2)\n      concurrent-ruby (~> 1.0)\n\n\
DEPENDENCIES\n  i18n\n";

// What a gem in the gem home does as it loads, and what it then reads.
const PROBE_GEM: &str = r#"require "active_support"
require "active_model"
require "active_record"
require "action_controller"
require "action_mailer"
require "active_job"
require "active_support/core_ext/object/blank"
require "active_support/multibyte/unicode"
require "active_support/core_ext/module/attribute_accessors_per_thread"

module ProbeGem
  # turbo-rails 2.x Turbo::Current's shape.
  class Current
    thread_mattr_accessor :request_id
    thread_mattr_accessor :level, default: :info
  end

  def self.report
    unicode = ActiveSupport::Multibyte::Unicode
    latin1 = "caf\xE9".dup.force_encoding(Encoding::UTF_8)
    {
      frameworks: [ActiveRecord::Base, ActionController::Base, ActionMailer::Base, ActiveJob::Base, ActiveModel::Type::Value].map(&:name),
      blank: [nil.blank?, "  ".blank?, [].present?, { a: 1 }.present?],
      unicode_version: unicode::UNICODE_VERSION,
      tidy_bytes: unicode.tidy_bytes(latin1),
      tidy_bytes_valid: unicode.tidy_bytes("café"),
      tidy_bytes_forced: unicode.tidy_bytes("caf\xE9".b, true),
      compose: unicode.compose("é".codepoints),
      decompose: unicode.decompose(:canonical, "é".codepoints),
      decompose_compatibility: unicode.decompose(:compatibility, "ﬁ".codepoints),
      transliterate: ActiveSupport::Inflector.transliterate(latin1),
      parameterize: "Ærøskøbing Straße".parameterize,
      per_thread: per_thread,
    }
  end

  def self.per_thread
    Current.request_id = "main"
    other = Thread.new { [Current.request_id, Current.level, (Current.new.request_id = "t"), Current.request_id] }.value
    invalid = begin
      Class.new { thread_mattr_accessor :"bad-name" }
    rescue NameError => e
      e.message
    end
    [Current.request_id, Current.new.request_id, Current.level, other, invalid]
  end
end
"#;

fn probe() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\ndatabase_adapter: postgresql\n")
        .write("Gemfile", "source \"https://rubygems.org\"\ngem \"rails\"\ngem \"i18n\"\n")
        .write("Gemfile.lock", LOCK)
        .write("config.ru", "")
        .write("config/environments/test.rb", "Rails.application.configure do\n  config.eager_load = false\nend\n")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_framework_features_unused\n")
        .write("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table \"probes\" do |t|\n    t.string \"title\"\n  end\nend\n")
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("app/models/probe.rb", "class Probe < ApplicationRecord\nend\n")
}

fn run(mode: &str, dir: &Path, gem: Option<&Path>) -> String {
    let mut cmd = emit_and_run::ruby();
    cmd.arg(root().join("tests/framework_feature_requires.rb")).arg("run").arg(mode).arg(dir);
    if let Some(gem) = gem {
        cmd.arg(gem);
    }
    let out = cmd.output().expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{mode} failed\n=== stdout ===\n{stdout}\n=== stderr ===\n{}", String::from_utf8_lossy(&out.stderr));
    stdout
}

#[test]
fn a_gems_rails_feature_requires_answer_from_the_ports_as_natively_from_the_loaded_frameworks() {
    let (tree, _) = probe().emit(BuildTarget::Ruby);
    let scratch = tree.parent().expect("scratch").to_path_buf();
    let app = scratch.join("app");
    let gem = scratch.join("probe_gem/lib");
    std::fs::create_dir_all(&gem).unwrap();
    std::fs::write(gem.join("probe_gem.rb"), PROBE_GEM).unwrap();
    let native = run("native", &app, Some(&gem));
    let emitted = run("emitted", &tree, Some(&gem));
    assert_eq!(emitted, native, "the tree's ports answered the gem differently from Rails");
    // Not vacuous: the gem ran and read each framework.
    for expected in [
        "frameworks: [\"ActiveRecord::Base\", \"ActionController::Base\", \"ActionMailer::Base\", \"ActiveJob::Base\", \"ActiveModel::Type::Value\"]",
        "tidy_bytes: \"café\"",
        "compose: [233]",
        "decompose_compatibility: [102, 105]",
        "transliterate: \"cafe\"",
        // String#parameterize through the Inflector (not the ASCII overlay).
        "parameterize: \"aeroskobing-strasse\"",
        // Per thread, with the default seeded in each thread.
        "per_thread: [\"main\", \"main\", :info, [nil, :info, \"t\", \"t\"], \"invalid attribute name: bad-name\"]",
    ] {
        assert!(native.contains(expected), "missing {expected:?}:\n{native}");
    }
}

/// The features the tree does not port fail by name, never loading the
/// gem home's activesupport.
#[test]
fn a_rails_feature_the_tree_does_not_port_is_a_named_load_error() {
    let (tree, _) = probe().emit(BuildTarget::Ruby);
    let out = run("refusals", &tree, None);
    for feature in ["active_support/all", "active_support/core_ext/hash"] {
        let line = format!("{feature}: LoadError: roundhouse: error[framework_feature_not_ported]: `require {feature:?}`");
        assert!(out.contains(&line), "missing {line:?}:\n{out}");
    }
    let features = tree.join("runtime/features");
    let read = |feature: &str| std::fs::read_to_string(features.join(format!("{feature}.rb"))).unwrap_or_else(|e| panic!("{feature}: {e}"));
    assert!(read("active_job").contains("require_relative \"../active_job\"\n"), "{}", read("active_job"));
    assert!(read("active_support/multibyte/unicode").contains("require_relative \"../../../active_support_multibyte_unicode\"\n"));
    let facades = std::fs::read_to_string(tree.join("runtime/gem_facades.rb")).unwrap();
    assert!(facades.contains("require_relative \"active_support_inflector\"\nrequire_relative \"active_support_multibyte_unicode\"\n"), "{facades}");
}
