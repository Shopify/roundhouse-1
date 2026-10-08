//! A job's class-body macro writes a class attribute that a Concern's
//! `included do` declares, as Core's `retries` does.
//!
//! BackgroundQueue::Retry's `included do` declares `class_attribute
//! :retry_limit` and `:backoff_strategy` (with a default) and seeds
//! `final_failure_procs = []`; its `ClassMethods#retries` writes them and
//! `on_final_failure` appends to one. A job body says `include
//! BackgroundQueue::Retry` and then `retries 3, backoff: :exponential`.
//! Ruby runs the include, and with it the `included do` block, before the
//! macro, so the macro writes over the declared defaults. The emitted
//! body must do the same: the carried declarations and their seeds at the
//! include, the replayed macro after them, and a subclass inheriting the
//! value until its own macro overrides it. `Concern#append_features`
//! extends `ClassMethods` before it runs the block, so a seed may call
//! one of them (`self.jitter = default_jitter`).
//!
//! The jobs run natively on Core's pinned activejob
//! (tests/support/rails_oracle.rb) and emitted on the runtime port, and
//! must print the same.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use std::process::Command;

const APPLICATION_JOB: &str = r#"class ApplicationJob < ActiveJob::Base
  TRAIL = []

  def trail
    TRAIL
  end
end
"#;

const RETRIED: &str = r##"module Retried
  extend ActiveSupport::Concern

  BACKOFF = [0, 60, 120].freeze

  included do
    class_attribute :final_failure_procs
    self.final_failure_procs = []
    class_attribute :retry_limit, instance_accessor: false
    class_attribute :backoff_strategy, default: BACKOFF, instance_accessor: false
    class_attribute :jitter, instance_accessor: false
    self.jitter = default_jitter
    attr_accessor :attempt

    before_enqueue do |job|
      raise ArgumentError, "#{job.class} must define retries" unless job.class.sane_retry_strategy
    end
  end

  module ClassMethods
    def default_jitter
      0.15
    end

    def sane_retry_strategy
      !retry_limit.nil?
    end

    def retries(limit, backoff: nil)
      backoff ||= :exponential
      self.retry_limit = limit
      self.backoff_strategy = case backoff
      when :exponential then BACKOFF
      when :no_delay then [0] * limit
      else backoff
      end
    end

    def on_final_failure(&blk)
      self.final_failure_procs += [blk]
    end
  end
end
"##;

const RETRIED_JOB: &str = r##"class RetriedJob < ApplicationJob
  include Retried

  retries 2, backoff: :no_delay
  on_final_failure { |job, error| job.trail << "final #{error}" }

  def perform(x)
    trail << "perform #{x}"
    x * 2
  end
end
"##;

const UNSET_JOB: &str = r#"class UnsetJob < ApplicationJob
  include Retried

  def perform
    :ran
  end
end
"#;

const SUBCLASS_JOB: &str = r#"class SubclassJob < RetriedJob
  retries 5, backoff: [1, 2, 3, 4, 5]
end
"#;

const OBSERVE: &str = r##"p [RetriedJob.retry_limit, RetriedJob.backoff_strategy, RetriedJob.final_failure_procs.size, RetriedJob.sane_retry_strategy, RetriedJob.jitter, SubclassJob.jitter]
p [UnsetJob.retry_limit, UnsetJob.backoff_strategy, UnsetJob.final_failure_procs.size, UnsetJob.sane_retry_strategy]
p [SubclassJob.retry_limit, SubclassJob.backoff_strategy, SubclassJob.final_failure_procs.size, SubclassJob.sane_retry_strategy]
job = RetriedJob.new(4)
p [job.final_failure_procs.size, job.respond_to?(:retry_limit), job.attempt]
job.attempt = 1
p job.attempt
RetriedJob.final_failure_procs.first.call(job, "boom")
p [RetriedJob.perform_now(3), ApplicationJob::TRAIL]
begin
  UnsetJob.perform_later
  p :enqueued
rescue ArgumentError => e
  p e.message
end
"##;

const FILES: &[(&str, &str, &str)] = &[
    (
        "app/jobs/application_job.rb",
        "app/models/application_job.rb",
        APPLICATION_JOB,
    ),
    (
        "app/jobs/concerns/retried.rb",
        "app/models/retried.rb",
        RETRIED,
    ),
    (
        "app/jobs/retried_job.rb",
        "app/models/retried_job.rb",
        RETRIED_JOB,
    ),
    (
        "app/jobs/unset_job.rb",
        "app/models/unset_job.rb",
        UNSET_JOB,
    ),
    (
        "app/jobs/subclass_job.rb",
        "app/models/subclass_job.rb",
        SUBCLASS_JOB,
    ),
];

/// `program` run natively on Core's pinned activejob (`:inline`), beside
/// the sources written at their emitted paths, so one set of
/// `require_relative`s serves both sides.
fn native(requires: &str) -> String {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let dir = std::env::temp_dir().join(format!("rh_job_retry_native_{}", std::process::id()));
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
         {requires}{OBSERVE}"
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

#[test]
fn a_job_macro_writes_the_included_class_attributes_like_activejob() {
    let mut app = emit_and_run::empty_app().write("roundhouse.yml", "test_paths:\n  - test\n");
    for (path, _, text) in FILES {
        app = app.write(path, text);
    }
    let (dir, errors) = app.emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let requires: String = FILES
        .iter()
        .map(|(_, emitted, _)| format!("require_relative {:?}\n", emitted.trim_end_matches(".rb")))
        .collect();
    let expected = native(&requires);
    assert_eq!(
        expected,
        r#"[2, [0, 0], 1, true, 0.15, 0.15]
[nil, [0, 60, 120], 0, false]
[5, [1, 2, 3, 4, 5], 1, true]
[1, false, nil]
1
[6, ["final boom", "perform 3"]]
"UnsetJob must define retries"
"#,
        "native control"
    );
    // The carried defs and seeds stand at the include, above the macro.
    let job = std::fs::read_to_string(dir.join("app/models/retried_job.rb")).unwrap();
    let at = |needle: &str| {
        job.find(needle)
            .unwrap_or_else(|| panic!("missing `{needle}` in:\n{job}"))
    };
    assert!(
        at("include Retried") < at("def self.retry_limit=(value)")
            && at("def self.retry_limit=(value)") < at("@__class_attr_retry_limit = nil")
            && at("@__class_attr_retry_limit = nil") < at("retries 2, backoff: :no_delay"),
        "{job}"
    );
    if std::env::var_os("RH_SHOW").is_some() {
        println!("{job}");
    }
    let script = format!("require File.expand_path('main', Dir.pwd)\n{requires}{OBSERVE}");
    let out = emit_and_run::ruby()
        .current_dir(&*dir)
        .args(["-W0", "-e", &script])
        .env("BLOG_DB", ":memory:")
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "emitted run failed in {}:\n{stdout}\n{}\n{}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr),
        std::fs::read_to_string(dir.join("app/models/retried_job.rb")).unwrap_or_default()
    );
    assert_eq!(stdout, expected, "emitted on the port vs native activejob");
}
