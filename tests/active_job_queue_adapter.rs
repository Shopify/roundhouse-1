//! `ActiveJob::QueueAdapter`'s class-level adapter (`self.queue_adapter =`)
//! and the test helper's `disable_test_adapter` (activejob's
//! test_helper.rb `TestQueueAdapter`), as Shopify Core's
//! `BackgroundQueue::Job` and `WithProductionJobQueue` use them.
//!
//! Core's `BackgroundQueue::Job` sets `self.queue_adapter = HedwigAdapter`,
//! and a test that includes `WithProductionJobQueue` calls
//! `BackgroundQueue::Job.disable_test_adapter` in `before_setup` so its
//! jobs flow through that adapter instead of the suite's `:test` one.
//! The class-body write was dropped ("not modelled") and the runtime had
//! no `queue_adapter` or `disable_test_adapter`, so 13 sweep-1 tests
//! stopped in setup with `undefined method 'disable_test_adapter'`.
//!
//! The jobs below run natively on Core's pinned activejob with the test
//! adapter installed the way `before_setup` installs it, and emitted on
//! the port (enqueue-only mode holds what the `:test` adapter holds); both
//! must print the same: which adapter took each job, scheduled or not,
//! that a subclass of a disabled class is disabled too, that a class
//! nobody disabled stays on the test adapter, and that the reset
//! `after_teardown` runs puts every class back on it.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use std::process::Command;

const APPLICATION_JOB: &str = r#"class ApplicationJob < ActiveJob::Base
  TRAIL = []

  before_enqueue { |job| TRAIL << "before_enqueue #{job.class}" }
end
"#;

/// The production adapter: records what it is handed, as Hedwig's
/// `enqueue` / `enqueue_at` receive a job and a timestamp.
const RECORDING_ADAPTER: &str = r#"class RecordingAdapter
  def self.enqueue(job)
    ApplicationJob::TRAIL << "adapter enqueue #{job.class} #{job.arguments.inspect}"
  end

  def self.enqueue_at(job, timestamp)
    ApplicationJob::TRAIL << "adapter enqueue_at #{job.class} #{job.arguments.inspect} #{timestamp.class}"
  end
end
"#;

const QUEUED_JOB: &str = r#"class QueuedJob < ApplicationJob
  self.queue_adapter = RecordingAdapter

  def perform(*args)
    TRAIL << "perform #{self.class} #{args.inspect}"
  end
end
"#;

const SUB_JOB: &str = r#"class SubJob < QueuedJob
end
"#;

const OTHER_JOB: &str = r#"class OtherJob < ApplicationJob
  self.queue_adapter = RecordingAdapter

  def perform(*args)
    TRAIL << "perform #{self.class} #{args.inspect}"
  end
end
"#;

const FILES: &[(&str, &str, &str)] = &[
    ("app/jobs/application_job.rb", "app/models/application_job.rb", APPLICATION_JOB),
    ("app/jobs/recording_adapter.rb", "app/models/recording_adapter.rb", RECORDING_ADAPTER),
    ("app/jobs/queued_job.rb", "app/models/queued_job.rb", QUEUED_JOB),
    ("app/jobs/sub_job.rb", "app/models/sub_job.rb", SUB_JOB),
    ("app/jobs/other_job.rb", "app/models/other_job.rb", OTHER_JOB),
];

/// What a test's `before_setup` / `after_teardown` do to the test adapter,
/// then the same enqueues on both sides.
const OBSERVE: &str = r#"def step(label)
  ApplicationJob::TRAIL << "-- #{label}"
  yield
end

def finish
  puts ApplicationJob::TRAIL
  puts "held #{held_count}"
end

before_setup
step("before disable: every class is on the test adapter") do
  QueuedJob.perform_later(1)
  OtherJob.perform_later(2)
end
QueuedJob.disable_test_adapter
step("disabled class enqueues through its adapter") do
  QueuedJob.perform_later(3)
  QueuedJob.set(wait: 60).perform_later(4)
  p QueuedJob.queue_adapter
