//! An installed gem's engine `isolate_namespace` prefixes the app's models
//! in its namespace.
//!
//! Core's components/platform/essentials declares
//! `MaintenanceTasks::CrossPodRun < ::ApplicationRecord` with no
//! `table_name`. The maintenance_tasks 2.17.0 gem (`gem
//! "maintenance_tasks"`, required by `Bundler.require`) loads
//! `lib/maintenance_tasks/engine.rb`, whose `isolate_namespace
//! MaintenanceTasks` defines `MaintenanceTasks.table_name_prefix` as
//! `maintenance_tasks_`, so the model reads
//! `maintenance_tasks_cross_pod_runs`; roundhouse read `cross_pod_runs`.
//! The gem is not carried as source: the compiler reads the engine file
//! from the gem home at the locked version. A gem Bundler does not require
//! never runs its engine, and an app module's own `table_name_prefix` is
//! redefined after the engine's and wins, as in Rails. The expectations
//! are native Rails' answers for the same gems and app files.

use std::path::{Path, PathBuf};
use std::process::Command;

const POD_ENGINE: &str = "module PodTasks\n  class Engine < ::Rails::Engine\n    isolate_namespace PodTasks\n  end\nend\n";
const QUIET_ENGINE: &str = "module QuietEngine\n  class Engine < ::Rails::Engine\n    isolate_namespace QuietEngine\n  end\nend\n";
const BILL_ENGINE: &str =
    "module Billing\n  module Engines\n    class Engine < ::Rails::Engine\n      isolate_namespace Billing\n    end\n  end\nend\n";

/// `(gem, version, [(path under the gem dir, source)])`.
const GEMS: &[(&str, &str, &[(&str, &str)])] = &[
    ("pod_tasks", "2.1.0", &[("lib/pod_tasks.rb", "require \"pod_tasks/engine\"\n"), ("lib/pod_tasks/engine.rb", POD_ENGINE)]),
    (
        "quiet_engine",
        "0.3.0",
        &[("lib/quiet_engine.rb", "require \"quiet_engine/engine\"\n"), ("lib/quiet_engine/engine.rb", QUIET_ENGINE)],
    ),
    (
        "billing-engines",
        "1.0.0",
        &[("lib/billing/engines.rb", "require \"billing/engines/engine\"\n"), ("lib/billing/engines/engine.rb", BILL_ENGINE)],
    ),
];

const GEMFILE: &str = "source \"https://rubygems.org\"\ngem \"rails\"\ngem \"pod_tasks\"\ngem \"quiet_engine\", require: false\ngem \"billing-engines\"\n";
const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    billing-engines (1.0.0)\n    pod_tasks (2.1.0)\n    quiet_engine (0.3.0)\n    rails (8.1.4)\n\nDEPENDENCIES\n  billing-engines\n  pod_tasks\n  quiet_engine\n  rails\n";

/// The app's model files, in the order the native control loads them.
const MODELS: &[(&str, &str)] = &[
    ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
    ("app/models/billing.rb", "module Billing\n  def self.table_name_prefix\n    \"bill_\"\n  end\nend\n"),
    ("app/models/pod_tasks/cross_pod_run.rb", "module PodTasks\n  class CrossPodRun < ::ApplicationRecord\n  end\nend\n"),
    ("app/models/quiet_engine/note.rb", "module QuietEngine\n  class Note < ::ApplicationRecord\n  end\nend\n"),
    ("app/models/billing/invoice.rb", "module Billing\n  class Invoice < ::ApplicationRecord\n  end\nend\n"),
];

const READER: &str = "class Reader\n  #: (PodTasks::CrossPodRun) -> String\n  def self.task(run) = run.task_name\nend\n";

const SCHEMA: &str = "ActiveRecord::Schema.define do\n  create_table :pod_tasks_cross_pod_runs do |t|\n    t.string :task_name, null: false\n  end\n  create_table :notes do |t|\n  end\n  create_table :bill_invoices do |t|\n  end\nend\n";

const NAMES: &[&str] = &["PodTasks::CrossPodRun", "QuietEngine::Note", "Billing::Invoice"];

fn scratch(tag: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "rh_gem_engine_prefix_{tag}_{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// A gem home laid out as RubyGems installs one.
fn gem_home() -> PathBuf {
    let home = scratch("home");
    for (gem, version, files) in GEMS {
        write(
            &home,
            &format!("specifications/{gem}-{version}.gemspec"),
            &format!("Gem::Specification.new do |s|\n  s.name = \"{gem}\".freeze\n  s.require_paths = [\"lib\".freeze]\nend\n"),
        );
        for (rel, source) in *files {
            write(&home, &format!("gems/{gem}-{version}/{rel}"), source);
        }
    }
    home
}

/// Native Rails 8.1.4: Bundler.require's requires (the default require
/// path, `-` as `/` when the name does not load), then the app's models.
fn native_tables(home: &Path) -> Vec<String> {
    let includes: Vec<String> = GEMS
        .iter()
        .map(|(gem, version, _)| format!("-I{}", home.join(format!("gems/{gem}-{version}/lib")).display()))
        .collect();
    let models: String = MODELS.iter().map(|(_, source)| *source).collect();
    let names: Vec<String> = NAMES.iter().map(|n| format!("puts {n}.table_name")).collect();
    let script = format!(
        "require \"rails\"\nrequire \"active_record\"\nrequire \"pod_tasks\"\nrequire \"billing/engines\"\n{models}{}\n",
        names.join("\n")
    );
    let out = Command::new("ruby").arg("-W0").args(&includes).arg("-e").arg(&script).output().expect("ruby is on PATH");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect()
}

#[test]
fn a_required_gem_engines_isolated_namespace_prefixes_app_models() {
    let home = gem_home();
    let native = native_tables(&home);
    assert_eq!(native, ["pod_tasks_cross_pod_runs", "notes", "bill_invoices"]);

    let app_dir = scratch("app");
    for (rel, source) in MODELS {
        write(&app_dir, rel, source);
    }
    write(&app_dir, "app/models/reader.rb", READER);
    write(&app_dir, "db/schema.rb", SCHEMA);
    write(&app_dir, "config/routes.rb", "Rails.application.routes.draw do\nend\n");
    write(&app_dir, "config/application.rb", "require_relative \"boot\"\nrequire \"rails/all\"\nBundler.require(*Rails.groups)\n");
    write(&app_dir, "config/boot.rb", "require \"bundler/setup\"\n");
    write(&app_dir, "Gemfile", GEMFILE);
    write(&app_dir, "Gemfile.lock", LOCK);
    write(&app_dir, "roundhouse.yml", &format!("gem_home: {}\n", home.display()));
    let ingested = roundhouse::ingest::ingest_app(&app_dir);
    let _ = std::fs::remove_dir_all(&home);
    let mut app = ingested.expect("ingest");
    let tables: Vec<String> = NAMES
        .iter()
        .map(|name| {
            app.models
                .iter()
                .find(|m| m.name.0.as_str() == *name)
                .unwrap_or_else(|| panic!("{name} is a model"))
                .table
                .0
                .to_string()
        })
        .collect();
    assert_eq!(tables, native);
    roundhouse::session::analyze_and_lower(&mut app);
    let errors: Vec<String> = roundhouse::analyze::diagnose(&app).into_iter().map(|d| d.to_string()).collect();
    let _ = std::fs::remove_dir_all(&app_dir);
    assert!(!errors.iter().any(|e| e.contains("task_name") || e.contains("model_table_not_in_schema")), "{errors:#?}");
}
