//! activejob's `ActiveJob::Callbacks` and activesupport's
//! `ActiveSupport::Rescuable` (Shopify/rails 5813559f, 8.2.0.alpha:
//! callbacks.rb, core.rb, enqueuing.rb, execution.rb, rescuable.rb).
//! Shopify Core's jobs register `before_/after_/around_perform` and
//! `_enqueue` callbacks and `rescue_from` handlers in their bodies and in
//! their concerns' `included` blocks (BackgroundQueue::Job and its
//! concerns), and BackgroundQueue::WithFallback's `self.included(base)`
//! registers an `around_enqueue`. The class-body calls were dropped, the
//! `included do` callbacks were refused as survey gaps, and the
//! WithFallback hook kept BackgroundQueue::Job's carried class attributes
//! from being carried.
//!
//! The CRuby and JRuby trees carry the port
//! (runtime/spinel/active_job_callbacks.rb, with
//! active_support_rescuable.rb) and the topmost job class of a hierarchy
//! that registers one includes it (`lower::job_class_side`); its jobs take
//! `perform_now` and `perform_later` from it and the callbacks run around
//! the shared runtime's adapter. The jobs below run natively on Core's
//! pinned activejob (tests/support/rails_oracle.rb, the `:inline` and
//! `:test` adapters) and emitted on the port (inline, then
//! `ActiveJob.enqueue_without_running` and `perform_held`), and must print
//! the same: the chain order across the concern, the hook and the body,
//! halting, the around ordering, `perform_now`'s return value and handled
//! exception, the cause chain, the re-raise, and an enqueue a callback
//! refuses. Spinel refuses the include by name.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::det::HashMap;
use roundhouse::ingest::ingest_app_from_tree;
use roundhouse::project::BuildTarget;
use std::path::PathBuf;
use std::process::Command;
/// The app's job base, with the trail every observation reads.
const APPLICATION_JOB: &str = r#"class ApplicationJob < ActiveJob::Base
  TRAIL = []

  def trail
    TRAIL
  end
end
"#;
/// A Concern whose `included` block registers perform and enqueue
/// callbacks and a handler, as BackgroundQueue::Job's concerns do.
const TRACED: &str = r#"module Traced
  extend ActiveSupport::Concern

  included do
    before_perform { |job| job.trail << "traced before #{job.arguments.first}" }
    around_perform do |job, block|
      trail << "traced around in"
      block.call
      trail << "traced around out"
    end
    before_enqueue :note_enqueue
    rescue_from(KeyError) { |e| trail << "traced rescued #{e.message}" }
  end

  def note_enqueue
    trail << "traced before_enqueue"
  end
end
"#;
/// A Concern that carries a class attribute and an accessor onto the job.
const STAMPED: &str = r#"module Stamped
  extend ActiveSupport::Concern

  included do
    class_attribute :stamp, default: "plain"
    attr_accessor :note
  end
end
"#;
/// The BackgroundQueue::WithFallback shape: a plain module whose
/// `self.included(base)` registers an `around_enqueue` on the includer.
const FALLBACK: &str = r#"module Fallback
  class << self
    def included(base)
      base.around_enqueue do |job, block|
        job.trail << "fallback in"
        block.call
        job.trail << "fallback out"
      end
    end
  end
end
"#;
/// A job that includes all three and registers the rest of the surface
/// in its own body: `prepend:`, a symbol around, a halting
/// `before_enqueue`, `after_enqueue`, `rescue_from ..., with:` and by
/// class name.
const REPORT_JOB: &str = r#"class ReportJob < ApplicationJob
  include Traced
  include Stamped
  include Fallback

  self.stamp = "report"

  before_perform :own_before
  before_perform(prepend: true) { |job| job.trail << "prepended" }
  after_perform { |job| job.trail << "after #{job.arguments.inspect}" }
  around_perform :own_around
  before_enqueue do |job|
    trail << "before_enqueue #{job.arguments.first}"
    throw :abort if job.arguments.first == :refuse
  end
  after_enqueue { trail << "after_enqueue" }
  rescue_from ArgumentError, with: :handle_argument
  rescue_from "RangeError" do |e|
    trail << "range #{e.message}"
  end

  def perform(kind, count = 1)
    trail << "perform #{kind} #{count}"
    case kind
    when :argument then raise ArgumentError, "bad"
    when :range then raise RangeError, "far"
    when :key then raise KeyError, "key"
    when :wrapped
      begin
        raise KeyError, "inner"
      rescue KeyError
        raise IOError, "outer"
      end
    when :io then raise IOError, "io"
    end
    "done #{kind}"
  end

  private

  def own_before
    trail << "own before"
  end

  def own_around
    trail << "own around in"
    yield
    trail << "own around out"
  end

  def handle_argument(error)
    trail << "handled #{error.message}"
  end
