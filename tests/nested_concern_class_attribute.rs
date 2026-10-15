//! A `class_attribute` a Concern declares in `included do`, reached
//! through another Concern the controller includes. Core's
//! `LoginProtection` includes `ApiVersioningConcern`, whose block
//! declares `api_change_monitor_track_changes`, and `AdminAreaController`'s
//! `log_api_change_usage` (a `ClassMethods` macro writing it) raised
//! NoMethodError for the writer: the nested carrier's `class_attribute`
//! was never run on the controller. The runtime macro is the PostgreSQL
//! CRuby tree's (`apply_postgres_bootstrap`), so the app names that
//! adapter.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const VERSIONING: &str = r#"module VersioningProbe
  extend ActiveSupport::Concern

  included do
    class_attribute :track_changes, default: false, instance_accessor: false
  end

  module ClassMethods
    def log_usage
      self.track_changes = true
    end
  end
end
"#;

const LOGIN: &str = r#"module LoginProbe
  extend ActiveSupport::Concern
  include VersioningProbe

  included do
    log_usage
  end
end
"#;

const APPLICATION: &str = "class ApplicationController < ActionController::Base\nend\n";
const DRAFTS: &str = "class DraftsController < ApplicationController\n  include LoginProbe\n\n  def index\n    head :ok\n  end\nend\n";
const OTHER: &str = "class OtherController < ApplicationController\n  def index\n    head :ok\n  end\nend\n";

fn overlay() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "database_adapter: postgresql\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\n  get \"drafts\", to: \"drafts#index\"\n  get \"other\", to: \"other#index\"\nend\n")
        .write("app/controllers/application_controller.rb", APPLICATION)
        .write("app/controllers/concerns/versioning_probe.rb", VERSIONING)
        .write("app/controllers/concerns/login_probe.rb", LOGIN)
        .write("app/controllers/drafts_controller.rb", DRAFTS)
        .write("app/controllers/other_controller.rb", OTHER)
}

fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

/// The macro is replayed on the includer, ahead of the Concern's own
/// block, and the includer's tree carries the port that defines it.
#[test]
fn the_nested_carriers_class_attribute_runs_on_the_includer() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let read = |rel: &str| std::fs::read_to_string(emitted.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
    let drafts = read("app/controllers/drafts_controller.rb");
    let declared = drafts.find("class_attribute :track_changes").unwrap_or_else(|| panic!("{drafts}"));
    let written = drafts.find("log_usage").unwrap_or_else(|| panic!("{drafts}"));
    assert!(declared < written, "{drafts}");
    assert!(emitted.join("runtime/active_support_class_attribute.rb").exists());
    let other = read("app/controllers/other_controller.rb");
    assert!(!other.contains("class_attribute"), "{other}");
}

/// The emitted controller boots on Core's gems (no database is
/// touched) and `log_usage`'s write through the macro's writer landed on
/// the includer only: Rails' answer for `class_attribute`.
#[test]
#[ignore = "requires Core's gem home (CORE_GEM_HOME) and its jemalloc"]
fn the_nested_carriers_writer_is_callable() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let gem_home = std::env::var("CORE_GEM_HOME")
        .unwrap_or_else(|_| "/nix/store/d958gj4a8pmld3fxa7fhyqasz61cafch-ruby-gems-dev/lib/ruby/gems/4.0.0".into());
    let jemalloc = std::env::var("RH_JEMALLOC").unwrap_or_else(|_| {
        "/nix/store/c04j2i0fkin1bw5fab35gx896558knsd-jemalloc-5.3.0-unstable-2025-09-12/lib/libjemalloc.so.2".into()
    });
    let script = "require File.expand_path('main', Dir.pwd); \
        require File.expand_path('app/controllers/drafts_controller', Dir.pwd); \
        puts DraftsController.track_changes.inspect; \
        puts OtherController.respond_to?(:track_changes)";
    let output = std::process::Command::new("ruby")
        .arg("-e")
        .arg(script)
        .current_dir(&*emitted)
        .env("GEM_HOME", &gem_home)
        .env("GEM_PATH", &gem_home)
        .env("LD_PRELOAD", &jemalloc)
        .output()
        .expect("spawn ruby");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.lines().collect::<Vec<_>>(), ["true", "false"], "{stdout}\n{}", String::from_utf8_lossy(&output.stderr));
}
