//! `enqueue_after_transaction_commit` and `ActiveRecord.after_all_transactions_commit`
//! against native Rails 8.2 on the same local PostgreSQL database.
//!
//! Rails 8.2 holds a job whose class sets `enqueue_after_transaction_commit`
//! (activejob/lib/active_job/enqueue_after_transaction_commit.rb) until the
//! outermost joinable transaction commits, and drops it when one rolls
//! back; `ActiveRecord.after_all_transactions_commit` runs its block at once
//! outside a transaction. Shopify Core sets
//! `config.active_job.enqueue_after_transaction_commit = true`, so every job
//! that does not opt out is held: `assert_no_enqueued_jobs` around a
//! rolled-back transaction passes natively and failed emitted, which
//! enqueued at the call.
//!
//! The runtime half is `ActiveJob::EnqueueAfterTransactionCommit` in
//! runtime/spinel/active_job_callbacks.rb (the class attribute, `raw_enqueue`
//! and `perform_all_later`), over the transaction manager of
//! runtime/spinel/cruby/active_record_transactions.rb; the compiler half
//! lifts the app's `config.active_job` key (`project::active_job_config_block`)
//! and replays a job body's `self.enqueue_after_transaction_commit = <bool>`.
//!
//! The same job files run natively (Core's pinned Rails, `:inline` adapter,
//! the railtie's include and config done by `native_prelude.rb`) and on the
//! emitted program, each on a freshly created database, and every line
//! `EnqueueProbe.lines` prints must be equal, and equal to what Rails is
//! known to answer.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE things (
    id bigserial NOT NULL,
    name character varying,
    CONSTRAINT things_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_enqueue_after_commit
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

const THING: &str = r#"class Thing < ApplicationRecord
end
"#;

const JOB_LOG: &str = r#"class JobLog
  def self.entries
    @entries ||= []
  end

  def self.add(entry)
    entries << entry
  end

  def self.take
    taken = entries.join(" ")
    entries.clear
    taken
  end
end
"#;

/// A callback in the base puts every job on the ActiveJob::Callbacks port,
/// as Core's BackgroundQueue::Job does.
const APPLICATION_JOB: &str = r#"class ApplicationJob < ActiveJob::Base
  before_perform { |job| JobLog.add("ran:#{job.class.name}:#{job.arguments.first}") }
end
"#;

/// Sets nothing: reads the app's `config.active_job` value.
const PLAIN_JOB: &str = r#"class PlainJob < ApplicationJob
  def perform(tag)
  end
end
"#;

/// Opts out, as Core's TaxCalculationJob does.
const IMMEDIATE_JOB: &str = r#"class ImmediateJob < ApplicationJob
  self.enqueue_after_transaction_commit = false

  def perform(tag)
  end
end
"#;

/// Opts in, as most of Core's jobs do.
const DEFERRED_JOB: &str = r#"class DeferredJob < ApplicationJob
  self.enqueue_after_transaction_commit = true

  def perform(tag)
  end
end
"#;

/// A job that inherits the opt-in from a parent job class.
const CHILD_JOB: &str = r#"class ChildJob < DeferredJob
end
"#;