end
"#;
/// A subclass that skips a callback and halts in a `before_perform`.
const HALTED_JOB: &str = r#"class HaltedJob < ReportJob
  skip_callback :perform, :before, :own_before
  before_perform { |job| throw :abort if job.arguments.first == :halt }
end
"#;
/// Core's ShopEvent::TrackJob shape: a namespaced job whose superclass is
/// written bare (`< TrackJob`, bound in `module ShopEvent`), and whose
/// subclass includes a callback concern.
const TRACK_JOB: &str = "module ShopEvent\n  class TrackJob < ApplicationJob\n    def perform(kind)\n      trail << \"track #{kind}\"\n    end\n  end\nend\n";

const TRACK_CREATE_JOB: &str =
    "module ShopEvent\n  class TrackCreateJob < TrackJob\n    include Traced\n  end\nend\n";

/// A job outside the hierarchy, with no callbacks: it keeps the
/// generated wrappers.
const PLAIN_JOB: &str = r#"class PlainJob < ActiveJob::Base
  def perform(value)
    value
  end
end
"#;
/// The observations, run after the files load; `hold_jobs`, `held_count`
/// and `run_held` are each side's test adapter.
const OBSERVE: &str = r#"def run(label)
  ApplicationJob::TRAIL.clear
  result = begin
    yield
  rescue Exception => e
    [:raised, e.class.name, e.message]
  end
  result = result.class.name if result.is_a?(ActiveJob::Base)
  result = [result.class.name, result.message] if result.is_a?(Exception)
  p [label, result, ApplicationJob::TRAIL.dup]
end
run(:now) { ReportJob.perform_now(:ok, 2) }
run(:argument) { ReportJob.perform_now(:argument) }
run(:range) { ReportJob.perform_now(:range) }
run(:key) { ReportJob.perform_now(:key) }
run(:wrapped) { ReportJob.perform_now(:wrapped) }
run(:io) { ReportJob.perform_now(:io) }
run(:later) { ReportJob.perform_later(:ok) }
run(:refused) { ReportJob.perform_later(:refuse) }
run(:halted) { HaltedJob.perform_now(:halt) }
run(:halted_ok) { HaltedJob.perform_now(:ok) }
run(:halted_later) { HaltedJob.perform_later(:halt) }
run(:nested) { ShopEvent::TrackCreateJob.perform_now(:create) }
run(:nested_parent) { ShopEvent::TrackJob.perform_later(:plain) }
run(:instance) { job = ReportJob.new(:ok); [job.perform_now, job.executions, job.arguments] }
p [ReportJob.stamp, HaltedJob.stamp, ReportJob.new.stamp, ApplicationJob.respond_to?(:stamp)]
p [ReportJob._perform_callbacks.map(&:kind), HaltedJob._perform_callbacks.count, ReportJob._enqueue_callbacks.map(&:kind).last(4)]
p ReportJob.rescue_handlers.map(&:first)
p PlainJob.perform_now(5)
hold_jobs
run(:held) { ReportJob.perform_later(:ok, 3) }
run(:held_refused) { ReportJob.perform_later(:refuse) }
p held_count
run(:drained) { run_held }
p held_count
"#;
/// The native `:test` adapter: what `queue_adapter.enqueued_jobs` holds,
/// run as the adapter's `perform_enqueued_jobs` runs them.
const NATIVE_ADAPTER: &str = r#"def hold_jobs
  ActiveJob::Base.queue_adapter = :test
end

def held_count
  ActiveJob::Base.queue_adapter.enqueued_jobs.size
end

def run_held
  jobs = ActiveJob::Base.queue_adapter.enqueued_jobs.dup
  ActiveJob::Base.queue_adapter.enqueued_jobs.clear
  jobs.each { |job| ActiveJob::Base.execute(job) }
  jobs.size
end
"#;

/// The shared runtime's: the emitted test harness's posture
/// (tests/active_job_enqueue_log.rs).
const EMITTED_ADAPTER: &str = r#"def hold_jobs
  ActiveJob.enqueue_without_running
