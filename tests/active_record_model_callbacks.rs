//! ActiveRecord::Base includes ActiveSupport::Callbacks, so a model's
//! class body may define and register its own callback chains. Shopify
//! Core's Payments::Vault concerns do (components/payments/vault,
//! `AfterRevocation` and `AroundRedaction`): their `included do` blocks
//! `include ActiveSupport::Callbacks`, `define_callbacks :revocation` /
//! `:redaction` and prepend a `run_callbacks { super }` wrapper; their
//! `class_methods` define `after_revocation` / `around_redaction` over
//! `set_callback ..., if:`; Credential::Shared includes both and defines
//! `revoke!` / `redact!` in its own `included do`; Credential::Global
//! and ::Shop include Shared, and their STI subclasses
//! (shop_pay_billing_agreement.rb and others) call `after_revocation`.
//!
//! Model lowering used to drop every such class-body call with a
//! `lower_residue` warning, so `check` passed and the callbacks never
//! ran. A lowered model now includes the module and replays them through
//! the runtime port (runtime/spinel/active_support_callbacks.rb), as an
//! STI subclass (a library class) does its own; the same shape runs
//! natively on Core's pinned activerecord with sqlite3
//! (tests/support/rails_oracle.rb) and emitted on the SQLite CRuby tree,
//! and must print the same. Calls on ActiveRecord's own chains
//! (`skip_callback :save, ...`), which a lowered model does not run
//! through the port, and any other class-body DSL call model lowering
//! drops, are `check` errors. Spinel refuses the module by name.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use std::process::Command;

const APPLICATION_RECORD: &str = "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n";

const AFTER_REVOCATION: &str = r#"module Vault
  module AfterRevocation
    extend ActiveSupport::Concern

    module RevokeWrapper
      def revoke!
        run_callbacks :revocation do
          super
        end
      end
    end

    class_methods do
      def after_revocation(callback, if: nil)
        set_callback :revocation, :after, callback, if:
      end
    end

    included do
      prepend RevokeWrapper
      include ActiveSupport::Callbacks

      define_callbacks :revocation
    end
  end
end
"#;

const AROUND_REDACTION: &str = r#"module Vault
  module AroundRedaction
    extend ActiveSupport::Concern

    class_methods do
      def around_redaction(callback, if: nil)
        set_callback :redaction, :around, callback, if:
      end
    end

    included do
      include ActiveSupport::Callbacks

      define_callbacks :redaction
    end
  end
end
"#;

const SHARED: &str = r#"module Vault
  module Shared
    extend ActiveSupport::Concern
    include Vault::AfterRevocation
    include Vault::AroundRedaction

    included do
      def redact!
        run_callbacks(:redaction) do
          revoke!
          log << :redact
        end
      end

      def revoke!
        return if revoked?

        @revoked = true
        log << :revoke
      end
    end

    def revoked?
      @revoked == true
    end

    def log
      @log ||= []
    end
  end
end
"#;

const CARD: &str = r#"class Card < ApplicationRecord
  include Vault::AfterRevocation
  include Vault::AroundRedaction

  after_revocation :notify_revoked
  after_revocation -> { log << :conditional }, if: -> { log.size > 1 }
  around_redaction :capture_around

  def log
    @log ||= []
  end

  def revoke!
    log << :revoke
  end

  def redact!
    run_callbacks(:redaction) { log << :redact }
  end

  private

  def notify_revoked
    log << :notified
  end

  def capture_around
    log << :around_before
    yield
    log << :around_after
  end
end
"#;

const GRANT: &str = "class Grant < ApplicationRecord\n  include Vault::Shared\nend\n";

const AGREEMENT: &str = r#"class Grant
  class Agreement < Grant
    after_revocation :disable
    around_redaction :wrap, if: :wrap?

    def disable
      log << :disabled
    end

    def wrap?
      name != "plain"
    end

    def wrap
      captured = name
      yield
      log << [:wrapped, captured]
    end
  end
end
"#;

const JOB: &str = r#"class Job < ApplicationRecord
  define_callbacks :perform, skip_after_callbacks_if_terminated: true
  set_callback :perform, :before, :prepare
  set_callback :perform, :after, :finish
  set_callback :perform, :after, :audit, if: -> { name == "audited" }

  def log
    @log ||= []
  end

  def perform
    run_callbacks(:perform) { log << :run }
    log
  end

  private

  def prepare
    log << :prepare
    throw :abort if name == "halt"
  end

  def finish
    log << :finish
  end

  def audit
    log << :audit
  end