end
step("a subclass of a disabled class is disabled; another class is not") do
  SubJob.perform_later(5)
  OtherJob.perform_later(6)
  p SubJob.queue_adapter
end
after_teardown
before_setup
step("after the reset every class is back on the test adapter") do
  QueuedJob.perform_later(7)
  SubJob.perform_later(8)
end
finish
"#;

/// Core's activejob, natively: the `TestQueueAdapter` module
/// `ActiveJob::TestHelper` extends onto `ActiveJob::Base`, installed and
/// reset as the helper's `before_setup` / `after_teardown` do.
const NATIVE_ADAPTER: &str = r#"require "active_job/test_helper"

TQA = ActiveJob::TestHelper::TestQueueAdapter

def before_setup
  TQA.test_adapter = ActiveJob::QueueAdapters::TestAdapter.new
end

def after_teardown
  TQA.reset
end

def held_count
  TQA.test_adapter.enqueued_jobs.size
end
"#;

/// The port: enqueue-only mode is the `:test` adapter, and the reset is
/// what the overlay's `ActiveJob::TestHelper#after_teardown` runs.
const EMITTED_ADAPTER: &str = r#"def before_setup
  ActiveJob.clear_held
  ActiveJob.enqueue_without_running
end

def after_teardown
  ActiveJob::QueueAdapter.reset
  ActiveJob.run_enqueued
end

def held_count
  ActiveJob::HELD.length
end
"#;

fn requires() -> String {
    FILES
        .iter()
        .map(|(_, emitted, _)| format!("require_relative {:?}\n", emitted.trim_end_matches(".rb")))
        .collect()
}

fn native() -> String {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    static RUN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "rh_active_job_queue_adapter_native_{}_{}",
        std::process::id(),
        RUN.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    for (_, emitted, text) in FILES {
        let file = dir.join(emitted);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let program = format!(
        "require {oracle:?}\nRailsOracle.activate(\"activesupport\", \"activejob\")\n\
         require \"active_support\"\nrequire \"active_job\"\nrequire \"logger\"\n\
         abort \"native: activejob is not Core's\" unless Gem.loaded_specs[\"activejob\"].full_gem_path.start_with?(RailsOracle::CORE_GEM_HOME)\n\
         ActiveJob::Base.queue_adapter = :inline\nActiveJob::Base.logger = Logger.new(nil)\n\
         {}{NATIVE_ADAPTER}{OBSERVE}",
        requires()
    );
    std::fs::write(dir.join("native.rb"), program).unwrap();
    let out = Command::new("ruby")
        .args(["-W0", "native.rb"])
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
fn a_disabled_job_class_enqueues_through_its_queue_adapter_like_activejob() {
    let mut app = emit_and_run::empty_app().write("roundhouse.yml", "test_paths:\n  - test\n");
    for (path, _, text) in FILES {
        app = app.write(path, text);
    }
    let (dir, errors) = app.emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    for (_, file, _) in FILES {
        let text = std::fs::read_to_string(dir.join(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
        assert!(!text.contains("roundhouse:"), "{file}: {text}");
    }
    let queued = std::fs::read_to_string(dir.join("app/models/queued_job.rb")).unwrap();
    assert!(queued.contains("self.queue_adapter = RecordingAdapter"), "the adapter write is dropped:\n{queued}");

    let script = format!("require File.expand_path('main', Dir.pwd)\n{}{EMITTED_ADAPTER}{OBSERVE}", requires());
    let out = emit_and_run::ruby()
        .current_dir(&*dir)
        .args(["-W0", "-e", &script])
        .env("BLOG_DB", ":memory:")
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));

    let native = native();
    assert!(native.contains("adapter enqueue QueuedJob [3]"), "{native}");
    assert!(native.contains("adapter enqueue_at QueuedJob [4] Float"), "{native}");
    assert!(native.contains("adapter enqueue SubJob [5]"), "{native}");
    assert_eq!(stdout, native, "emitted on the port vs native activejob");
}