end

def held_count
  ActiveJob::HELD.length
end

def run_held
  ActiveJob.perform_held([])
end
"#;

const FILES: &[(&str, &str, &str)] = &[
    (
        "app/jobs/application_job.rb",
        "app/models/application_job.rb",
        APPLICATION_JOB,
    ),
    (
        "app/jobs/concerns/traced.rb",
        "app/models/traced.rb",
        TRACED,
    ),
    (
        "app/jobs/concerns/stamped.rb",
        "app/models/stamped.rb",
        STAMPED,
    ),
    ("app/jobs/fallback.rb", "app/models/fallback.rb", FALLBACK),
    (
        "app/jobs/report_job.rb",
        "app/models/report_job.rb",
        REPORT_JOB,
    ),
    (
        "app/jobs/halted_job.rb",
        "app/models/halted_job.rb",
        HALTED_JOB,
    ),
    (
        "app/jobs/shop_event/track_job.rb",
        "app/models/shop_event/track_job.rb",
        TRACK_JOB,
    ),
    (
        "app/jobs/shop_event/track_create_job.rb",
        "app/models/shop_event/track_create_job.rb",
        TRACK_CREATE_JOB,
    ),
    (
        "app/jobs/plain_job.rb",
        "app/models/plain_job.rb",
        PLAIN_JOB,
    ),
];

fn app() -> emit_and_run::Overlay {
    let mut app = emit_and_run::empty_app().write("roundhouse.yml", "test_paths:\n  - test\n");
    for (path, _, text) in FILES {
        app = app.write(path, text);
    }
    app
}

fn requires() -> String {
    FILES
        .iter()
        .map(|(_, emitted, _)| format!("require_relative {:?}\n", emitted.trim_end_matches(".rb")))
        .collect()
}

