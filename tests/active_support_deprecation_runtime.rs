// ActiveSupport::Deprecation (runtime/spinel/active_support_deprecation.rb)
// against the Rails native Core runs (Shopify/rails 5813559f4aed,
// 8.2.0.alpha; tests/support/rails_oracle.rb).
// tests/active_support_deprecation_runtime.rb runs the same deprecators
// through a booted Rails app and through the port and requires every
// observation equal.
//
// The port rides only with the PostgreSQL CRuby tree. Analysis types
// `ActiveSupport::Deprecation.new` and its methods, so a gem's
// `DEPRECATOR = ActiveSupport::Deprecation.new("5.0.0", "ShopifyI18n")`
// resolves in `check`; a tree without the port refuses the read by name.
// The app's `config.active_support.deprecation` reaches every registered
// deprecator after the environment program, as activesupport's
// `active_support.deprecation_behavior` initializer applies it.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn deprecation_answers_like_rails() {
    let out = Command::new("ruby")
        .arg(root().join("tests/active_support_deprecation_runtime.rb"))
        .arg("compare")
        .current_dir(root())
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("Native Rails and emitted ActiveSupport::Deprecation agree"),
        "deprecation diverged from Rails\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    assert!(stdout.lines().count() > 60, "too few observations:\n{stdout}");
    for expected in [
        "gem fields: [\"ShopifyI18n\", \"5.0.0\", nil, false, []]",
        "default stderr: \"DEPRECATION WARNING: old thing (called from Object#stderr_of at <THIS>:",
        "registered silenced: \"\"",
        ":notify: [[\"DEPRECATION WARNING: cents \", \"Shopify/Money\", \"5.0.0\", true]]",
        "disallowed string: \"ActiveSupport::DeprecationException: DEPRECATION WARNING: this is forbidden",
        "options applied later: [true, \"ActiveSupport::DeprecationException: DEPRECATION WARNING: late\"]",
    ] {
        assert!(stdout.contains(expected), "missing {expected:?}:\n{stdout}");
    }
}

/// shopify-i18n's, country_db's and Core's uses, in a probe app.
const MODEL: &str = r#"module ProbeGem
  DEPRECATOR = ActiveSupport::Deprecation.new("5.0.0", "ProbeGem")

  def self.deprecator
    DEPRECATOR
  end
end

class Probe < ApplicationRecord
  def probe
    Rails.application.deprecators[:probe_gem] = ProbeGem.deprecator
    ProbeGem.deprecator.warn("old")
    ActiveSupport::Deprecation.new.warn("bare")
    ProbeGem.deprecator.silence { ProbeGem.deprecator.silenced }
  end
end
"#;

fn probe_app(postgres: bool) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rh_deprecation_{}_{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
    let mut files = vec![
        ("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n".to_string()),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n".to_string()),
        ("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table \"probes\" do |t|\n    t.string \"title\"\n  end\nend\n".to_string()),
        ("app/models/probe.rb", MODEL.to_string()),
        (
            "config/application.rb",
            "require \"rails/all\"\n\nmodule Probe\n  class Application < Rails::Application\n    config.load_defaults 8.1\n  end\nend\n".to_string(),
        ),
        (
            "config/environments/test.rb",
            "Rails.application.configure do\n  config.active_support.deprecation = :silence\nend\n".to_string(),
        ),
        ("config/environments/development.rb", "Rails.application.configure do\n  config.cache_classes = false\nend\n".to_string()),
    ];
    if postgres {
        files.push(("config/database.yml", "test:\n  adapter: postgresql\n  database: roundhouse_unused\n".to_string()));
        files.push(("roundhouse.yml", "database_adapter: postgresql\n".to_string()));
    }
    for (path, text) in files {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    dir
}

fn run(dir: &Path, args: &[&str]) -> (bool, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_roundhouse"));
    command.args(args).arg(dir);
    if args[0] != "check" {
        command.arg("-o").arg(dir.join(format!("emitted-{}", args[1])));
    }
    let output = command.output().unwrap();
    (output.status.success(), format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr)))
}