const PROBE: &str = r##"class EnqueueProbe
  def self.run(out, label)
    JobLog.take
    yield
    out << "#{label}: #{JobLog.take}"
  end

  def self.lines
    out = []
    run(out, "outside a transaction") do
      PlainJob.perform_later("a")
      DeferredJob.perform_later("a")
      JobLog.add("after-enqueue")
    end
    run(out, "commit") do
      Thing.transaction do
        PlainJob.perform_later("b")
        DeferredJob.perform_later("b")
        ImmediateJob.perform_later("b")
        JobLog.add("in-txn")
      end
      JobLog.add("after-txn")
    end
    run(out, "rollback") do
      Thing.transaction do
        PlainJob.perform_later("c")
        DeferredJob.perform_later("c")
        ImmediateJob.perform_later("c")
        JobLog.add("in-txn")
        raise ActiveRecord::Rollback
      end
      JobLog.add("after-txn")
    end
    run(out, "exception") do
      begin
        Thing.transaction do
          DeferredJob.perform_later("d")
          raise "boom"
        end
      rescue => e
        JobLog.add(e.message)
      end
    end
    run(out, "inherited opt-in") do
      Thing.transaction do
        ChildJob.perform_later("e")
        JobLog.add("in-txn")
      end
    end
    run(out, "joined nested") do
      Thing.transaction do
        Thing.transaction do
          DeferredJob.perform_later("f")
          JobLog.add("inner")
        end
        JobLog.add("outer")
      end
      JobLog.add("done")
    end
    run(out, "savepoint rolled back") do
      Thing.transaction do
        DeferredJob.perform_later("g-outer")
        Thing.transaction(requires_new: true) do
          DeferredJob.perform_later("g-inner")
          raise ActiveRecord::Rollback
        end
        JobLog.add("outer")
      end
    end
    run(out, "savepoint committed") do
      Thing.transaction do
        Thing.transaction(requires_new: true) do
          DeferredJob.perform_later("h")
          JobLog.add("inner")
        end
        JobLog.add("outer")
      end
    end
    run(out, "with a write") do
      Thing.transaction do
        Thing.create!(name: "x")
        DeferredJob.perform_later("i")
        JobLog.add("rows:#{Thing.count}")
      end
    end
    run(out, "rolled back write") do
      Thing.transaction do
        Thing.create!(name: "y")
        DeferredJob.perform_later("j")
        raise ActiveRecord::Rollback
      end
      JobLog.add("rows:#{Thing.count}")
    end
    run(out, "return value") do
      Thing.transaction do
        job = DeferredJob.perform_later("k")
        JobLog.add("#{job.class} #{job.successfully_enqueued?}")
        late = ImmediateJob.perform_later("k")
        JobLog.add("#{late.class} #{late.successfully_enqueued?}")
      end
    end
    run(out, "perform_all_later") do
      Thing.transaction do
        ActiveJob.perform_all_later([DeferredJob.new("l"), ImmediateJob.new("l"), PlainJob.new("l")])
        JobLog.add("in-txn")
      end
    end
    run(out, "perform_all_later outside") do
      ActiveJob.perform_all_later([DeferredJob.new("m"), ImmediateJob.new("m")])
      JobLog.add("after")
    end
    run(out, "perform_all_later rolled back") do
      Thing.transaction do
        ActiveJob.perform_all_later([DeferredJob.new("n"), ImmediateJob.new("n")])
        raise ActiveRecord::Rollback
      end
    end
    run(out, "after_all outside") do
      ActiveRecord.after_all_transactions_commit { JobLog.add("block") }
      JobLog.add("after")
    end
    run(out, "after_all commit") do
      Thing.transaction do
        ActiveRecord.after_all_transactions_commit { JobLog.add("block") }
        JobLog.add("in-txn")
      end
      JobLog.add("after-txn")
    end
    run(out, "after_all rollback") do
      Thing.transaction do
        ActiveRecord.after_all_transactions_commit { JobLog.add("block") }
        raise ActiveRecord::Rollback
      end
      JobLog.add("after-txn")
    end
    run(out, "after_all nested") do
      Thing.transaction do
        Thing.transaction do
          ActiveRecord.after_all_transactions_commit { JobLog.add("block") }
          JobLog.add("inner")
        end
        JobLog.add("outer")
      end
      JobLog.add("after-txn")
    end
    run(out, "after_all savepoint rollback") do
      Thing.transaction do
        Thing.transaction(requires_new: true) do
          ActiveRecord.after_all_transactions_commit { JobLog.add("block") }
          raise ActiveRecord::Rollback
        end
        JobLog.add("outer")
      end
    end
    run(out, "after_all in a block") do
      Thing.transaction do
        ActiveRecord.after_all_transactions_commit do
          JobLog.add("block")
          DeferredJob.perform_later("o")
        end
      end
    end
    out
  end
end
"##;

