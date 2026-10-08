//! A Concern's `included do attr_* end` on a plain (non-model) includer.
//!
//! `ActiveSupport::Concern#append_features` runs the block with
//! `base.class_eval`, so each literal accessor is defined in the includer
//! itself, public whatever the includer's default visibility. A Concern
//! that includes the Concern defers it to its own includer; a plain module
//! that includes it is the base, and its includers reach the accessor
//! through it. Core's shape: SessionAccessorConcern declares
//! `attr_reader :session_accessor_prefix` there, and the methods its
//! `session_accessor` macro defines read it.
//!
//! Each case runs the emitted app against the same classes under native
//! Rails (the pinned oracle, tests/support/rails_oracle.rb). Fixtures are
//! written into the Overlay's scratch copy, never into `fixtures/real-blog`
//! (a link shared by every worktree).

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

const ORACLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");

/// Runs `program` (a file: the oracle re-executes it under jemalloc).
fn native(name: &str, program: &str) -> String {
    let dir = std::env::temp_dir().join(format!(
        "rh_concern_library_accessors_{name}_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("main.rb"),
        format!("require {ORACLE:?}\n{program}"),
    )
    .unwrap();
    let out = Command::new("ruby")
        .arg("main.rb")
        .current_dir(&dir)
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "native {name} failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::remove_dir_all(&dir).unwrap();
    stdout
}

const NATIVE_PRELUDE: &str = "RailsOracle.activate(\"activesupport\", \"activemodel\", \"activerecord\")\n\
     require \"active_support\"\nrequire \"active_support/concern\"\nrequire \"active_support/core_ext/array/wrap\"\n";

// ---- Core's shape: the accessor read by session_accessor's methods -------

const SESSION_ACCESSOR_CONCERN: &str = r##"module SessionAccessorConcern
  extend ActiveSupport::Concern

  included do
    attr_reader :session_accessor_prefix
  end

  class_methods do
    def session_accessor(*names)
      Array.wrap(names).each do |name|
        define_method("#{name}=") do |value|
          key = [session_accessor_prefix, name].compact.join("_").to_sym
          @session[key] = value
        end
        define_method(name.to_s) do
          key = [session_accessor_prefix, name].compact.join("_").to_sym
          @session[key]
        end
      end
    end
  end
end
"##;

const USER_SESSION_ATTRIBUTES: &str = r##"class UserSessionAttributes
  include SessionAccessorConcern

  session_accessor :login_at,
    :user_id,
    :staff_forged_user_id

  def initialize(session, shop = nil, prefix = nil)
    @session = session
    @shop = shop
    @session_accessor_prefix = prefix
  end

  def identity_login?
    !!user_id
  end
end
"##;

const EMPLOYEE_SESSION_ATTRIBUTES: &str = r##"class EmployeeSessionAttributes
  include SessionAccessorConcern

  session_accessor :login_at, :employee_id, :staff_forged_user_id

  def initialize(session, shop = nil)
    @session = session
    @shop = shop
  end
end
"##;

const SESSION_SCRIPT: &str = r##"session = {}
u = UserSessionAttributes.new(session)
p u.session_accessor_prefix
p u.identity_login?
u.user_id = 7
u.login_at = 42
p [u.user_id, u.login_at, u.identity_login?]
w = UserSessionAttributes.new(session, nil, "admin")
p w.session_accessor_prefix
w.user_id = 9
w.staff_forged_user_id = 11
p [w.user_id, w.staff_forged_user_id, u.user_id]
e = EmployeeSessionAttributes.new(session)
e.employee_id = 3
p [e.employee_id, e.session_accessor_prefix]
p session
p UserSessionAttributes.instance_method(:session_accessor_prefix).owner
p EmployeeSessionAttributes.instance_method(:session_accessor_prefix).owner
p UserSessionAttributes.public_method_defined?(:session_accessor_prefix)
p UserSessionAttributes.method_defined?(:session_accessor_prefix=)
p UserSessionAttributes.instance_method(:session_accessor_prefix).arity
"##;

#[test]
fn session_accessor_reads_the_included_block_reader_as_native() {
    let expected = native(
        "session_accessor",
        &format!(
            "{NATIVE_PRELUDE}{SESSION_ACCESSOR_CONCERN}{USER_SESSION_ATTRIBUTES}{EMPLOYEE_SESSION_ATTRIBUTES}{SESSION_SCRIPT}"
        ),
    );
    let expected: Vec<&str> = expected.lines().collect();
    assert_eq!(
        expected,
        [
            "nil",
            "false",
            "[7, 42, true]",
            r#""admin""#,
            "[9, 11, 7]",
            "[3, nil]",
            "{user_id: 7, login_at: 42, admin_user_id: 9, admin_staff_forged_user_id: 11, employee_id: 3}",
            "UserSessionAttributes",
            "EmployeeSessionAttributes",
            "true",
            "false",
            "0",
        ],
        "native control"
    );
    let run = emit_and_run::real_blog()
        .write(
            "app/models/concerns/session_accessor_concern.rb",
            SESSION_ACCESSOR_CONCERN,
        )
        .write(
            "app/models/user_session_attributes.rb",
            USER_SESSION_ATTRIBUTES,
        )
        .write(
            "app/models/employee_session_attributes.rb",
            EMPLOYEE_SESSION_ATTRIBUTES,
        )
        .run_ruby(SESSION_SCRIPT);
    run.assert_passes();
    let actual: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(
        actual, expected,
        "emitted Ruby diverges from native line for line"
    );
}

// ---- Transitively: a deferring Concern, a plain module, `private` --------

const PREFIXED: &str = r##"module Prefixed
  extend ActiveSupport::Concern

  included do
    attr_accessor :prefix
    attr_writer :suffix
  end

  def label
    [prefix, @suffix].compact.join("-")
  end
end
"##;

/// A Concern: `include Prefixed` is deferred to its includer.
const PREFIXED_SCOPE: &str = r##"module PrefixedScope
  extend ActiveSupport::Concern
  include Prefixed
end
"##;

/// A plain module: `include Prefixed` runs the block on the module.
const PREFIXED_CARRIER: &str = r##"module PrefixedCarrier
  include Prefixed
end
"##;

const SCOPED_LABEL: &str = r##"class ScopedLabel
  private

  include PrefixedScope

  def secret = :hidden
end
"##;

const CARRIED_LABEL: &str = r##"class CarriedLabel
  include PrefixedCarrier
end
"##;

const LABEL_SCRIPT: &str = r##"s = ScopedLabel.new
s.prefix = "a"
s.suffix = "z"
p [s.prefix, s.label]
c = CarriedLabel.new
p c.label
c.prefix = "b"
p c.label
p ScopedLabel.instance_method(:prefix).owner
p ScopedLabel.instance_method(:suffix=).owner
p CarriedLabel.instance_method(:prefix=).owner
p ScopedLabel.public_method_defined?(:prefix)
p ScopedLabel.public_method_defined?(:suffix=)
p ScopedLabel.private_method_defined?(:secret)
p ScopedLabel.method_defined?(:suffix)
p ScopedLabel.instance_method(:suffix=).arity
"##;

#[test]
fn accessors_land_on_the_base_that_ran_the_block_as_native() {
    let expected = native(
        "labels",
        &format!(
            "{NATIVE_PRELUDE}{PREFIXED}{PREFIXED_SCOPE}{PREFIXED_CARRIER}{SCOPED_LABEL}{CARRIED_LABEL}{LABEL_SCRIPT}"
        ),
    );
    let expected: Vec<&str> = expected.lines().collect();
    assert_eq!(
        expected,
        [
            r#"["a", "a-z"]"#,
            r#""""#,
            r#""b""#,
            "ScopedLabel",
            "ScopedLabel",
            "PrefixedCarrier",
            "true",
            "true",
            "true",
            "false",
            "1",
        ],
        "native control"
    );
    let run = emit_and_run::real_blog()
        .write("app/models/concerns/prefixed.rb", PREFIXED)
        .write("app/models/concerns/prefixed_scope.rb", PREFIXED_SCOPE)
        .write("app/models/concerns/prefixed_carrier.rb", PREFIXED_CARRIER)
        .write("app/models/scoped_label.rb", SCOPED_LABEL)
        .write("app/models/carried_label.rb", CARRIED_LABEL)
        .run_ruby(LABEL_SCRIPT);
    run.assert_passes();
    let actual: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(
        actual, expected,
        "emitted Ruby diverges from native line for line"
    );
}

// ---- Gaps stay explicit ----------------------------------------------------

fn tree(files: &[(&str, &str)]) -> roundhouse::det::HashMap<std::path::PathBuf, Vec<u8>> {
    let schema = (
        "db/schema.rb",
        "ActiveRecord::Schema.define { create_table(:messages) { |t| t.string :body } }",
    );
    files
        .iter()
        .chain([&schema])
        .map(|(p, c)| (std::path::PathBuf::from(p), c.as_bytes().to_vec()))
        .collect()
}

/// Strict ingest fails with the refusal; survey ingest records it once
/// and leaves the includer without the accessor it could not settle.
fn refused(files: &[(&str, &str)], owner: &str) -> (String, roundhouse::App) {
    use roundhouse::ingest::{ingest_app_from_tree, survey};
    let error = ingest_app_from_tree(tree(files))
        .expect_err("the activation is refused")
        .to_string();
    assert!(
        error.contains("concern attr_accessor") && error.contains(&format!("on {owner}")),
        "{error}"
    );
    survey::activate();
    let result = ingest_app_from_tree(tree(files));
    let gaps = survey::drain();
    let app = result.expect("survey keeps the app");
    assert_eq!(
        gaps.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
        [error.clone()]
    );
    (error, app)
}

fn methods<'a>(app: &'a roundhouse::App, owner: &str) -> Vec<&'a str> {
    let lc = app
        .library_classes
        .iter()
        .find(|lc| lc.name.0.as_str() == owner)
        .expect(owner);
    lc.methods.iter().map(|m| m.name.as_str()).collect()
}

