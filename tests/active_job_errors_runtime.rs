// Active Job's error classes (runtime/spinel/active_job_errors.rb)
// against the Rails native Core runs (Shopify/rails 5813559f4aed,
// 8.2.0.alpha; tests/support/rails_oracle.rb).
// tests/active_job_errors_runtime.rb runs Core's uses through the pinned
// activejob and through the port and requires every observation equal.
//
// The port rides only with the PostgreSQL CRuby tree: hedwig's
// `class JobEnqueueError < ActiveJob::EnqueueError` and the
// unknown-job-class fingerprinter's `matches_on ActiveJob::UnknownJobClassError`
// resolve in `check`; a tree without the port refuses the read by name.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn active_job_errors_answer_like_rails() {
    let out = Command::new("ruby")
        .arg(root().join("tests/active_job_errors_runtime.rb"))
        .arg("compare")
        .current_dir(root())
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("Native Rails and emitted Active Job errors agree"),
        "Active Job errors diverged from Rails\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    assert!(stdout.lines().count() > 40, "too few observations:\n{stdout}");
    for expected in [
        "ActiveJob::UnknownJobClassError superclass: \"NameError\"",
        "unknown name: \"SomeUnknownJob\"",
        "unknown message: \"Failed to instantiate job, class `SomeUnknownJob` doesn't exist\"",
        "hedwig rescued as EnqueueError: \"[\\\"Hedwig::JobEnqueueError\\\", \\\"redis\\\"]\"",
        "deserialization message: \"Error while trying to deserialize arguments: missing record\"",
        "ActiveJob::SerializationError superclass: \"ArgumentError\"",
    ] {
        assert!(stdout.contains(expected), "missing {expected:?}:\n{stdout}");
    }
}

/// hedwig's subclass, the fingerprinter's and Core's raises, in a probe app.
const MODEL: &str = r##"module ProbeQueue
  class JobEnqueueError < ActiveJob::EnqueueError; end
end

class Probe < ApplicationRecord
  def probe(name)
    error = ActiveJob::UnknownJobClassError.new(name)
    grouping = "#{error.class.name}:#{error.name}"
    raise ProbeQueue::JobEnqueueError, grouping if name.empty?
    raise ActiveJob::SerializationError, "unsupported" if name == "x"
    grouping
  rescue ActiveJob::DeserializationError::RecordNotFound, ActiveJob::DeserializationError
    nil
  end
end
"##;
fn probe_app(postgres: bool) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rh_active_job_errors_{}_{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
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
fn active_job_errors_resolve_and_are_refused_where_the_tree_lacks_them() {
    let dir = probe_app(false);
    let (ok, text) = run(&dir, &["check"]);
    assert!(ok, "check refused the Active Job errors: {text}");
    assert!(!text.contains("constant not supported") && !text.contains("no known method"), "{text}");
    // The SQLite ruby tree does not carry the port, nor do the strict targets.
    for target in ["ruby", "rust"] {
        let (ok, text) = run(&dir, &["--target", target]);
        assert!(!ok, "{target} accepted ActiveJob::EnqueueError: {text}");
        assert!(
            text.contains("runtime_constant") && text.contains("runtime/active_job_errors.rb"),
            "{target} lost the named refusal: {text}"
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}

/// The PostgreSQL CRuby tree (`project::apply_postgres_bootstrap`) carries
/// the port verbatim, loads it before the app's code and emits the reads
/// unrefused.
#[test]
fn the_postgresql_ruby_tree_carries_the_active_job_errors() {
    use roundhouse::project::{target_files, BuildTarget};
    let dir = probe_app(true);
    let mut app = roundhouse::ingest::ingest_app(&dir).expect("ingest probe app");
    roundhouse::session::analyze_and_lower(&mut app);
    assert!(app.database_yml.is_some(), "not a PostgreSQL app: {:?}", app.database_refusals);
    let (files, diagnostics) = roundhouse::emit::diagnostics::scope(|| target_files(&app, &dir, BuildTarget::Ruby));
    let files = files.expect("package the PostgreSQL ruby tree");
    let refused: Vec<String> = diagnostics.iter().map(ToString::to_string).filter(|d| d.contains("ActiveJob")).collect();
    assert!(refused.is_empty(), "{refused:?}");
    let source = |path: &str| files.iter().find(|(p, _)| p == path).map(|(_, text)| text.as_str())
        .unwrap_or_else(|| panic!("tree lacks {path}: {:?}", files.iter().map(|(p, _)| p).filter(|p| p.starts_with("app/")).collect::<Vec<_>>()));
    assert_eq!(
        source("runtime/active_job_errors.rb"),
        std::fs::read_to_string(root().join("runtime/spinel/active_job_errors.rb")).unwrap()
    );
    let boot = source("boot.rb");
    let required = boot.find("require_relative \"runtime/active_job_errors\"").expect("boot requires the port");
    assert!(required < boot.find("require_relative \"app/models\"").expect("boot loads app/models"), "{boot}");
    let queue = source("app/models/probe_queue/job_enqueue_error.rb");
    assert!(queue.contains("JobEnqueueError < ActiveJob::EnqueueError") && !queue.contains("not supported"), "{queue}");
    let model = source("app/models/probe.rb");
    assert!(model.contains("ActiveJob::UnknownJobClassError.new(name)") && !model.contains("not supported"), "{model}");
    std::fs::remove_dir_all(dir).unwrap();
}
