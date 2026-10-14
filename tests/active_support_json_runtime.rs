// core_ext/object/json.rb's `as_json` table (runtime/spinel/
// active_support_json.rb's `Encoding.as_json`) against the Rails native
// Core runs (Shopify/rails 5813559f4aed, 8.2.0.alpha; tests/support/
// rails_oracle.rb). tests/active_support_json_runtime.rb asks the same
// values natively and through the port and requires every observation
// equal.
//
// The analyzer flags an `as_json` no static method answers
// (`expr::OBJECT_AS_JSON`) and `lower::as_json_residue` writes it as
// `ActiveSupport::JSON::Encoding.as_json(value, options)`. The PostgreSQL
// CRuby tree carries the port wherever emitted app code names it; a tree
// without it refuses the read by name. `errors.as_json` on a record
// answers ActiveModel::Errors, which the Ruby trees port over what each
// error push records (runtime/spinel/active_model_errors.rb) and every
// other tree refuses (tests/errors_hash_port.rs). A
// record's `as_json(options)` other than `only:` answers
// ActiveModel::Serializers::JSON, which only the PostgreSQL CRuby tree's
// ActiveRecord::Base includes; every other tree refuses it by name.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn as_json_answers_like_rails() {
    let out = Command::new("ruby")
        .arg(root().join("tests/active_support_json_runtime.rb"))
        .arg("compare")
        .current_dir(root())
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("Native Rails and emitted ActiveSupport::JSON as_json agree"),
        "as_json diverged from Rails\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    assert!(stdout.lines().count() > 140, "too few observations:\n{stdout}");
    for expected in [
        "bigdecimal: \"1234.5678\" (US-ASCII)",
        "float nan: nil (NilClass)",
        "hash only nested: {\"a\"=>{\"a\"=>1 (Integer)}}",
        "instance values: {\"name\"=>\"acme\" (US-ASCII), \"at\"=>\"2024-02-29\" (UTF-8),",
        "time offset p0: \"2024-12-31T23:59:58-05:30\" (US-ASCII)",
        "time utc p6: \"2024-05-06T07:08:09.123456Z\" (US-ASCII)",
        "legacy time utc: \"2024/05/06 07:08:09 +0000\" (UTF-8)",
        "ipv4 net: \"10.0.0.0/8\" (UTF-8)",
        "process status: {:exitstatus=>3 (Integer), :pid=>true (TrueClass)}",
    ] {
        assert!(stdout.contains(expected), "missing {expected:?}:\n{stdout}");
    }
}

/// Core's shapes: a value object's `as_json` on its parts, a Hash's
/// `as_json(except:)`, a PORO's bare `as_json` (instance values).
const SNAPSHOT: &str = r##"class Snapshot
  def initialize(title, ratio)
    @title = title
    @ratio = ratio
  end

  def payload
    {
      title: @title.as_json,
      ratio: @ratio.as_json,
      name: :snap.as_json,
      count: 3.as_json,
      tags: ["a", :b].as_json,
      nested: { a: 1, b: 2 }.as_json(except: :b),
      me: as_json,
    }
  end
end
"##;

/// ActiveModel's surface, which only the Ruby trees' ports answer.
const REFUSED: &str = r##"class Probe < ApplicationRecord
  def errors_json
    errors.as_json
  end

  def errors_json_full
    errors.as_json(full_messages: true)
  end
end

module ProbeSerializing
  def serialized
    as_json(except: [:title])
  end
end
"##;