/// The includer's own `def prefix` is ordered against the `include` in a
/// body the IR no longer keeps in sequence: refused, not guessed. An
/// identical public accessor restated by the includer is no conflict.
#[test]
fn an_includer_definition_of_the_name_is_refused() {
    let (error, app) = refused(
        &[
            ("app/models/concerns/prefixed.rb", PREFIXED),
            (
                "app/models/clash_label.rb",
                "class ClashLabel\n  include Prefixed\n\n  def prefix = :mine\nend\n",
            ),
        ],
        "ClashLabel",
    );
    assert!(error.contains("`prefix`"), "{error}");
    // The rest of the block is still supplied; the includer's def stands.
    let mut names = methods(&app, "ClashLabel");
    names.sort();
    assert_eq!(names, ["prefix", "prefix=", "suffix="]);

    let app = roundhouse::ingest::ingest_app_from_tree(tree(&[
        ("app/models/concerns/prefixed.rb", PREFIXED),
        (
            "app/models/restated_label.rb",
            "class RestatedLabel\n  include Prefixed\n\n  attr_accessor :prefix\nend\n",
        ),
    ]))
    .expect("an identical public accessor is no conflict");
    let mut names = methods(&app, "RestatedLabel");
    names.sort();
    assert_eq!(names, ["prefix", "prefix=", "suffix="]);
}

