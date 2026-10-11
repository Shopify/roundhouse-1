// ActionDispatch::Session::AbstractStore, ActionDispatch::Constants and
// ActionDispatch::ShowExceptions (runtime/spinel/action_dispatch_session_store.rb,
// runtime/spinel/action_dispatch_show_exceptions.rb) against the Rails
// native Core runs (Shopify/rails 5813559f4aed, 8.2.0.alpha;
// tests/support/rails_oracle.rb). tests/action_dispatch_session_store_runtime.rb
// runs Core's uses through the pinned actionpack and through the ports,
// both over rack 2.2.24, and requires every observation equal.
//
// The ports ride only with the PostgreSQL CRuby tree: Core's
// `class MultiSessionStore < ActionDispatch::Session::AbstractStore` and
// `class SessionUnavailableMiddleware < ActionDispatch::ShowExceptions`
// resolve in `check`; a tree without the ports refuses the read by name.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn session_stores_answer_like_rails() {
    let out = Command::new("ruby")
        .arg(root().join("tests/action_dispatch_session_store_runtime.rb"))
        .arg("compare")
        .current_dir(root())
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("Native Rails and emitted ActionDispatch session stores agree"),
        "session stores diverged from Rails\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    assert!(stdout.lines().count() > 40, "too few observations:\n{stdout}");
    for expected in [
        "rack: \"2.2.24\"",
        "store key: \"_session_id\"",
        "redis default_options: {path: \"/\", domain: nil, expire_after: 3600, secure: true, httponly: true, defer: false, renew: false, key_prefix: \"core:\", fallback_to_readslave: true}",
        "stale check present class retries: \"[:retried, 2]\"",
        "middleware ivars: [:@app, :@exceptions_app]",
        "render_exception with the exception: [\"Error during failsafe response: undefined method 'status_code' for an instance of SessionUnavailable\", [500,",
    ] {
        assert!(stdout.contains(expected), "missing {expected:?}:\n{stdout}");
    }
}

/// Core's MultiSessionStore and SessionUnavailableMiddleware, in a probe app.
const MODEL: &str = r##"class ProbeSessionStore < ActionDispatch::Session::AbstractStore
  class CookieDomainMismatch < StandardError; end
  DEFAULT_OPTIONS = ActionDispatch::Session::AbstractStore::DEFAULT_OPTIONS.merge(expire_after: 3600)

  def instance(env)
    store = self.class.new(nil, key: "_probe_session_id", secure: true)
    raise CookieDomainMismatch, "no domain" if store.default_options[:domain]
    store
  end
end

class ProbeSessionMiddleware < ActionDispatch::ShowExceptions
  def initialize(app)
    super(app, Rails.application.config.exceptions_app)
  end

  def call(env)
    @app.call(env)
  rescue ActionDispatch::Session::SessionRestoreError => e
    render_exception(env, e)
  end
end
"##;
fn probe_app(postgres: bool) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rh_session_store_{}_{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
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
fn session_stores_resolve_and_are_refused_where_the_tree_lacks_them() {
    let dir = probe_app(false);
    let (ok, text) = run(&dir, &["check"]);
    assert!(ok, "check refused the session stores: {text}");
    assert!(!text.contains("constant not supported") && !text.contains("no known method"), "{text}");
    // The SQLite ruby tree does not carry the port, nor do the strict targets.
    for target in ["ruby", "rust"] {
        let (ok, text) = run(&dir, &["--target", target]);
        assert!(!ok, "{target} accepted ActionDispatch::Session::AbstractStore: {text}");
        assert!(
            text.contains("runtime_constant") && text.contains("runtime/action_dispatch_session_store.rb") && text.contains("runtime/action_dispatch_show_exceptions.rb"),
            "{target} lost the named refusal: {text}"
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}

/// The PostgreSQL CRuby tree (`project::apply_postgres_bootstrap`) carries
/// the port verbatim, loads it before the app's code and emits the reads
/// unrefused.
#[test]
fn the_postgresql_ruby_tree_carries_the_session_stores() {
    use roundhouse::project::{target_files, BuildTarget};
    let dir = probe_app(true);
    let mut app = roundhouse::ingest::ingest_app(&dir).expect("ingest probe app");
    roundhouse::session::analyze_and_lower(&mut app);
    assert!(app.database_yml.is_some(), "not a PostgreSQL app: {:?}", app.database_refusals);
    let (files, diagnostics) = roundhouse::emit::diagnostics::scope(|| target_files(&app, &dir, BuildTarget::Ruby));
    let files = files.expect("package the PostgreSQL ruby tree");
    let refused: Vec<String> = diagnostics.iter().map(ToString::to_string).filter(|d| d.contains("ActionDispatch")).collect();
    assert!(refused.is_empty(), "{refused:?}");
    let source = |path: &str| files.iter().find(|(p, _)| p == path).map(|(_, text)| text.as_str())
        .unwrap_or_else(|| panic!("tree lacks {path}: {:?}", files.iter().map(|(p, _)| p).filter(|p| p.starts_with("app/")).collect::<Vec<_>>()));
    let boot = source("boot.rb");
    let models_at = boot.find("require_relative \"app/models\"").expect("boot loads app/models");
    for port in ["action_dispatch_session_store", "action_dispatch_show_exceptions"] {
        assert_eq!(
            source(&format!("runtime/{port}.rb")),
            std::fs::read_to_string(root().join(format!("runtime/spinel/{port}.rb"))).unwrap()
        );
        let required = boot.find(&format!("require_relative \"runtime/{port}\"")).unwrap_or_else(|| panic!("boot requires {port}"));
        assert!(required < models_at, "{boot}");
    }
    // The shared session object loads first: the port reopens it.
    let shared = boot.find("require_relative \"runtime/action_dispatch/session\"");
    if let Some(shared) = shared {
        assert!(shared < boot.find("require_relative \"runtime/action_dispatch_session_store\"").unwrap(), "{boot}");
    }
    let store = source("app/models/probe_session_store.rb");
    assert!(store.contains("ProbeSessionStore < ActionDispatch::Session::AbstractStore") && !store.contains("not supported"), "{store}");
    assert!(store.contains("ActionDispatch::Session::AbstractStore::DEFAULT_OPTIONS.merge"), "{store}");
    let middleware = source("app/models/probe_session_middleware.rb");
    assert!(middleware.contains("ProbeSessionMiddleware < ActionDispatch::ShowExceptions") && !middleware.contains("not supported"), "{middleware}");
    std::fs::remove_dir_all(dir).unwrap();
}