fn probe_app(postgres: bool, refused: bool) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rh_asjson_{}_{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
    let mut files = vec![
        ("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n".to_string()),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n".to_string()),
        ("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table \"probes\" do |t|\n    t.string \"title\"\n  end\nend\n".to_string()),
        ("app/models/snapshot.rb", SNAPSHOT.to_string()),
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
    if refused {
        files.push(("app/models/probe.rb", REFUSED.to_string()));
    }
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
fn as_json_routes_and_is_refused_where_the_tree_lacks_the_port() {
    let dir = probe_app(false, false);
    let (ok, text) = run(&dir, &["check"]);
    assert!(ok, "check refused as_json: {text}");
    assert!(!text.contains("as_json"), "{text}");
    // The strict targets do not carry the port.
    let (ok, text) = run(&dir, &["--target", "rust"]);
    assert!(!ok, "rust accepted ActiveSupport::JSON::Encoding: {text}");
    assert!(
        text.contains("runtime_constant") && text.contains("ActiveSupport::JSON::Encoding.as_json")
            && text.contains("runtime/active_support_json.rb"),
        "rust lost the named refusal: {text}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn active_model_as_json_is_gated_to_its_ports() {
    let dir = probe_app(false, true);
    let (ok, text) = run(&dir, &["check"]);
    assert!(ok, "check refused ActiveModel's as_json: {text}");
    // A module's `as_json(except:)` may be a record's: the SQLite tree's
    // Base has no Serializers::JSON, so its emit refuses it by name. The
    // record's `errors.as_json` is ActiveModel::Errors', which the Ruby
    // trees answer from the attributes their pushes record.
    let (ok, text) = run(&dir, &["--target", "ruby", "--allow-unsupported"]);
    assert!(ok, "{text}");
    assert!(
        text.lines().any(|l| l.contains("runtime_constant") && l.contains("`ActiveRecord::Base#as_json(options)` answers ActiveModel::Serializers::JSON")
            && l.contains("runtime/active_model_serialization.rb")),
        "ruby lost the named refusal: {text}"
    );
    assert!(!text.contains("ActiveModel::Errors"), "{text}");
    let probe = std::fs::read_to_string(dir.join("emitted-ruby/app/models/probe.rb")).unwrap();
    // The plain read is answered from the record's `error_details` on every
    // tree (`lower::errors_index`); `as_json(options)` needs the port.
    assert!(probe.contains("ActiveSupport.errors_to_hash(error_details)"), "{probe}");
    assert!(probe.contains("ActiveModel::ErrorAttributes.as_json(errors, full_messages: true)"), "{probe}");
    // The strict targets carry neither port.
    let (ok, text) = run(&dir, &["--target", "rust"]);
    assert!(!ok, "rust accepted ActiveModel::Errors#as_json: {text}");
    assert!(
        text.lines().any(|l| l.contains("ActiveModel::Errors#as_json")
            && l.contains("declared by the Ruby-family runtime (runtime/active_model_errors.rb), which rust does not emit")),
        "rust lost the named refusal: {text}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

/// The PostgreSQL CRuby tree carries the port verbatim, requires it from
/// active_support_ext.rb, and emits each routed send as the port's call.
#[test]
fn the_postgresql_ruby_tree_carries_the_json_port() {
    use roundhouse::project::{target_files, BuildTarget};
    let dir = probe_app(true, false);
    let mut app = roundhouse::ingest::ingest_app(&dir).expect("ingest probe app");
    roundhouse::session::analyze_and_lower(&mut app);
    assert!(app.database_yml.is_some(), "not a PostgreSQL app: {:?}", app.database_refusals);
    let (files, diagnostics) = roundhouse::emit::diagnostics::scope(|| target_files(&app, &dir, BuildTarget::Ruby));
    let files = files.expect("package the PostgreSQL ruby tree");
    let refused: Vec<String> = diagnostics.iter().map(ToString::to_string).filter(|d| d.contains("as_json") || d.contains("JSON")).collect();
    assert!(refused.is_empty(), "{refused:?}");
    let source = |path: &str| files.iter().find(|(p, _)| p == path).map(|(_, text)| text.as_str())
        .unwrap_or_else(|| panic!("tree lacks {path}: {:?}", files.iter().map(|(p, _)| p).collect::<Vec<_>>()));
    // Verbatim, but for the stdlib require the tree adds for what it names:
    // written below the leading magic comments (4c3b8b3d9), because Ruby
    // reads `# frozen_string_literal: true` only before the first token.
    let port = source("runtime/active_support_json.rb");
    let original = std::fs::read_to_string(root().join("runtime/spinel/active_support_json.rb")).unwrap();
    assert!(original.starts_with("# frozen_string_literal: true\n"), "the port's magic comment moved");
    assert_eq!(port.replacen("require \"uri\"\n", "", 1), original, "{port}");
    let ext = source("runtime/active_support_ext.rb");
    assert_eq!(ext.matches("require_relative \"active_support_json\"").count(), 1, "{ext}");
    let snapshot = source("app/models/snapshot.rb");
    for call in [
        "ActiveSupport::JSON::Encoding.as_json(@title)",
        "ActiveSupport::JSON::Encoding.as_json(@ratio)",
        "ActiveSupport::JSON::Encoding.as_json(:snap)",
        "ActiveSupport::JSON::Encoding.as_json(self)",
    ] {
        assert!(snapshot.contains(call), "missing {call}: {snapshot}");
    }
    assert!(snapshot.contains("ActiveSupport::JSON::Encoding.as_json([\"a\", :b])"), "{snapshot}");
    assert!(snapshot.contains("ActiveSupport::JSON::Encoding.as_json({ a: 1, b: 2 }, except: :b)"), "{snapshot}");
    assert_eq!(snapshot.matches(".as_json").count(), snapshot.matches("Encoding.as_json(").count(), "{snapshot}");
    assert!(!snapshot.contains("not supported"), "{snapshot}");
    std::fs::remove_dir_all(dir).unwrap();
}
