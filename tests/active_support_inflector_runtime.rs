// ActiveSupport::Inflector (runtime/spinel/active_support_inflector.rb)
// against the Rails native Core runs (Shopify/rails 5813559f4aed,
// 8.2.0.alpha; tests/support/rails_oracle.rb).
// tests/active_support_inflector_runtime.rb runs Rails' own inflector test
// tables, Core's 004_inflections.rb declarations and essentials' memoizing
// prepend through the pinned activesupport and through the port and
// requires every observation equal.
//
// The port rides only with the PostgreSQL CRuby tree: `check` resolves
// `ActiveSupport::Inflector.underscore`; a tree without the port refuses
// the read by name. The PostgreSQL tree replays the app's
// config/initializers inflections into it at boot, in registration order.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn inflector_answers_like_rails() {
    let out = Command::new("ruby")
        .arg(root().join("tests/active_support_inflector_runtime.rb"))
        .arg("compare")
        .current_dir(root())
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("Native Rails and emitted ActiveSupport::Inflector agree"),
        "inflector diverged from Rails\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    assert!(stdout.lines().count() > 500, "too few observations:\n{stdout}");
    for expected in [
        "default pluralize person: [\"people\", \"people\", \"People\"]",
        "core GraphQLType: [\"GraphQLType\", \"graphqlType\", \"graphql_type\", \"GraphQLTypes\",",
        "core payment_details: [\"PaymentDetails\", \"paymentDetails\", \"payment_details\", \"payment_details\", \"payment_details\",",
        "memoized ancestors: [\"ActiveSupportInflectorMemoization\", \"#<Class:ActiveSupport::Inflector>\"]",
        "memoized camelize: [\"GraphQLType\", false, \"graphqlType\"]",
    ] {
        assert!(stdout.contains(expected), "missing {expected:?}:\n{stdout}");
    }
}

/// essentials' memoizing prepend and the reads Core's code makes.
const MODEL: &str = r##"module ProbeMemo
  def underscore(camel_cased_word)
    super.to_s
  end
end

ActiveSupport::Inflector.singleton_class.prepend(ProbeMemo)

class Probe < ApplicationRecord
  def probe
    [ActiveSupport::Inflector.underscore("APIKey"), ActiveSupport::Inflector.camelize("api_key"), ActiveSupport::Inflector.pluralize("leaf")]
  end

  def rules
    ActiveSupport::Inflector.inflections.acronyms
  end
end
"##;

const INFLECTIONS: &str = r##"ActiveSupport::Inflector.inflections(:en) do |inflect|
  inflect.acronym "API"
  inflect.irregular "leaf", "leaves"
  inflect.uncountable %w( fish sheep )
  inflect.plural "quota", "quotas"
  inflect.irregular "fish", "fishes"
end
"##;

fn probe_app(postgres: bool) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rh_inflector_{}_{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
    let mut files = vec![
        ("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n".to_string()),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n".to_string()),
        ("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table \"probes\" do |t|\n    t.string \"title\"\n  end\nend\n".to_string()),
        ("app/models/probe.rb", MODEL.to_string()),
        ("config/initializers/inflections.rb", INFLECTIONS.to_string()),
        (
            "config/application.rb",
            "require \"rails/all\"\n\nmodule Probe\n  class Application < Rails::Application\n    config.load_defaults 8.1\n  end\nend\n".to_string(),
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
fn inflector_resolves_and_is_refused_where_the_tree_lacks_it() {
    let dir = probe_app(false);
    let (ok, text) = run(&dir, &["check"]);
    assert!(ok, "check refused ActiveSupport::Inflector: {text}");
    assert!(!text.contains("constant not supported") && !text.contains("no known method"), "{text}");
    // The SQLite ruby tree does not carry the port, nor do the strict targets.
    for target in ["ruby", "rust"] {
        let (ok, text) = run(&dir, &["--target", target]);
        assert!(!ok, "{target} accepted ActiveSupport::Inflector: {text}");
        assert!(
            text.contains("runtime_constant") && text.contains("runtime/active_support_inflector.rb"),
            "{target} lost the named refusal: {text}"
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}

/// The PostgreSQL CRuby tree (`project::apply_postgres_bootstrap`) carries
/// the port verbatim, loads it with the gems (before Bundler.require, as
/// `require "rails"` does), replays the app's literal inflections into it
/// in registration order before the app's code, and emits the
/// reads unrefused. (A regex rule is not read at ingest, which reports
/// it, so it is not replayed.)
#[test]
fn the_postgresql_ruby_tree_carries_the_inflector_and_the_app_rules() {
    use roundhouse::project::{target_files, BuildTarget};
    let dir = probe_app(true);
    let mut app = roundhouse::ingest::ingest_app(&dir).expect("ingest probe app");
    roundhouse::session::analyze_and_lower(&mut app);
    assert!(app.database_yml.is_some(), "not a PostgreSQL app: {:?}", app.database_refusals);
    let (files, diagnostics) = roundhouse::emit::diagnostics::scope(|| target_files(&app, &dir, BuildTarget::Ruby));
    let files = files.expect("package the PostgreSQL ruby tree");
    let refused: Vec<String> = diagnostics.iter().map(ToString::to_string).filter(|d| d.contains("Inflector")).collect();
    assert!(refused.is_empty(), "{refused:?}");
    let source = |path: &str| files.iter().find(|(p, _)| p == path).map(|(_, text)| text.as_str())
        .unwrap_or_else(|| panic!("tree lacks {path}"));
    assert_eq!(
        source("runtime/active_support_inflector.rb"),
        std::fs::read_to_string(root().join("runtime/spinel/active_support_inflector.rb")).unwrap()
    );
    // With the gems, as `require "rails"` loads it before Bundler.require:
    // after Module#delegate (Uncountables), ahead of every gem require.
    let facades = source("runtime/gem_facades.rb");
    let delegate = facades.find("require_relative \"module_delegate\"\n").expect("gem_facades loads Module#delegate");
    let required = facades.find("require_relative \"active_support_inflector\"\n").unwrap_or_else(|| panic!("gem_facades requires the port:\n{facades}"));
    assert_eq!(required, delegate + "require_relative \"module_delegate\"\n".len(), "{facades}");
    let boot = source("boot.rb");
    assert!(!boot.contains("runtime/active_support_inflector"), "boot loads the port a second time:\n{boot}");
    let gems = boot.find("require_relative \"runtime/gem_facades\"").expect("boot loads the gems' facades");
    let replay = "# The app's config/initializers inflections, which Rails runs at boot.\n\
                  ActiveSupport::Inflector.inflections(:en) do |inflect|\n  \
                  inflect.acronym(\"API\")\n  \
                  inflect.irregular(\"leaf\", \"leaves\")\n  \
                  inflect.uncountable(\"fish\", \"sheep\")\n  \
                  inflect.plural(\"quota\", \"quotas\")\n  \
                  inflect.irregular(\"fish\", \"fishes\")\n\
                  end\n";
    let replayed = boot.find(replay).unwrap_or_else(|| panic!("boot replays the app's inflections:\n{boot}"));
    // After the gems, immediately before the app's code.
    assert!(gems < replayed, "{boot}");
    assert_eq!(replayed + replay.len(), boot.find("require_relative \"app/models\"").expect("boot loads app/models"), "{boot}");
    let model = source("app/models/probe.rb");
    assert!(model.contains("ActiveSupport::Inflector.underscore(\"APIKey\")") && !model.contains("not supported"), "{model}");
    std::fs::remove_dir_all(dir).unwrap();
}
