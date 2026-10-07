// `Rails.backtrace_cleaner` (runtime/spinel/active_support_backtrace_cleaner.rb)
// against the Rails native Core runs (Shopify/rails 5813559f4aed,
// 8.2.0.alpha; tests/support/rails_oracle.rb). tests/backtrace_cleaner_runtime.rb
// runs the same backtraces through a booted Rails app's cleaner and through
// the port, from the same app root, and requires every observation equal.
//
// The port rides only with the PostgreSQL CRuby tree. Analysis types
// `Rails.backtrace_cleaner` as Rails::BacktraceCleaner, so Core's
// `Rails.backtrace_cleaner.dup` + `add_silencer` (QueryErrorLogSubscriber)
// resolves in `check`; a tree without the port refuses the read by name.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn backtrace_cleaner_answers_like_rails() {
    let out = Command::new("ruby")
        .arg(root().join("tests/backtrace_cleaner_runtime.rb"))
        .arg("compare")
        .current_dir(root())
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("Native Rails and emitted Rails.backtrace_cleaner agree"),
        "backtrace cleaner diverged from Rails\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    // Not a vacuous agreement: both sides observed the whole script, and
    // what Rails answered is the root-relative, components-only trace
    // Core's QueryErrorLogSubscriber logs.
    assert!(stdout.lines().count() > 150, "too few observations:\n{stdout}");
    for expected in [
        "subscriber message: \"components/platform/essentials/lib/development_support/query_error_log_subscriber.rb:24:in 'QueryErrorLogSubscriber#sql'\\n",
        "clean_frame 15 :noise: \"activerecord (8.1.4) lib/active_record/base.rb:1:in 'ActiveRecord::Base.x'\"",
        "clean_frame 8: nil",
        "clean_frame 10: \"app/views/shops/show.html.erb:3\"",
        "clean locations: [\"components/shop/lib/frames.rb:3:in 'Frames.locations'\"]",
        "original unchanged: true",
    ] {
        assert!(stdout.contains(expected), "missing {expected:?}:\n{stdout}");
    }
}

/// QueryErrorLogSubscriber's and query_debug.rb's uses, in a probe app.
const BODY: &str = r#"cleaner = Rails.backtrace_cleaner.dup
    cleaner.add_silencer { |line| !(line =~ /^components/) }
    Rails.backtrace_cleaner.remove_silencers!
    Rails.backtrace_cleaner.add_silencer { |line| !(line =~ %r{^(components)/}) }
    [cleaner.clean(caller).first(10).join("\n"), Rails.backtrace_cleaner.clean(caller).take(10).join("\n")]"#;

