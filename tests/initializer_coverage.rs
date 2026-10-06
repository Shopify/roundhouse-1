//! Initializer coverage (`ingest::initializers`,
//! `analyze::initializer_coverage`): every top-level statement of a
//! `config/initializers/` file — the app's and each in-repo engine's —
//! that no ingest reader applies is an `initializer_unsupported` error
//! at its file and line, in Rails' initializer load order. A statement a
//! reader consumed is not reported; a container a reader descended into
//! (a `Kaminari.configure` block) reports only its unconsumed children.
//!
//! The fixture is built on disk and ingested through `ingest_app`, the
//! path the `check` binary takes.

use std::path::{Path, PathBuf};

use roundhouse::analyze::{Analyzer, diagnose};
use roundhouse::diagnostic::Severity;
use roundhouse::ingest::ingest_app;

const SCHEMA: &str = r#"ActiveRecord::Schema.define do
  create_table "notes", force: :cascade do |t|
    t.string "body"
  end
end
"#;

const APPLICATION: &str = r#"require_relative "boot"

module Shop
  class Application < Rails::Application
    config.load_defaults 7.1
  end
end

Rails.load_embedded_engines
"#;

/// Four engines. `contexts.yml` names billing then audit; billing's
/// entry file requires ledger first, so ledger's engine class is
/// defined first. zeta is a path-gem engine nothing requires: the
/// unmeasured fallback, after every reached one. Directory order would
/// be audit, billing, ledger, zeta.
const CONTEXTS: &str = "all:\n  - billing\n  - audit\nessentials:\n  - ledger\n";

fn lockfile() -> String {
    let mut out = String::new();
    for gem in ["audit", "billing", "ledger", "zeta"] {
        out.push_str(&format!("PATH\n  remote: components/{gem}\n  specs:\n    {gem} (1.0)\n\n"));
    }
    out.push_str("GEM\n  remote: https://rubygems.org/\n  specs:\n\nDEPENDENCIES\n");
    for gem in ["audit", "billing", "ledger", "zeta"] {
        out.push_str(&format!("  {gem}!\n"));
    }
    out
}

fn engine(module: &str) -> String {
    format!("module {module}\n  class Engine < ::Rails::Engine\n  end\nend\n")
}