fn native() -> String {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let dir = std::env::temp_dir().join(format!(
        "rh_active_job_callbacks_native_{}",
        std::process::id()
    ));
    // The emitted tree's layout, so the one `requires()` serves both.
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
    assert!(
        out.status.success(),
        "native run failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::remove_dir_all(&dir).unwrap();
    stdout
}

const EXPECTED_START: &str = r#"[:now, "done ok", ["prepended", "traced before ok", "traced around in", "own before", "own around in", "perform ok 2", "own around out", "after [:ok, 2]", "traced around out"]]
[:argument, ["ArgumentError", "bad"], ["prepended", "traced before argument", "traced around in", "own before", "own around in", "perform argument 1", "handled bad"]]
"#;

#[test]
fn job_callbacks_and_handlers_run_like_activejob() {
    let (dir, errors) = app().emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    for runtime in [
        "active_job_callbacks.rb",
        "active_support_rescuable.rb",
        "active_support_callbacks.rb",
    ] {
        assert!(dir.join("runtime").join(runtime).exists(), "{runtime}");
    }
    let facades =
        std::fs::read_to_string(dir.join("runtime/gem_facades.rb")).expect("gem_facades.rb");
    assert!(
        facades.contains("require_relative \"active_job_callbacks\""),
        "{facades}"
    );
    for (_, file, _) in FILES {
        let text =
            std::fs::read_to_string(dir.join(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
        assert!(!text.contains("roundhouse:"), "{file}: {text}");
    }
    let base = std::fs::read_to_string(dir.join("app/models/application_job.rb")).unwrap();
    assert!(base.contains("include ActiveJob::Callbacks"), "{base}");
    let report = std::fs::read_to_string(dir.join("app/models/report_job.rb")).unwrap();
    // The concern's callbacks at its include, the body's after it.
    let at = |needle: &str| {
        report
            .find(needle)
            .unwrap_or_else(|| panic!("{needle}: {report}"))
    };
    assert!(
        at("include Traced") < at("before_enqueue :note_enqueue"),
        "{report}"
    );
    assert!(
        at("before_enqueue :note_enqueue") < at("include Stamped"),
        "{report}"
    );
    assert!(
        at("include Fallback") < at("before_perform :own_before"),
        "{report}"
    );
    assert!(!report.contains("def self.perform_later"), "{report}");
    let plain = std::fs::read_to_string(dir.join("app/models/plain_job.rb")).unwrap();
    assert!(
        plain.contains("def self.perform_later(value)")
            && plain.contains("def self.perform_now(value)"),
        "{plain}"
    );

    let script = format!(
        "require File.expand_path('main', Dir.pwd)\n{}{EMITTED_ADAPTER}{OBSERVE}",
        requires()
    );
    let out = emit_and_run::ruby()
        .current_dir(&*dir)
        .args(["-W0", "-e", &script])
        .env("BLOG_DB", ":memory:")
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.starts_with(EXPECTED_START), "{stdout}");
    assert_eq!(stdout, native(), "emitted on the port vs native activejob");

    // The enqueue log the test helpers read counts the enqueue the chain
    // completes, not the one it refuses.
    let log = format!(
        "require File.expand_path('main', Dir.pwd)\n{}ActiveJob.enqueue_without_running\n\
         before = ActiveJob.performed.length\nReportJob.perform_later(:ok)\nReportJob.perform_later(:refuse)\nPlainJob.perform_later(1)\n\
         p ActiveJob.performed.drop(before), ActiveJob.perform_held([\"ReportJob\"]), ApplicationJob::TRAIL.include?(\"perform ok 1\")\n",
        requires()
    );
    let out = emit_and_run::ruby()
        .current_dir(&*dir)
        .args(["-W0", "-e", &log])
        .env("BLOG_DB", ":memory:")
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(stdout, "[\"ReportJob\", \"PlainJob\"]\n1\ntrue\n");
}

/// Spinel has no port and refuses the include by name.
#[test]
fn spinel_refuses_the_callbacks_include() {
    let (dir, errors) = app().emit(BuildTarget::Spinel);
    assert!(!dir.join("runtime/active_job_callbacks.rb").exists());
    assert!(!dir.join("runtime/active_support_rescuable.rb").exists());
    let expected = "ApplicationJob includes ActiveJob::Callbacks, declared by the Ruby-family runtime (runtime/active_job_callbacks.rb), which spinel does not emit";
    assert!(errors.iter().any(|e| e.contains(expected)), "{errors:#?}");
}

/// An app whose jobs register no callback carries no port and keeps the
/// generated wrappers.
#[test]
fn jobs_without_callbacks_keep_their_wrappers() {
    let (dir, errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("app/jobs/plain_job.rb", PLAIN_JOB)
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    assert!(!dir.join("runtime/active_job_callbacks.rb").exists());
    let plain = std::fs::read_to_string(dir.join("app/models/plain_job.rb")).unwrap();
    assert!(
        !plain.contains("include ActiveJob::Callbacks")
            && plain.contains("def self.perform_later(value)"),
        "{plain}"
    );
}

fn tree(files: &[(&str, &str)]) -> HashMap<PathBuf, Vec<u8>> {
    files
        .iter()
        .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
        .collect()
}

const SCHEMA: &str = "ActiveRecord::Schema.define(version: 1) do\nend\n";

/// A job that includes a callback concern below a callback of its own
/// registers the concern's after its own natively; the emitted body
/// writes includes first, so it is refused rather than reordered.
#[test]
fn an_include_below_an_own_callback_is_refused() {
    let late = "class LateJob < ApplicationJob\n  before_perform { trail << \"late own\" }\n  include Traced\n\n  def perform\n    trail << \"late perform\"\n  end\nend\n";
    let error = ingest_app_from_tree(tree(&[
        ("db/schema.rb", SCHEMA),
        ("app/jobs/application_job.rb", APPLICATION_JOB),
        ("app/jobs/concerns/traced.rb", TRACED),
        ("app/jobs/late_job.rb", late),
    ]))
    .err()
    .expect("the late include is refused")
    .to_string();
    assert!(
        error.contains("`before_perform` in `included do` of Traced: LateJob includes Traced below a callback its own body registers (line 2)"),
        "{error}"
    );
}

/// The WithFallback hook is consumed for jobs only: a plain class that
/// includes it beside a class-attribute concern is still refused.
#[test]
fn the_registration_hook_is_consumed_for_jobs_only() {
    let widget = "class Widget\n  include Stamped\n  include Fallback\nend\n";
    let error = ingest_app_from_tree(tree(&[
        ("db/schema.rb", SCHEMA),
        ("app/jobs/concerns/stamped.rb", STAMPED),
        ("app/jobs/fallback.rb", FALLBACK),
        ("app/models/widget.rb", widget),
    ]))
    .err()
    .expect("the hook is not consumed for a plain class")
    .to_string();
    assert!(
        error.contains("cannot be carried alongside an unconsumed included hook"),
        "{error}"
    );
}