/// What the railtie does natively once the app sets
/// `config.active_job.enqueue_after_transaction_commit`: the module is
/// included in ActiveJob::Base after Active Record loads, and the value is
/// assigned. The `:inline` adapter runs a job at its enqueue, so the log
/// shows when it was enqueued.
fn native_prelude(config: bool) -> String {
    format!(
        "require \"logger\"\nActiveJob::Base.queue_adapter = :inline\nActiveJob::Base.logger = Logger.new(nil)\n\
         ActiveJob::Base.include ActiveJob::EnqueueAfterTransactionCommit\n{}",
        if config { "ActiveJob::Base.enqueue_after_transaction_commit = true\n" } else { "" }
    )
}

const APPLICATION_RB: &str = r#"require_relative "boot"
require "rails/all"

module Probe
  class Application < Rails::Application
    config.active_job.enqueue_after_transaction_commit = true
  end
end
"#;

/// What native requires, in order.
const LOADS: [&str; 9] = [
    "app/models/job_log",
    "app/models/application_record",
    "app/models/thing",
    "app/models/application_job",
    "app/models/plain_job",
    "app/models/immediate_job",
    "app/models/deferred_job",
    "app/models/child_job",
    "app/models/enqueue_probe",
];

/// The app's files; `config` is whether it sets
/// `config.active_job.enqueue_after_transaction_commit = true`.
fn files(config: bool) -> Vec<(&'static str, String)> {
    let mut files: Vec<(&'static str, String)> = vec![
        ("roundhouse.yml", "database_adapter: postgresql\n".into()),
        ("config/database.yml", DATABASE_YML.into()),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n".into()),
        ("db/structure.sql", STRUCTURE.into()),
        ("app/models/application_record.rb", APPLICATION_RECORD.into()),
        ("app/models/thing.rb", THING.into()),
        ("app/models/job_log.rb", JOB_LOG.into()),
        ("app/models/application_job.rb", APPLICATION_JOB.into()),
        ("app/models/plain_job.rb", PLAIN_JOB.into()),
        ("app/models/immediate_job.rb", IMMEDIATE_JOB.into()),
        ("app/models/deferred_job.rb", DEFERRED_JOB.into()),
        ("app/models/child_job.rb", CHILD_JOB.into()),
        ("app/models/enqueue_probe.rb", PROBE.into()),
    ];
    if config {
        files.push(("config/application.rb", APPLICATION_RB.into()));
    }
    files
}

fn overlay(config: bool) -> emit_and_run::Overlay {
    files(config).iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

/// `check`'s errors other than the PostgreSQL adapter notice every
/// PostgreSQL probe app carries.
fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

fn read(emitted: &std::path::Path, file: &str) -> String {
    std::fs::read_to_string(emitted.join(file)).unwrap_or_else(|e| panic!("{file}: {e}"))
}

/// The compiler side: the app's config is applied to the job base, a body's
/// literal is replayed, and no "dropped" line is written for either.
#[test]
fn enqueue_after_commit_config_and_body_assignments_compile() {
    let (emitted, errors) = overlay(true).emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let models = read(&emitted, "app/models.rb");
    assert!(
        models.contains("ActiveJob::Base.enqueue_after_transaction_commit = Rails.application.active_job_enqueue_after_transaction_commit"),
        "the config is not applied to the job base:\n{models}"
    );
    let application = read(&emitted, "config/application.rb");
    assert!(application.contains("def active_job_enqueue_after_transaction_commit"), "{application}");
    for (file, setting) in [
        ("app/models/immediate_job.rb", "self.enqueue_after_transaction_commit = false"),
        ("app/models/deferred_job.rb", "self.enqueue_after_transaction_commit = true"),
    ] {
        let job = read(&emitted, file);
        assert!(job.contains(setting), "{file} lost its setting:\n{job}");
    }
    let port = read(&emitted, "runtime/active_job_callbacks.rb");
    assert!(
        port.contains("module EnqueueAfterTransactionCommit") && port.contains("after_all_transactions_commit"),
        "the port does not defer the enqueue"
    );
    // An app that never writes the key applies nothing.
    let (plain, errors) = overlay(false).emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    assert!(!read(&plain, "app/models.rb").contains("ActiveJob::Base.enqueue_after_transaction_commit ="));
}

/// The job files natively and emitted, same PostgreSQL.
fn differential(config: bool, expected: &[&str]) {
    let (emitted, errors) = overlay(config).emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let mut native_files = files(config);
    native_files.push(("native_prelude.rb", native_prelude(config)));
    let native_files: Vec<(&str, &str)> = native_files.iter().map(|(p, t)| (*p, t.as_str())).collect();
    let (native, emitted) = pg_probe::native_and_emitted(
        "enqueue-after-commit",
        &native_files,
        &emitted,
        "rh_e2e_enqueue_after_commit",
        "EnqueueProbe",
        &LOADS,
    );
    assert_eq!(native, expected, "native");
    assert_eq!(emitted, native, "emitted");
}

/// Core's setting: `config.active_job.enqueue_after_transaction_commit = true`.
#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn enqueue_after_commit_with_the_app_config_matches_native_rails_on_postgresql() {
    differential(true, &EXPECTED_CONFIGURED);
}