fn fixture() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir()
        .join(format!("roundhouse-initializer-coverage-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let lock = lockfile();
    let files: Vec<(&str, String)> = vec![
        ("Gemfile.lock", lock),
        ("db/schema.rb", SCHEMA.to_string()),
        (
            "app/models/application_record.rb",
            "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n".to_string(),
        ),
        ("app/models/note.rb", "class Note < ApplicationRecord\nend\n".to_string()),
        ("config/application.rb", APPLICATION.to_string()),
        ("config/contexts.yml", CONTEXTS.to_string()),
        // Engines.
        ("components/billing/lib/billing.rb", "require \"ledger\"\nrequire \"billing/engine\"\n".to_string()),
        ("components/billing/lib/billing/engine.rb", engine("Billing")),
        ("components/billing/config/initializers/a.rb", "Billing.strict_ledger = true\n".to_string()),
        ("components/ledger/lib/ledger.rb", "require_relative \"ledger/engine\"\n".to_string()),
        ("components/ledger/lib/ledger/engine.rb", engine("Ledger")),
        (
            "components/ledger/config/initializers/a.rb",
            "Ledger.precision = 4\nRails.application.config.filter_parameters += [:ledger_key]\n".to_string(),
        ),
        ("components/audit/lib/audit.rb", engine("Audit")),
        ("components/audit/config/initializers/a.rb", "Audit.enable!\n".to_string()),
        ("components/zeta/lib/zeta/engine.rb", engine("Zeta")),
        ("components/zeta/config/initializers/a.rb", "Zeta.boot\n".to_string()),
        // Root initializers.
        (
            "config/initializers/assets.rb",
            "Rails.application.config.assets.version = \"1.0\"\n\
             Rails.application.config.filter_parameters += [:password]\n\
             Rails.application.config.filter_parameters += FILTERS\n\
             extra = [:pin_code]\n\
             Rails.application.config.filter_parameters += extra\n"
                .to_string(),
        ),
        // The block parameter of `Rails.application.configure` is the
        // application (railties `Railtie#configure` instance_evals with
        // the receiver as the argument): `app.config` is the config, and
        // the other two lines raise NoMethodError in Rails 8.1.4.
        (
            "config/initializers/configure.rb",
            "Rails.application.configure do |app|\n  app.config.filter_parameters << :token\n  app.filter_parameters << :pin\n  app.default_per_page = 50\nend\n"
                .to_string(),
        ),
        (
            "config/initializers/kaminari.rb",
            "Kaminari.configure do |config|\n  config.default_per_page = 20\n  config.max_per_page = 100\nend\n"
                .to_string(),
        ),
        ("config/initializers/mixins.rb", "ActiveRecord::Base.include Auditable\n".to_string()),
        (
            "config/initializers/on_load.rb",
            "# frozen_string_literal: true\n\nActiveSupport.on_load(:active_record) do\n  self.belongs_to_required_by_default = false\nend\n"
                .to_string(),
        ),
        ("config/initializers/requires.rb", "require \"foo\"\n".to_string()),
        (
            "config/initializers/session_store.rb",
            "Rails.application.config.session_store :cookie_store, key: \"_shop_session\"\n".to_string(),
        ),
        ("config/initializers/sub/deep.rb", "Deep.wire(:x)\n".to_string()),
        ("config/initializers/time_formats.rb", "Time::DATE_FORMATS[:short_day] = \"%a %d\"\n".to_string()),
    ];
    for (path, body) in files {
        let p = dir.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    dir
}

fn relative<'a>(root: &Path, path: &'a str) -> &'a str {
    let root = root.display().to_string();
    path.strip_prefix(&root).unwrap_or(path).trim_start_matches('/')
}