/// A computed name and a conditional declaration have no static
/// accessor set, and a controller includer has no splice: each reports.
#[test]
fn a_dynamic_conditional_or_controller_activation_is_refused() {
    for declaration in [
        "attr_reader(*%i[prefix])",
        "attr_reader :prefix if ENV[\"X\"]",
    ] {
        let concern = format!(
            "module Prefixed\n  extend ActiveSupport::Concern\n\n  included do\n    {declaration}\n  end\nend\n"
        );
        let (_, app) = refused(
            &[
                ("app/models/concerns/prefixed.rb", &concern),
                (
                    "app/models/plain_label.rb",
                    "class PlainLabel\n  include Prefixed\nend\n",
                ),
            ],
            "PlainLabel",
        );
        assert!(methods(&app, "PlainLabel").is_empty(), "{declaration}");
    }
    refused(
        &[
            (
                "app/models/concerns/prefixed.rb",
                "module Prefixed\n  extend ActiveSupport::Concern\n\n  included do\n    attr_accessor :prefix\n  end\nend\n",
            ),
            (
                "app/controllers/application_controller.rb",
                "class ApplicationController < ActionController::Base; end",
            ),
            (
                "app/controllers/labels_controller.rb",
                "class LabelsController < ApplicationController\n  include Prefixed\nend\n",
            ),
        ],
        "LabelsController",
    );
}
