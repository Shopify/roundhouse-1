//! A Sorbet-typed Concern's `included do` runs on a plain job includer.
//!
//! Core's BackgroundQueue::Retry is `extend(ActiveSupport::Concern)`,
//! `extend(T::Helpers)` and `requires_ancestor { ... }`; its `included do`
//! declares `class_attribute :retry_limit, instance_accessor: false`,
//! seeds one, and registers a `before_enqueue`; `ClassMethods#retries`
//! writes them. A vendored gem (elasticsearch-shopify's
//! client_interface.rb) carries the fallback a gem writes for when
//! sorbet-runtime is absent: `unless defined?(T::Helpers); module T;
//! module Helpers; def interface!; end; end; end; end`. That reopening
//! counted as a rebinding of T::Helpers, so every Concern in the app was
//! unverified and no `included do` ran on a plain includer: the job's
//! class body then raised `undefined method 'retry_limit='`.
//!
//! A reopening whose body only (re)defines sorbet-runtime's own
//! declarations (`interface!`, `abstract!`, `requires_ancestor`, `sig`,
//! ...) leaves the Concern protocol as it is, so the block runs; one that
//! defines anything else still blocks verification, by name.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use std::process::Command;

const APPLICATION_JOB: &str = r#"class ApplicationJob < ActiveJob::Base
  TRAIL = []
end
"#;

/// elasticsearch-shopify's fallback, as the gem writes it.
const SORBET_FALLBACK: &str = r#"unless defined?(T::Helpers)
  module T
    module Helpers
      def interface!; end
    end
  end
end
"#;

const RETRYING: &str = r##"module Retrying
  extend(ActiveSupport::Concern)
  extend(T::Helpers)

  requires_ancestor { ApplicationJob }

  included do
    class_attribute :retry_limit, instance_accessor: false
    class_attribute :n, instance_accessor: false
    self.n = 1
    attr_accessor :attempt

    before_enqueue do |job|
      ApplicationJob::TRAIL << "before_enqueue #{job.class.name} limit=#{job.class.retry_limit} n=#{job.class.n}"
    end
  end

  module ClassMethods
    extend(T::Helpers)

    requires_ancestor { Module }

    def retries(limit)
      self.retry_limit = limit
      self.n = 3
    end
  end
end
"##;

const RETRIED_JOB: &str = r##"class RetriedJob < ApplicationJob
  include Retrying

  retries 2

  def perform(x)
    TRAIL << "perform #{x}"
    x * 2
  end
end
"##;

const PLAIN_JOB: &str = r#"class PlainJob < ApplicationJob
  include Retrying

  def perform
    TRAIL << "plain"
  end
end
"#;

const OBSERVE: &str = r##"p [RetriedJob.retry_limit, RetriedJob.n, PlainJob.retry_limit, PlainJob.n, ApplicationJob.respond_to?(:n)]
job = RetriedJob.new(4)
p [job.respond_to?(:retry_limit), job.respond_to?(:n), job.attempt]
job.attempt = 2
p job.attempt
RetriedJob.perform_later(3)
PlainJob.perform_later
p ApplicationJob::TRAIL
"##;

const FILES: &[(&str, &str, &str)] = &[
    ("app/jobs/application_job.rb", "app/models/application_job.rb", APPLICATION_JOB),
    ("app/models/sorbet_fallback.rb", "app/models/sorbet_fallback.rb", SORBET_FALLBACK),
    ("app/jobs/concerns/retrying.rb", "app/models/retrying.rb", RETRYING),
    ("app/jobs/retried_job.rb", "app/models/retried_job.rb", RETRIED_JOB),
    ("app/jobs/plain_job.rb", "app/models/plain_job.rb", PLAIN_JOB),
];

/// The `require_relative`s of the files under `root`. The emitted tree
/// has no sorbet_fallback.rb: the emitter drops the sorbet-runtime
/// surface (`extend T::Helpers`, `requires_ancestor`, `interface!`) with
/// the declarations the fallback stands in for.
fn requires(root: Option<&std::path::Path>) -> String {
    FILES
        .iter()
        .filter(|(_, emitted, _)| root.is_none_or(|root| root.join(emitted).exists()))
        .map(|(_, emitted, _)| format!("require_relative {:?}\n", emitted.trim_end_matches(".rb")))
        .collect()
}

