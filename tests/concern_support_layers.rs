//! A Concern's `included do` outside `app/models`: under `app/utils`,
//! `app/services` or `lib`, as Core keeps `BackgroundQueue::*` beside
//! `BackgroundQueue::Job < ActiveJob::Base`.
//!
//! Rails runs the block wherever the Concern file lives. Ingest used to
//! collect a block only for Concerns in the models and controllers layers,
//! so one elsewhere was neither run on its includer nor reported: the
//! emitted module dropped it with a warning. The block now runs on each
//! includer, and a statement it cannot model is an error at that
//! statement.
//!
//! Each case runs the emitted app against the same classes under native
//! Rails (the pinned oracle, tests/support/rails_oracle.rb). Fixtures are
//! written into the Overlay's scratch copy, never into `fixtures/real-blog`.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

const ORACLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");

/// Runs `program` (a file: the oracle re-executes it under jemalloc).
fn native(name: &str, program: &str) -> String {
    let dir = std::env::temp_dir().join(format!(
        "rh_concern_support_layers_{name}_{}",
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

const NATIVE_PRELUDE: &str = "RailsOracle.activate(\"activesupport\", \"activemodel\", \"activerecord\", \"activejob\")\n\
     require \"active_support\"\nrequire \"active_support/concern\"\n\
     require \"active_support/core_ext/class/attribute\"\nrequire \"active_job\"\n";

/// The jobprobe shape: a Concern in `app/utils` whose block declares a
/// class attribute and an accessor, included by an ActiveJob base class.
const LOGGED: &str = r##"module Logged
  extend ActiveSupport::Concern

  included do
    attr_accessor :log_level

    class_attribute :log_params, instance_accessor: false, default: true
  end

  def describe
    "#{self.class.name}:#{self.class.log_params}:#{log_level.inspect}"
  end
end
"##;

/// A Concern in `lib`, included by the same job.
const TRACKED: &str = r##"module Tracked
  extend ActiveSupport::Concern

  included do
    class_attribute :tracking, default: :off
  end
end
"##;

/// A Concern in `app/services`, depending on Logged (deferred to the
/// includer).
const AUDITED: &str = r##"module Audited
  extend ActiveSupport::Concern
  include Logged

  included do
    class_attribute :audit_level, default: 1
  end
end
"##;

const BASE_JOB: &str = r##"class BaseJob < ActiveJob::Base
  include Logged
  include Tracked

  def perform = nil
end
"##;
const QUIET_JOB: &str =
    "class QuietJob < BaseJob\n  self.log_params = false\n  self.tracking = :on\nend\n";
/// Already in the ancestors: the block does not run again, and the
/// accessor is BaseJob's.
const AGAIN_JOB: &str = "class AgainJob < QuietJob\n  include Logged\nend\n";
const AUDIT_JOB: &str =
    "class AuditJob < ActiveJob::Base\n  include Audited\n\n  def perform = nil\nend\n";

const SCRIPT: &str = r##"p [BaseJob.log_params, QuietJob.log_params, AgainJob.log_params, AuditJob.log_params]
p [BaseJob.tracking, QuietJob.tracking, AgainJob.tracking, AuditJob.audit_level]
job = AgainJob.new
job.log_level = :debug
p [job.describe, BaseJob.new.describe, AuditJob.new.describe]
p [BaseJob.new.respond_to?(:log_params), AgainJob.instance_method(:log_level).owner]
p AuditJob.ancestors.take_while { |m| m != ActiveJob::Base }
"##;

#[test]
fn a_concern_outside_app_models_runs_its_block_as_native() {
    let classes = [
        ("app/utils/logged.rb", LOGGED),
        ("lib/tracked.rb", TRACKED),
        ("app/services/audited.rb", AUDITED),
        ("app/utils/base_job.rb", BASE_JOB),
        ("app/jobs/quiet_job.rb", QUIET_JOB),
        ("app/jobs/again_job.rb", AGAIN_JOB),
        ("app/jobs/audit_job.rb", AUDIT_JOB),
    ];
    let program: String = classes.iter().map(|(_, source)| *source).collect();
    let expected = native("layers", &format!("{NATIVE_PRELUDE}{program}{SCRIPT}"));
    let expected: Vec<&str> = expected.lines().collect();
    assert_eq!(
        expected,
        [
            "[true, false, false, true]",
            "[:off, :on, :on, 1]",
            r#"["AgainJob:false::debug", "BaseJob:true:nil", "AuditJob:true:nil"]"#,
            "[false, BaseJob]",
            "[AuditJob, Audited, Logged]",
        ],
        "native control"
    );
    let run = classes
        .iter()
        .fold(emit_and_run::real_blog(), |overlay, (path, source)| {
            overlay.write(path, source)
        })
        .run_ruby(SCRIPT);
    run.assert_passes();
    let actual: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(
        actual, expected,
        "emitted Ruby diverges from native line for line"
    );
}

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

/// A statement the job gets nothing for (`after_discard`, which the
/// ActiveJob::Callbacks port does not carry; the callbacks it does carry
/// are tests/active_job_callbacks_port.rs) is an error at the statement,
/// naming the includer; the rest of the block is still supplied.
#[test]
fn an_unmodeled_statement_of_such_a_block_is_an_error_not_a_drop() {
    use roundhouse::ingest::{ingest_app_from_tree, survey};
    let timed = "module Timed\n  extend ActiveSupport::Concern\n\n  included do\n    class_attribute :budget, default: 5\n    after_discard :time_it\n  end\n\n  def time_it = yield\nend\n";
    let files = [
        ("app/utils/timed.rb", timed),
        (
            "app/utils/timed_job.rb",
            "class TimedJob < ActiveJob::Base\n  include Timed\nend\n",
        ),
    ];
    let error = ingest_app_from_tree(tree(&files))
        .expect_err("the statement is refused")
        .to_string();
    assert!(
        error.contains("`after_discard` in `included do` of Timed is not modeled on a plain")
            && error.contains("TimedJob"),
        "{error}"
    );
    survey::activate();
    let result = ingest_app_from_tree(tree(&files));
    let gaps: Vec<String> = survey::drain().iter().map(|g| g.to_string()).collect();
    let app = result.expect("survey keeps the app");
    assert_eq!(gaps.len(), 1, "{gaps:#?}");
    let job = app
        .library_classes
        .iter()
        .find(|lc| lc.name.0.as_str() == "TimedJob")
        .expect("TimedJob");
    assert!(
        job.methods.iter().any(|m| m.name.as_str() == "budget"),
        "the modeled part of the block is still supplied"
    );
}

// ---- `mixes_in_class_methods` on a Concern's dependency ----------------------

/// Mixed declares `mixes_in_class_methods`; Gathering depends on it. A
/// Concern defers its includes to its includer, so Mixed's sorbet
/// `included` hook runs for Host and extends it. (`Module#include` calls
/// `included` even when Concern's `append_features` defers, so Gathering
/// is extended too.)
const MIXED: &str = r##"module Mixed
  extend ActiveSupport::Concern
  extend T::Helpers

  module Definitions
    def mixed(n) = "#{name}:#{n}"
  end

  mixes_in_class_methods(Definitions)
end
"##;
const GATHERING: &str = "module Gathering\n  extend ActiveSupport::Concern\n  include Mixed\nend\n";
const HOST: &str = "class Host\n  include Gathering\nend\n";
const SUB_HOST: &str = "class SubHost < Host\nend\n";
const MIXED_SCRIPT: &str = r##"p [Host.mixed(1), SubHost.mixed(2), Article.mixed(3)]
p [Host.singleton_class.include?(Mixed::Definitions), Gathering.singleton_class.include?(Mixed::Definitions)]
p Host.ancestors.take(3)
"##;

#[test]
fn a_mixes_in_dependency_extends_the_final_includer_as_native() {
    let classes = [
        ("app/models/concerns/mixed.rb", MIXED),
        ("app/models/concerns/gathering.rb", GATHERING),
        ("app/models/host.rb", HOST),
        ("app/models/sub_host.rb", SUB_HOST),
    ];
    let program: String = classes.iter().map(|(_, source)| *source).collect();
    // A model includer too (the fixture's Article, given the include).
    const ARTICLE: &str = "class Article < ActiveRecord::Base\n  include Gathering\nend\n";
    let expected = native(
        "mixed",
        &format!(
            "{NATIVE_PRELUDE}require \"sorbet-runtime\"\nrequire \"active_record\"\n{program}{ARTICLE}{MIXED_SCRIPT}"
        ),
    );
    let expected: Vec<&str> = expected.lines().collect();
    assert_eq!(
        expected,
        [
            r#"["Host:1", "SubHost:2", "Article:3"]"#,
            "[true, true]",
            "[Host, Gathering, Mixed]"
        ],
        "native control"
    );
    let run = classes
        .iter()
        .fold(emit_and_run::real_blog(), |overlay, (path, source)| {
            overlay.write(path, source)
        })
        .edit(
            "app/models/article.rb",
            "class Article < ApplicationRecord\n",
            "class Article < ApplicationRecord\n  include Gathering\n",
        )
        .run_ruby(MIXED_SCRIPT);
    run.assert_passes();
    let actual: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(
        actual, expected,
        "emitted Ruby diverges from native line for line"
    );
}