end
"#;

const QUIET_JOB: &str = "class QuietJob < Job\n  skip_callback :perform, :after, :finish\nend\n";

const SCHEMA: &str = r#"ActiveRecord::Schema.define(version: 1) do
  create_table "cards", force: :cascade do |t|
    t.string "name"
  end
  create_table "grants", force: :cascade do |t|
    t.string "name"
    t.string "type"
  end
  create_table "jobs", force: :cascade do |t|
    t.string "name"
    t.string "type"
  end
end
"#;

/// In load order, for the native run.
const FILES: &[(&str, &str)] = &[
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/concerns/vault/after_revocation.rb", AFTER_REVOCATION),
    ("app/models/concerns/vault/around_redaction.rb", AROUND_REDACTION),
    ("app/models/concerns/vault/shared.rb", SHARED),
    ("app/models/card.rb", CARD),
    ("app/models/grant.rb", GRANT),
    ("app/models/grant/agreement.rb", AGREEMENT),
    ("app/models/job.rb", JOB),
    ("app/models/quiet_job.rb", QUIET_JOB),
];

const OBSERVE: &str = r#"c = Card.new
c.revoke!
p [:card_revoke, c.log]
c2 = Card.new
c2.log << :x
c2.revoke!
p [:card_revoke_cond, c2.log]
c3 = Card.new
c3.redact!
p [:card_redact, c3.log]
g = Grant.new(name: "g")
g.redact!
p [:grant_redact, g.log]
a = Grant::Agreement.new(name: "a")
a.redact!
p [:agreement_redact, a.log]
a2 = Grant::Agreement.new(name: "plain")
a2.redact!
a2.revoke!
p [:agreement_plain, a2.log]
a.save!
r = Grant.find(a.id)
r.revoke!
p [:reloaded, r.class.name, r.log]
p [:job, Job.new(name: "j").perform]
p [:job_audited, Job.new(name: "audited").perform]
p [:job_halt, Job.new(name: "halt").perform]
p [:quiet, QuietJob.new(name: "audited").perform]
p [:quiet_halt, QuietJob.new(name: "halt").perform]
p [:job_chain, Job._perform_callbacks.map { |cb| [cb.kind, cb.filter] }]
p [:quiet_chain, QuietJob._perform_callbacks.map { |cb| [cb.kind, cb.filter] }]
p [:agreement_chains, Grant::Agreement._revocation_callbacks.map(&:kind), Grant._revocation_callbacks.map(&:kind)]
"#;

const EXPECTED: &str = r#"[:card_revoke, [:revoke, :notified]]
[:card_revoke_cond, [:x, :revoke, :conditional, :notified]]
[:card_redact, [:around_before, :redact, :around_after]]
[:grant_redact, [:revoke, :redact]]
[:agreement_redact, [:revoke, :disabled, :redact, [:wrapped, "a"]]]
[:agreement_plain, [:revoke, :disabled, :redact, :disabled]]
[:reloaded, "Grant::Agreement", [:revoke, :disabled]]
[:job, [:prepare, :run, :finish]]
[:job_audited, [:prepare, :run, :audit, :finish]]
[:job_halt, [:prepare]]
[:quiet, [:prepare, :run, :audit]]
[:quiet_halt, [:prepare]]
[:job_chain, [[:before, :prepare], [:after, :finish], [:after, :audit]]]
[:quiet_chain, [[:before, :prepare], [:after, :audit]]]
[:agreement_chains, [:after], []]
"#;

fn app() -> emit_and_run::Overlay {
    let mut app = emit_and_run::empty_app();
    for (path, text) in FILES {
        app = app.write(path, text);
    }
    app.write("db/schema.rb", SCHEMA)
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("config/database.yml", "test:\n  adapter: sqlite3\n  database: \":memory:\"\n")
}