/// `program` run natively on Core's pinned activejob (`:inline`) with
/// sorbet-runtime loaded, as Core boots, so the fallback is skipped.
fn native() -> String {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let dir = std::env::temp_dir().join(format!("rh_plain_includer_native_{}", std::process::id()));
    for (_, emitted, text) in FILES {
        let file = dir.join(emitted);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let program = format!(
        "require {oracle:?}\nRailsOracle.activate(\"activesupport\", \"activejob\")\n\
         require \"sorbet-runtime\"\nrequire \"active_support\"\nrequire \"active_job\"\nrequire \"logger\"\n\
         abort \"native: activejob is not Core's\" unless Gem.loaded_specs[\"activejob\"].full_gem_path.start_with?(RailsOracle::CORE_GEM_HOME)\n\
         ActiveJob::Base.queue_adapter = :inline\nActiveJob::Base.logger = Logger.new(nil)\n\
         {}{OBSERVE}",
        requires(None)
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

fn app(files: &[(&str, &str, &str)]) -> emit_and_run::Overlay {
    let mut app = emit_and_run::empty_app().write("roundhouse.yml", "test_paths:\n  - test\n");
    for (path, _, text) in files {
        app = app.write(path, text);
    }
    app
}

#[test]
fn a_sorbet_fallback_leaves_the_included_block_running_on_a_plain_job() {
    let (dir, errors) = app(FILES).emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let expected = native();
    assert_eq!(
        expected,
        r#"[2, 3, nil, 1, false]
[false, false, nil]
2
["before_enqueue RetriedJob limit=2 n=3", "perform 3", "before_enqueue PlainJob limit= n=1", "plain"]
"#,
        "native control"
    );
    let job = std::fs::read_to_string(dir.join("app/models/retried_job.rb")).unwrap();
    let at = |needle: &str| job.find(needle).unwrap_or_else(|| panic!("missing `{needle}` in:\n{job}"));
    assert!(at("include Retrying") < at("def self.n=(value)") && at("def self.n=(value)") < at("retries 2"), "{job}");
    let script = format!("require File.expand_path('main', Dir.pwd)\n{}{OBSERVE}", requires(Some(dir.as_path())));
    let out = emit_and_run::ruby()
        .current_dir(&*dir)
        .args(["-W0", "-e", &script])
        .env("BLOG_DB", ":memory:")
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "emitted run failed in {}:\n{stdout}\n{}\n{job}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr),
    );
    assert_eq!(stdout, expected, "emitted on the port vs native activejob");
}

/// A T::Helpers reopening that defines a method sorbet-runtime does not
/// declare could be a hook `extend` runs, so every Concern stays
/// unverified, and its plain includers say so by name instead of
/// running without the block.
#[test]
fn a_t_helpers_reopening_with_another_method_stays_a_barrier() {
    let rebinding = "module T\n  module Helpers\n    def interface!; end\n\n    def extended(base)\n      base\n    end\n  end\nend\n";
    let files: Vec<(&str, &str, &str)> = FILES
        .iter()
        .map(|&(path, emitted, text)| if path.ends_with("sorbet_fallback.rb") { (path, emitted, rebinding) } else { (path, emitted, text) })
        .collect();
    let tree = files
        .iter()
        .map(|(path, _, text)| (std::path::PathBuf::from(path), text.as_bytes().to_vec()))
        .chain([(std::path::PathBuf::from("roundhouse.yml"), b"test_paths:\n  - test\n".to_vec())])
        .collect();
    let error = roundhouse::ingest::ingest_app_from_tree(tree).expect_err("the barrier is refused at ingest");
    let message = format!("{error:?}");
    assert!(
        message.contains("`included do` of Retrying is not run on a plain (non-model) includer")
            && message.contains("rebinds ActiveSupport::Concern, T::Sig or T::Helpers")
            && message.contains("PlainJob, RetriedJob would run without it"),
        "{message}"
    );
}