#[test]
fn unconsumed_initializer_statements_are_errors_in_load_order() {
    let dir = fixture();
    let root = dir.canonicalize().unwrap();
    let mut app = ingest_app(&dir).expect("ingest");

    // Engines in class-definition order (the require walk), the
    // unreached one last, then the app; each railtie's files sorted by
    // full path, subdirectories included.
    let files: Vec<&str> = app.initializer_files.iter().map(|f| relative(&root, f)).collect();
    assert_eq!(
        files,
        [
            "components/ledger/config/initializers/a.rb",
            "components/billing/config/initializers/a.rb",
            "components/audit/config/initializers/a.rb",
            "components/zeta/config/initializers/a.rb",
            "config/initializers/assets.rb",
            "config/initializers/configure.rb",
            "config/initializers/kaminari.rb",
            "config/initializers/mixins.rb",
            "config/initializers/on_load.rb",
            "config/initializers/requires.rb",
            "config/initializers/session_store.rb",
            "config/initializers/sub/deep.rb",
            "config/initializers/time_formats.rb",
        ]
    );

    Analyzer::new(&app).analyze(&mut app);
    let gaps: Vec<(String, u32, String, String)> = diagnose(&app)
        .into_iter()
        .filter(|d| d.code() == "initializer_unsupported")
        .map(|d| {
            assert_eq!(d.severity, Severity::Error);
            let source = &app.sources[d.span.file.0 as usize - 1];
            let (line, _) = source.line_col(d.span.start);
            let roundhouse::diagnostic::DiagnosticKind::InitializerUnsupported { construct, detail } =
                &d.kind
            else {
                unreachable!()
            };
            (
                relative(&root, &source.path).to_string(),
                line,
                construct.as_str().to_string(),
                detail.clone(),
            )
        })
        .collect();
    let expected: Vec<(String, u32, String, String)> = [
        ("components/ledger/config/initializers/a.rb", 1, "Ledger.precision=", "Ledger.precision = 4"),
        ("components/billing/config/initializers/a.rb", 1, "Billing.strict_ledger=", "Billing.strict_ledger = true"),
        ("components/audit/config/initializers/a.rb", 1, "Audit.enable!", "Audit.enable!"),
        ("components/zeta/config/initializers/a.rb", 1, "Zeta.boot", "Zeta.boot"),
        // A local of the initializer does not exist in the method the
        // operand moves into; the local's own assignment is a gap too.
        ("config/initializers/assets.rb", 4, "local assignment", "extra = [:pin_code]"),
        (
            "config/initializers/assets.rb",
            5,
            "Rails.application.config.filter_parameters+=",
            "filter_parameters operand reads `extra`, a local of the initializer",
        ),
        (
            "config/initializers/configure.rb",
            3,
            "app.filter_parameters.<<",
            "`app.filter_parameters` is private on the application: NoMethodError in Rails 8.1.4",
        ),
        (
            "config/initializers/configure.rb",
            4,
            "app.default_per_page=",
            "`app` is Rails.application here, not its config: this calls `Rails.application.default_per_page=`",
        ),
        ("config/initializers/kaminari.rb", 3, "config.max_per_page=", "config.max_per_page = 100"),
        (
            "config/initializers/on_load.rb",
            3,
            "ActiveSupport.on_load",
            "ActiveSupport.on_load(:active_record) do",
        ),
        ("config/initializers/requires.rb", 1, "require", "require \"foo\""),
        ("config/initializers/sub/deep.rb", 1, "Deep.wire", "Deep.wire(:x)"),
    ]
    .into_iter()
    .map(|(f, l, c, d)| (f.to_string(), l, c.to_string(), d.to_string()))
    .collect();
    assert_eq!(gaps, expected);

    // `+= FILTERS` is applied as app code: the constant it names is
    // diagnosed like any app code's, not as an initializer gap.
    let unresolved: Vec<String> = diagnose(&app)
        .iter()
        .filter(|d| d.severity == Severity::Error && d.code() != "initializer_unsupported")
        .map(|d| d.render(&app.sources))
        .collect();
    assert!(
        unresolved
            .iter()
            .any(|r| r.contains("config/initializers/assets.rb:3:") && r.contains("FILTERS")),
        "{unresolved:#?}"
    );

    // The rendered form: file:line:col, code, construct and first line.
    let rendered: Vec<String> = diagnose(&app)
        .iter()
        .filter(|d| d.code() == "initializer_unsupported")
        .map(|d| d.render(&app.sources))
        .collect();
    let requires = rendered
        .iter()
        .find(|r| r.contains("requires.rb"))
        .expect("the require is reported");
    assert!(
        requires.ends_with(
            "config/initializers/requires.rb:1:1: error[initializer_unsupported]: \
             initializer statement `require` does not run in the emitted app: require \"foo\""
        ),
        "{requires}"
    );

    // What the readers consumed still reached the IR.
    assert!(app.time_formats.keys().any(|k| k.as_str() == "short_day"));
    assert!(app.module_mixins.iter().any(|m| m.module.as_str() == "Auditable"));
    // The filter_parameters writes that apply, in boot order: the ledger
    // engine's, then the app's; the refused `+= extra` and
    // `app.filter_parameters << :pin` add nothing.
    let reopen = app.rails_application.as_ref().expect("the Application reopen");
    let filters = reopen
        .methods
        .iter()
        .find(|m| m.name.as_str() == "filter_parameters")
        .expect("filter_parameters is synthesized");
    let body = format!("{:?}", filters.body);
    let order: Vec<usize> = ["ledger_key", "password", "token"]
        .iter()
        .map(|k| body.find(k).unwrap_or_else(|| panic!("{k} missing from {body}")))
        .collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "boot order lost: {body}");
    assert!(!body.contains("pin"), "{body}");
    std::fs::remove_dir_all(dir).unwrap();
}