#[test]
fn deprecation_resolves_and_is_refused_where_the_tree_lacks_it() {
    let dir = probe_app(false);
    let (ok, text) = run(&dir, &["check"]);
    assert!(ok, "check refused ActiveSupport::Deprecation: {text}");
    assert!(!text.contains("constant not supported") && !text.contains("no known method"), "{text}");
    // The SQLite ruby tree does not carry the port, nor do the strict targets.
    for target in ["ruby", "rust"] {
        let (ok, text) = run(&dir, &["--target", target]);
        assert!(!ok, "{target} accepted ActiveSupport::Deprecation: {text}");
        assert!(
            text.contains("runtime_constant") && text.contains("runtime/active_support_deprecation.rb"),
            "{target} lost the named refusal: {text}"
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}

/// The PostgreSQL CRuby tree (`project::apply_postgres_bootstrap`) carries
/// the port verbatim, loads it before the app's code, applies the test
/// environment's `config.active_support.deprecation` before the railties'
/// initializers, and emits the reads unrefused.
#[test]
fn the_postgresql_ruby_tree_carries_the_port_and_applies_the_config() {
    use roundhouse::project::{target_files, BuildTarget};
    let dir = probe_app(true);
    let mut app = roundhouse::ingest::ingest_app(&dir).expect("ingest probe app");
    roundhouse::session::analyze_and_lower(&mut app);
    assert!(app.database_yml.is_some(), "not a PostgreSQL app: {:?}", app.database_refusals);
    let (files, diagnostics) = roundhouse::emit::diagnostics::scope(|| target_files(&app, &dir, BuildTarget::Ruby));
    let files = files.expect("package the PostgreSQL ruby tree");
    let refused: Vec<String> = diagnostics.iter().map(ToString::to_string).filter(|d| d.contains("Deprecation")).collect();
    assert!(refused.is_empty(), "{refused:?}");
    let source = |path: &str| files.iter().find(|(p, _)| p == path).map(|(_, text)| text.as_str())
        .unwrap_or_else(|| panic!("tree lacks {path}"));
    assert_eq!(
        source("runtime/active_support_deprecation.rb"),
        std::fs::read_to_string(root().join("runtime/spinel/active_support_deprecation.rb")).unwrap()
    );
    let boot = source("boot.rb");
    let required = boot.find("require_relative \"runtime/active_support_deprecation\"").expect("boot requires the port");
    assert!(required < boot.find("require_relative \"app/models\"").expect("boot loads app/models"), "{boot}");
    // After the environment program, before the railties' initializers.
    let models = source("app/models.rb");
    let environment = models.find("RoundhouseEnvironmentTest.run_environment").unwrap_or_else(|| panic!("runs the test environment:\n{models}"));
    let applied = models
        .find("Rails::Application.apply_deprecation_behavior(report_deprecations: nil, deprecation: Rails.application.active_support_deprecation, disallowed_deprecation: nil, disallowed_deprecation_warnings: nil)")
        .unwrap_or_else(|| panic!("boot applies the deprecation keys:\n{models}"));
    let railties = models.find("Rails::Railtie.run_initializers_of_railties(Rails.application, :boot)").expect("runs the railties");
    assert!(environment < applied && applied < railties, "{models}");
    assert!(!models.contains("`config.active_support.deprecation` is framework configuration"), "{models}");
    let gem = source("app/models/probe_gem.rb");
    assert!(gem.contains("ActiveSupport::Deprecation.new(\"5.0.0\", \"ProbeGem\")") && !gem.contains("not supported"), "{gem}");
    let model = source("app/models/probe.rb");
    assert!(model.contains("ProbeGem.deprecator.silence") && !model.contains("not supported"), "{model}");
    std::fs::remove_dir_all(dir).unwrap();
}