fn probe_app(postgres: bool) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rh_backtrace_cleaner_{}_{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
    let mut files = vec![
        ("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n".to_string()),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n".to_string()),
        ("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table \"probes\" do |t|\n    t.string \"title\"\n  end\nend\n".to_string()),
        ("app/models/probe.rb", format!("class Probe < ApplicationRecord\n  def probe\n    {BODY}\n  end\nend\n")),
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
fn rails_backtrace_cleaner_resolves_and_is_refused_where_the_tree_lacks_it() {
    let dir = probe_app(false);
    let (ok, text) = run(&dir, &["check"]);
    assert!(ok, "check refused Rails.backtrace_cleaner: {text}");
    assert!(!text.contains("backtrace_cleaner lacks"), "{text}");
    // The SQLite ruby tree does not carry the port, nor do the strict targets.
    for target in ["ruby", "rust"] {
        let (ok, text) = run(&dir, &["--target", target]);
        assert!(!ok, "{target} accepted Rails.backtrace_cleaner: {text}");
        assert!(
            text.contains("runtime_constant") && text.contains("`Rails.backtrace_cleaner` answers Rails::BacktraceCleaner"),
            "{target} lost the named refusal: {text}"
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}

/// The PostgreSQL CRuby tree (`project::apply_postgres_bootstrap`) carries
/// the port verbatim, loads it at boot, and emits the reads unrefused. The
/// CLI admits a PostgreSQL primary only through a database manifest, so
/// this packages the tree through the library, as tests/db_pg_runtime.rs does.
#[test]
fn the_postgresql_ruby_tree_carries_the_port_and_emits_its_reads() {
    use roundhouse::project::{target_files, BuildTarget};
    let dir = probe_app(true);
    let mut app = roundhouse::ingest::ingest_app(&dir).expect("ingest probe app");
    roundhouse::session::analyze_and_lower(&mut app);
    assert!(app.database_yml.is_some(), "not a PostgreSQL app: {:?}", app.database_refusals);
    let (files, diagnostics) = roundhouse::emit::diagnostics::scope(|| target_files(&app, &dir, BuildTarget::Ruby));
    let files = files.expect("package the PostgreSQL ruby tree");
    let refused: Vec<String> = diagnostics.iter().map(ToString::to_string)
        .filter(|d| d.contains("backtrace_cleaner") || d.contains("BacktraceCleaner")).collect();
    assert!(refused.is_empty(), "{refused:?}");
    let source = |path: &str| files.iter().find(|(p, _)| p == path).map(|(_, text)| text.as_str())
        .unwrap_or_else(|| panic!("tree lacks {path}"));
    assert_eq!(
        source("runtime/active_support_backtrace_cleaner.rb"),
        std::fs::read_to_string(root().join("runtime/spinel/active_support_backtrace_cleaner.rb")).unwrap()
    );
    // Loaded before the app's code, as `require "rails"` loads it.
    let boot = source("boot.rb");
    let required = boot.find("require_relative \"runtime/active_support_backtrace_cleaner\"").expect("boot requires the port");
    assert!(required < boot.find("require_relative \"app/models\"").expect("boot loads app/models"), "{boot}");
    let model = source("app/models/probe.rb");
    assert!(model.contains("Rails.backtrace_cleaner.dup") && !model.contains("not supported"), "{model}");
    std::fs::remove_dir_all(dir).unwrap();

    // The same packaging of the SQLite app, whose tree lacks the port,
    // refuses the read: the survey above saw it.
    let dir = probe_app(false);
    let mut app = roundhouse::ingest::ingest_app(&dir).expect("ingest probe app");
    roundhouse::session::analyze_and_lower(&mut app);
    let (_, diagnostics) = roundhouse::emit::diagnostics::scope(|| target_files(&app, &dir, BuildTarget::Ruby));
    assert!(
        diagnostics.iter().map(ToString::to_string)
            .any(|d| d.contains("`Rails.backtrace_cleaner` answers Rails::BacktraceCleaner")),
        "{diagnostics:?}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

/// Core's components/platform/essentials/config/initializers/backtrace_silencers.rb
/// (verbatim in tests/support/core_backtrace_silencers.rb): every statement
/// is an initializer program statement (ingest::initializer_programs), the
/// boot registers Active Record's railtie hook before firing `:active_record`
/// for the programs, and tests/backtrace_silencers_runtime.rb finds the
/// natively booted app and the emitted tree's boot leaving the same
/// `Rails.backtrace_cleaner`, `ActiveRecord::LogSubscriber.backtrace_cleaner`
/// and QueryErrorLogSubscriber trace. The emitted boot loads the `pg` gem
/// (it never connects), from the oracle's pinned gem home.
#[test]
fn cores_backtrace_silencers_initializer_runs_like_rails() {
    use roundhouse::project::{target_files, BuildTarget};
    const FILE: &str = "config/initializers/backtrace_silencers.rb";
    let dir = probe_app(true);
    let initializer = std::fs::read_to_string(root().join("tests/support/core_backtrace_silencers.rb")).unwrap();
    std::fs::create_dir_all(dir.join("config/initializers")).unwrap();
    std::fs::write(dir.join(FILE), &initializer).unwrap();

    let mut app = roundhouse::ingest::ingest_app(&dir).expect("ingest probe app");
    let programs: Vec<(&str, usize)> = app.initializer_programs.iter().map(|p| (p.file.as_str(), p.statements)).collect();
    assert_eq!(programs, [(FILE, 15)], "{:?}", app.initializer_programs);
    let gaps: Vec<_> = app.initializer_gaps.iter().map(|g| (&g.construct, &g.detail)).collect();
    assert!(gaps.is_empty(), "{gaps:?}");
    roundhouse::session::analyze_and_lower(&mut app);
    let (files, diagnostics) = roundhouse::emit::diagnostics::scope(|| target_files(&app, &dir, BuildTarget::Ruby));
    let files = files.expect("package the PostgreSQL ruby tree");
    let refused: Vec<String> = diagnostics.iter().map(ToString::to_string)
        .filter(|d| d.contains("backtrace_silencers") || d.contains("LogSubscriber") || d.contains("BacktraceCleaner")).collect();
    assert!(refused.is_empty(), "{refused:?}");
    let source = |path: &str| files.iter().find(|(p, _)| p == path).map(|(_, text)| text.as_str())
        .unwrap_or_else(|| panic!("tree lacks {path}"));
    assert_eq!(
        source("runtime/active_record_log_subscriber.rb"),
        std::fs::read_to_string(root().join("runtime/spinel/active_record_log_subscriber.rb")).unwrap()
    );
    let boot = source("boot.rb");
    let at = |needle: &str| boot.find(needle).unwrap_or_else(|| panic!("{needle} missing:\n{boot}"));
    let cleaner = at("require_relative \"runtime/active_support_backtrace_cleaner\"");
    let railtie_hook = at("require_relative \"runtime/active_record_log_subscriber\"");
    let fire = at("ActiveSupport.run_load_hooks(:active_record, ActiveRecord::Base)");
    let program = at("RoundhouseInitializerConfigInitializersBacktraceSilencers.run_initializer\n");
    assert!(cleaner < railtie_hook && railtie_hook < fire && fire < program, "{boot}");

    let tree = dir.join("emitted");
    for (path, text) in &files {
        let file = tree.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let out = Command::new("ruby")
        .arg(root().join("tests/backtrace_silencers_runtime.rb"))
        .arg("compare")
        .arg(&dir)
        .arg(&tree)
        .current_dir(root())
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("Native Rails and the emitted tree agree on Core's backtrace_silencers.rb"),
        "the initializer's cleaners diverged from Rails\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    // Not a vacuous agreement: the initializer ran (Rails' default
    // silencer is gone, Core's are there) and the Active Record block ran
    // on a copy that also skips lib/patches/active_record.
    for expected in [
        "Rails cleaner silencers, filters: [7, 3]",
        "LogSubscriber cleaner is a copy: true",
        "LogSubscriber cleaner silencers, filters: [8, 3]",
        "Rails clean_frame 8: \"components/platform/essentials/lib/patches/active_record/strict_loading.rb:12:in 'Patches::ActiveRecord::StrictLoading#load_target'\"",
        "LogSubscriber clean_frame 8: nil",
        "Rails clean_frame 4: nil",
        "subscriber message: \"components/platform/essentials/lib/development_support/query_error_log_subscriber.rb:24:in 'QueryErrorLogSubscriber#sql'\\n",
    ] {
        assert!(stdout.contains(expected), "missing {expected:?}:\n{stdout}");
    }
    println!("{stdout}");
    std::fs::remove_dir_all(dir).unwrap();
}