/// No app config: Rails' default (false) holds only the jobs that opt in.
#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn enqueue_after_commit_by_default_matches_native_rails_on_postgresql() {
    differential(false, &EXPECTED_DEFAULT);
}

/// What native Rails 8.2 answers (checked on the task PostgreSQL), so two
/// identical wrong answers still fail. A job's `ran:` line is written when
/// it is enqueued (the `:inline` adapter).
const EXPECTED_CONFIGURED: [&str; 20] = [
    "outside a transaction: ran:PlainJob:a ran:DeferredJob:a after-enqueue",
    "commit: ran:ImmediateJob:b in-txn ran:PlainJob:b ran:DeferredJob:b after-txn",
    "rollback: ran:ImmediateJob:c in-txn after-txn",
    "exception: boom",
    "inherited opt-in: in-txn ran:ChildJob:e",
    "joined nested: inner outer ran:DeferredJob:f done",
    "savepoint rolled back: outer ran:DeferredJob:g-outer",
    "savepoint committed: inner outer ran:DeferredJob:h",
    "with a write: rows:1 ran:DeferredJob:i",
    "rolled back write: rows:1",
    "return value: DeferredJob true ran:ImmediateJob:k ImmediateJob true ran:DeferredJob:k",
    "perform_all_later: ran:ImmediateJob:l in-txn ran:DeferredJob:l ran:PlainJob:l",
    "perform_all_later outside: ran:ImmediateJob:m ran:DeferredJob:m after",
    "perform_all_later rolled back: ran:ImmediateJob:n",
    "after_all outside: block after",
    "after_all commit: in-txn block after-txn",
    "after_all rollback: after-txn",
    "after_all nested: inner outer block after-txn",
    "after_all savepoint rollback: outer",
    "after_all in a block: block ran:DeferredJob:o",
];

const EXPECTED_DEFAULT: [&str; 20] = [
    "outside a transaction: ran:PlainJob:a ran:DeferredJob:a after-enqueue",
    "commit: ran:PlainJob:b ran:ImmediateJob:b in-txn ran:DeferredJob:b after-txn",
    "rollback: ran:PlainJob:c ran:ImmediateJob:c in-txn after-txn",
    "exception: boom",
    "inherited opt-in: in-txn ran:ChildJob:e",
    "joined nested: inner outer ran:DeferredJob:f done",
    "savepoint rolled back: outer ran:DeferredJob:g-outer",
    "savepoint committed: inner outer ran:DeferredJob:h",
    "with a write: rows:1 ran:DeferredJob:i",
    "rolled back write: rows:1",
    "return value: DeferredJob true ran:ImmediateJob:k ImmediateJob true ran:DeferredJob:k",
    "perform_all_later: ran:ImmediateJob:l ran:PlainJob:l in-txn ran:DeferredJob:l",
    "perform_all_later outside: ran:ImmediateJob:m ran:DeferredJob:m after",
    "perform_all_later rolled back: ran:ImmediateJob:n",
    "after_all outside: block after",
    "after_all commit: in-txn block after-txn",
    "after_all rollback: after-txn",
    "after_all nested: inner outer block after-txn",
    "after_all savepoint rollback: outer",
    "after_all in a block: block ran:DeferredJob:o",
];