fn native() -> String {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let dir = std::env::temp_dir().join(format!("rh_active_record_model_callbacks_native_{}", std::process::id()));
    for (path, text) in FILES.iter().chain([&("db/schema.rb", SCHEMA)]) {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let loads: String = FILES.iter().map(|(path, _)| format!("load {path:?}\n")).collect();
    let program = format!(
        "require {oracle:?}\nRailsOracle.activate(\"activerecord\", sqlite3: \"2.9.2\")\nrequire \"active_record\"\n\
         abort \"native: activerecord is not Core's\" unless Gem.loaded_specs[\"activerecord\"].full_gem_path.start_with?(RailsOracle::CORE_GEM_HOME)\n\
         ActiveRecord::Base.establish_connection(adapter: \"sqlite3\", database: \":memory:\")\n\
         ActiveRecord::Migration.verbose = false\nload \"db/schema.rb\"\n{loads}{OBSERVE}"
    );
    std::fs::write(dir.join("native.rb"), program).unwrap();
    let out = Command::new("ruby")
        .arg("native.rb")
        .current_dir(&dir)
        .env("GLIBC_TUNABLES", "glibc.rtld.optional_static_tls=262144")
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "native run failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    stdout
}

#[test]
fn model_callback_chains_run_like_activerecord() {
    let (dir, errors) = app().emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let read = |file: &str| std::fs::read_to_string(dir.join(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
    let card = read("app/models/card.rb");
    for line in [
        "include ActiveSupport::Callbacks",
        "prepend Vault::AfterRevocation::RevokeWrapper",
        "define_callbacks :revocation",
        "define_callbacks :redaction",
        "after_revocation :notify_revoked",
        "around_redaction :capture_around",
    ] {
        assert!(card.contains(line), "card.rb lacks {line:?}:\n{card}");
    }
    // The macros run after the class methods that define them.
    assert!(card.find("def self.after_revocation") < card.rfind("after_revocation :notify_revoked"), "{card}");
    let agreement = read("app/models/grant/agreement.rb");
    assert!(agreement.contains("after_revocation :disable") && agreement.contains("around_redaction :wrap, if: :wrap?"), "{agreement}");
    assert!(read("app/models/quiet_job.rb").contains("skip_callback :perform, :after, :finish"));
    let job = read("app/models/job.rb");
    assert!(job.contains("include ActiveSupport::Callbacks") && job.contains("define_callbacks :perform, skip_after_callbacks_if_terminated: true"), "{job}");
    assert!(dir.join("runtime/active_support_callbacks.rb").exists());

    let script = format!("require File.expand_path('main', Dir.pwd)\nMain.configure_default_adapter!\n{OBSERVE}");
    let out = emit_and_run::ruby().current_dir(&*dir).args(["-W0", "-e", &script]).env("BLOG_DB", ":memory:").output().expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stdout, EXPECTED);
    assert_eq!(stdout, native(), "emitted on the port vs native activerecord");
}

#[test]
fn spinel_refuses_the_models_callbacks() {
    let (_dir, errors) = app().emit(BuildTarget::Spinel);
    for (class, verb) in [("Card", "includes"), ("Grant", "includes"), ("Job", "uses")] {
        let expected = format!(
            "{class} {verb} ActiveSupport::Callbacks, declared by the Ruby-family runtime (runtime/active_support_callbacks.rb), which spinel does not emit"
        );
        assert!(errors.iter().any(|e| e.contains(&expected)), "spinel {class}: {errors:#?}");
    }
}

/// What model lowering drops is a `check` error, not a warning: a call on
/// one of ActiveRecord's own chains (which the lowered model runs without
/// the port), in a model or an STI subclass, and an unknown macro.
#[test]
fn dropped_model_dsl_is_a_check_error() {
    let app = app()
        .edit("app/models/card.rb", "  around_redaction :capture_around\n", "  around_redaction :capture_around\n  set_callback :save, :after, :notify_revoked\n  frobnicate :x\n")
        .edit("app/models/quiet_job.rb", "  skip_callback :perform, :after, :finish\n", "  skip_callback :perform, :after, :finish\n  skip_callback :save, :before, :prepare\n");
    let (_dir, errors) = app.emit(BuildTarget::Ruby);
    for expected in [
        "set_callback not supported (all targets): model DSL call on `Card` not lowered; the class does not run it",
        "frobnicate not supported (all targets): model DSL call on `Card` not lowered; the class does not run it",
        "skip_callback not supported (all targets): STI subclass body call on `QuietJob` not lowered; the class does not run it",
    ] {
        assert!(errors.iter().any(|e| e.contains(expected)), "{expected}: {errors:#?}");
    }
    assert_eq!(errors.len(), 3, "{errors:#?}");
}
