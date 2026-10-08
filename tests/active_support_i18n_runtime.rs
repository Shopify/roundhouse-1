// active_support/i18n.rb (runtime/spinel/active_support_i18n.rb, with
// Active Support's English locale carried verbatim at
// runtime/spinel/active_support/locale/) and ActiveSupport::Ractors
// (runtime/spinel/active_support_ractors.rb) against the Rails native Core
// runs (Shopify/rails 5813559f4aed, 8.2.0.alpha;
// tests/support/rails_oracle.rb). tests/active_support_i18n_runtime.rb
// runs the same observations through the pinned activesupport and through
// the ports and requires every one equal.
//
// The ports ride with the PostgreSQL CRuby tree whose lockfile has i18n:
// `require "rails"` loads active_support/i18n before Bundler.require, so
// I18n's load path has Active Support's two locale files when a gem (or
// the app) first reads it. worldwide 1.29.0 reads I18n as it loads.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn i18n_answers_like_rails() {
    let out = Command::new("ruby")
        .arg(root().join("tests/active_support_i18n_runtime.rb"))
        .arg("compare")
        .current_dir(root())
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("Native Rails and emitted active_support/i18n agree"),
        "active_support/i18n diverged from Rails\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    assert!(stdout.lines().count() > 60, "too few observations:\n{stdout}");
    for expected in [
        "load_path: [\"active_support/locale/en.yml\", \"active_support/locale/en.rb\"]",
        "t support.array.last_word_connector: \"\\\", and \\\"\"",
        "ordinalize 22: \"22nd\"",
        "ordinal 111: \"th\"",
        "I18n.fallbacks: \"[:\\\"fr-CA\\\", :fr]\"",
    ] {
        assert!(stdout.contains(expected), "missing {expected:?}:\n{stdout}");
    }
}

const LOCK_WITH_I18N: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    concurrent-ruby (1.3.5)\n    i18n (1.15.2)\n      concurrent-ruby (~> 1.0)\n\nDEPENDENCIES\n  i18n\n";

fn probe_app(lock: Option<&str>) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rh_as_i18n_{}_{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
    let mut files = vec![
        ("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n".to_string()),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n".to_string()),
        ("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table \"probes\" do |t|\n    t.string \"title\"\n  end\nend\n".to_string()),
        ("app/models/probe.rb", "class Probe < ApplicationRecord\nend\n".to_string()),
        (
            "config/application.rb",
            "require \"rails/all\"\n\nmodule Probe\n  class Application < Rails::Application\n    config.load_defaults 8.1\n  end\nend\n".to_string(),
        ),
        ("config/database.yml", "test:\n  adapter: postgresql\n  database: roundhouse_unused\n".to_string()),
        ("roundhouse.yml", "database_adapter: postgresql\n".to_string()),
    ];
    if let Some(lock) = lock {
        files.push(("Gemfile.lock", lock.to_string()));
    }
    for (path, text) in files {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    dir
}

fn tree(dir: &Path) -> Vec<(String, String)> {
    use roundhouse::project::{target_files, BuildTarget};
    let mut app = roundhouse::ingest::ingest_app(dir).expect("ingest probe app");
    roundhouse::session::analyze_and_lower(&mut app);
    assert!(app.database_yml.is_some(), "not a PostgreSQL app: {:?}", app.database_refusals);
    let (files, _) = roundhouse::emit::diagnostics::scope(|| target_files(&app, dir, BuildTarget::Ruby));
    files.expect("package the PostgreSQL ruby tree")
}

/// The PostgreSQL CRuby tree of an app whose lockfile has i18n carries
/// both ports and the two locale files verbatim, and gem_facades.rb loads
/// active_support/i18n right after the Inflector, before any gem.
#[test]
fn the_postgresql_ruby_tree_carries_active_support_i18n() {
    let dir = probe_app(Some(LOCK_WITH_I18N));
    let files = tree(&dir);
    let source = |path: &str| {
        files
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, text)| text.as_str())
            .unwrap_or_else(|| panic!("tree lacks {path}"))
    };
    for (emitted, carried) in [
        ("runtime/active_support_i18n.rb", "runtime/spinel/active_support_i18n.rb"),
        ("runtime/active_support_ractors.rb", "runtime/spinel/active_support_ractors.rb"),
        ("runtime/active_support/locale/en.yml", "runtime/spinel/active_support/locale/en.yml"),
        ("runtime/active_support/locale/en.rb", "runtime/spinel/active_support/locale/en.rb"),
    ] {
        assert_eq!(source(emitted), std::fs::read_to_string(root().join(carried)).unwrap(), "{emitted}");
    }
    assert!(source("runtime/features/active_support/i18n.rb").contains("require_relative \"../../active_support_i18n\""));
    let facades = source("runtime/gem_facades.rb");
    let inflector = facades.find("require_relative \"active_support_inflector\"\n").expect("facades load the Inflector");
    let i18n = facades.find("require_relative \"active_support_i18n\"\n").expect("facades load active_support/i18n");
    assert!(inflector < i18n, "{facades}");
    let first_gem = facades.find("].each do |gem_name|").expect("facades require the gem list");
    assert!(i18n < first_gem, "{facades}");
    std::fs::remove_dir_all(dir).unwrap();
}

/// Without i18n in the lockfile nothing loads it: the tree keeps
/// ActiveSupport::Ractors and leaves active_support/i18n out.
#[test]
fn a_tree_without_i18n_in_its_lockfile_does_not_load_it() {
    let dir = probe_app(None);
    let files = tree(&dir);
    assert!(files.iter().any(|(p, _)| p == "runtime/active_support_ractors.rb"));
    assert!(!files.iter().any(|(p, _)| p == "runtime/active_support_i18n.rb" || p.starts_with("runtime/active_support/locale/")));
    let facades = &files.iter().find(|(p, _)| p == "runtime/gem_facades.rb").expect("gem_facades.rb").1;
    assert!(!facades.contains("active_support_i18n"), "{facades}");
    std::fs::remove_dir_all(dir).unwrap();
}
