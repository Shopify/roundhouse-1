//! `class Current < ActiveSupport::CurrentAttributes` becomes a plain,
//! statically-resolvable class (`ingest::current_attributes`).
//!
//! Rails stacks three pieces of metaprogramming here — `attribute`
//! defining accessors into a generated module, class-level `Current.user`
//! arriving via `method_missing`, and an app's own writer reaching the
//! generated one through `super`. An emitted tree has none of it, and a
//! statically-resolved target cannot follow `method_missing` at all.

use roundhouse::det::HashMap;
use std::path::PathBuf;
use std::process::Command;

use roundhouse::emit::ruby;
use roundhouse::ingest::ingest_app_from_tree;

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

fn tree(files: &[(&str, &str)]) -> HashMap<PathBuf, Vec<u8>> {
    files
        .iter()
        .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
        .collect()
}

fn current() -> String {
    let app = ingest_app_from_tree(tree(&[
        (
            "db/schema.rb",
            "ActiveRecord::Schema.define do\n  create_table \"accounts\" do |t|\n    t.string \"name\"\n  end\nend\n",
        ),
        ("app/models/account.rb", "class Account < ApplicationRecord\nend\n"),
        (
            "app/models/current.rb",
            r#"class Current < ActiveSupport::CurrentAttributes
  attribute :session, :user, :request

  delegate :host, :protocol, to: :request, prefix: true, allow_nil: true

  def session=(value)
    super(value)

    if value.present?
      self.user = session.user
    end
  end

  def account
    Account.first
  end
end
"#,
        ),
    ]))
    .expect("ingest");
    ruby::emit_library(&app)
        .iter()
        .find(|f| f.path.to_string_lossy().ends_with("current.rb"))
        .map(|f| f.content.clone())
        .expect("current.rb")
}

/// The base is gone — nothing in an emitted tree provides it.
#[test]
fn the_current_attributes_base_is_dropped() {
    let c = current();
    assert!(!c.contains("ActiveSupport::CurrentAttributes"), "{c}");
    assert!(c.contains("class Current\n"), "{c}");
}

/// `attribute :session, :user, :request` becomes real ivar accessors.
#[test]
fn declared_attributes_become_accessors() {
    let c = current();
    for m in ["def user\n    @user", "def user=(value)\n    @user = value", "def request\n    @request"] {
        assert!(c.contains(m), "missing {m:?}:\n{c}");
    }
}

/// The app writes its OWN `session=`, so only the reader is synthesized —
/// and its `super(value)` becomes the storage write it means, rather than
/// needing a module for `super` to find.
#[test]
fn an_app_written_writer_keeps_its_body_and_loses_super() {
    let c = current();
    assert!(!c.contains("super("), "no super survives:\n{c}");
    assert!(
        c.contains("def session=(value)\n    @session = value"),
        "super became the storage write:\n{c}"
    );
    assert_eq!(c.matches("def session=").count(), 1, "not double-defined:\n{c}");
}

/// `delegate … prefix: true, allow_nil: true` — the nil guard is the
/// whole point of `allow_nil`.
#[test]
fn delegate_expands_with_its_nil_guard() {
    let c = current();
    assert!(c.contains("def request_host"), "prefixed name:\n{c}");
    assert!(c.contains("@request.nil?"), "allow_nil guard:\n{c}");
}

/// The surface app code actually calls. Rails supplies these through
/// `method_missing`; a statically-resolved target needs them real —
/// including for a method the app defined itself (`account`).
#[test]
fn class_level_forwarders_exist_for_every_instance_method() {
    let c = current();
    for m in [
        "def self.user\n    Current.instance.user",
        "def self.user=(value)\n    Current.instance.user = value",
        "def self.account\n    Current.instance.account",
        "def self.request_host",
        "def self.instance",
        "def self.reset",
    ] {
        assert!(c.contains(m), "missing {m:?}:\n{c}");
    }
}

/// `attribute :session, :user` on CurrentAttributes is the class-body
/// form apps actually call (`Current.session =`, `Current.user`). The
/// app-written writer still reaches storage through rewritten `super`.
#[test]
fn emitted_current_attributes_session_and_user_run() {
    emit_and_run::real_blog()
        .write(
            "app/models/current.rb",
            r#"class Current < ActiveSupport::CurrentAttributes
  attribute :session, :user

  def session=(value)
    super(value)

    if value.present?
      self.user = session.user
    end
  end
end
"#,
        )
        .run_ruby(
            r#"
session = Object.new
def session.user
  "ada"
end
Current.session = session
raise "user from session writer" unless Current.user == "ada"
raise "session stored" unless Current.session.equal?(session)
Current.reset
raise "reset user" unless Current.user.nil?
puts "current attributes passed"
"#,
        )
        .assert_passes();
}

const RESETTING_CURRENT: &str = r#"class Current < ActiveSupport::CurrentAttributes
  attribute :a, :b

  before_reset { $log << [:before1, a] }
  resets do
    $log << [:after1, a]
    self.b = "B"
  end
  after_reset { $log << [:after2, b] }
  before_reset :note

  def b
    super || "dflt"
  end

  private

  def note
    $log << [:note, a]
  end
end
"#;

const RESETTING_PROBE: &str = r#"
$log = []
p Current.b
Current.a = 1
Current.reset
p $log
p Current.a
p Current.b
Current.b = "set"
p Current.b
"#;

/// Reset callbacks (`before_reset`, `resets`, `after_reset`, a block or a
/// method name) and a reader reaching the storage through `super`, run
/// as activesupport 8.2 runs them: before callbacks in order on the
/// instance being reset, after ones in reverse on the reset instance.
/// api_versioning 4.6.0's `ThreadLocalVersionStorage` `resets` its
/// `requested_handle` and reads it through `super ||`.
#[test]
fn reset_callbacks_and_reader_super_run_like_activesupport() {
    let oracle = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/rails_oracle");
    let script = std::env::temp_dir().join(format!("rh_current_reset_native_{}.rb", std::process::id()));
    std::fs::write(
        &script,
        format!(
            "require {:?}\nRailsOracle.activate(\"activesupport\")\nrequire \"active_support\"\n\
             require \"active_support/current_attributes\"\n{RESETTING_CURRENT}\n{RESETTING_PROBE}",
            oracle.display().to_string()
        ),
    )
    .unwrap();
    let native = Command::new("ruby").arg(&script).output().expect("ruby");
    std::fs::remove_file(&script).unwrap();
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let native = String::from_utf8_lossy(&native.stdout).into_owned();
    assert!(native.contains("[[:before1, 1], [:note, 1], [:after2, \"dflt\"], [:after1, nil]]"), "{native}");

    let run = emit_and_run::real_blog().write("app/models/current.rb", RESETTING_CURRENT).run_ruby(RESETTING_PROBE);
    run.assert_passes();
    assert_eq!(native, run.stdout);
}
