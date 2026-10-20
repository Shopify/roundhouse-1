//! Statements Core relies on that the emitted program used to lose:
//! a Concern's `included do … end` callback registration with its nested
//! class, a class-body self-writer (`self.instances = []`), and a
//! file-level `Target.prepend(Mod)`.
//!
//! Core's `Podding::JobShopLock` registers `Callback.new` as an around
//! `:execution` callback from its `included` block, which sets
//! `Shop.current` for `perform_now`; `MethodTimingRecorder` seeds
//! `self.instances = []` after a `class << self; attr_accessor`;
//! shop_identity's patch file ends with
//! `ActiveRecord::Relation.prepend(FindEachWithShopSelection)`. The
//! emitted tree must print what CRuby (on Core's activejob) prints.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

/// BackgroundQueue::Job's shape: an `:execution` before callback that
/// resets the request state sits above the include, so it runs before the
/// lock's around callback sets the shop. An enqueue callback above it
/// calls the job's own writer.
const APPLICATION_JOB: &str = r#"class ApplicationJob < ActiveJob::Base
  include ExecutionCallbacks

  after_enqueue do
    self.enqueued = true
  end

  set_callback :execution, :before do
    Thread.current[:lock_shop] = nil
  end

  include Podding::JobShopLock

  attr_accessor :enqueued
end
"#;

/// BackgroundQueue::ExecutionCallbacks: the `:execution` chain the lock
/// hangs off, run around `perform_now`.
const EXECUTION_CALLBACKS: &str = r#"module ExecutionCallbacks
  extend ActiveSupport::Concern

  included do
    define_callbacks :execution, skip_after_callbacks_if_terminated: true
  end

  def perform_now
    run_callbacks(:execution) do
      super
    end
  end
end
"#;

const LOCK: &str = r#"module Podding
  module JobShopLock
    extend ActiveSupport::Concern

    included do
      class_attribute :unscoped_shop, default: false

      set_callback(:execution, :around, Callback.new)
    end

    #: () -> String
    def shop_from_job_params
      "shop-#{Thread.current[:lock_shop]}"
    end

    class Callback
      #: (untyped) { () -> untyped } -> untyped
      def around(job, &block)
        Thread.current[:lock_shop] = job.class.name
        yield
      ensure
        Thread.current[:lock_shop] = nil
      end
    end
  end
end
"#;

const JOB: &str = r#"class LockedJob < ApplicationJob
  #: () -> String
  def perform
    shop_from_job_params
  end
end
"#;

/// Core reopens `Module` (config/boot_instrumentation.rb), so the app
/// has a `Module` class of its own for a `< Module` base to resolve to.
const MODULE_REOPEN: &str = r#"class Module
  #: () -> String
  def tagged = "tagged"
end
"#;

const RECORDER: &str = r#"class Recorder < Module
  class << self
    attr_accessor :instances
  end

  self.instances = []

  #: (String) -> void
  def initialize(name)
    super()
    self.class.instances.push(name)
  end
end
"#;

const TARGET: &str = r#"class Target
  #: () -> Array[Integer]
  def each_item = [1, 2]
end
"#;

/// shop_identity's shape: the module body ends with a prepend of the
/// module it just defined, on a class outside it.
const PATCH: &str = r#"module Wrapping
  module Patch
    #: () -> Array[Integer]
    def each_item = super.map { |x| x * 10 }
  end

  Target.prepend(Patch)
end
"#;

/// The same, on a runtime class (`ActiveRecord::Relation`).
const RELATION_PATCH: &str = r#"module ActiveRecord
  module Marked
    #: () -> bool
    def marked? = true
  end

  ActiveRecord::Relation.prepend(Marked)
end
"#;

/// Another file of the app reopens `ActiveRecord::Relation` (Core's
/// podding/patches/active_record.rb), which makes it a class the app
/// defines: the prepend above still has to run.
const RELATION_REOPEN: &str = r#"module ActiveRecord
  class Relation
    #: () -> Integer
    def reopened = 1
  end
end
"#;

const SCRIPT: &str = r#"
puts LockedJob.perform_now.inspect
puts LockedJob.unscoped_shop.inspect
puts Thread.current[:lock_shop].inspect
Recorder.new("a")
puts Recorder.instances.inspect
puts Target.new.each_item.inspect
puts ActiveRecord::Relation.ancestors.include?(ActiveRecord::Marked).inspect
"#;

const EXPECTED: [&str; 6] = ["\"shop-LockedJob\"", "false", "nil", "[\"a\"]", "[10, 20]", "true"];

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("app/jobs/application_job.rb", APPLICATION_JOB)
        .write("app/models/execution_callbacks.rb", EXECUTION_CALLBACKS)
        .write("app/models/podding/job_shop_lock.rb", LOCK)
        .write("app/jobs/locked_job.rb", JOB)
        .write("app/models/module.rb", MODULE_REOPEN)
        .write("app/models/recorder.rb", RECORDER)
        .write("app/models/target.rb", TARGET)
        .write("lib/patches/wrapping_patch.rb", PATCH)
        .write("lib/shop_identity/patches/active_record.rb", RELATION_PATCH)
        .write("lib/podding/patches/active_record.rb", RELATION_REOPEN)
}

/// The program on Core's pinned activejob (`:inline`).
fn native() -> Vec<String> {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let program = format!(
        "require {oracle:?}\nRailsOracle.activate(\"activesupport\", \"activerecord\", \"activejob\")\n\
         require \"active_support\"\nrequire \"active_record\"\nrequire \"active_job\"\nrequire \"logger\"\n\
         ActiveJob::Base.queue_adapter = :inline\nActiveJob::Base.logger = Logger.new(nil)\n\
         {EXECUTION_CALLBACKS}\n{LOCK}\n{APPLICATION_JOB}\n{JOB}\n{MODULE_REOPEN}\n{RECORDER}\n{TARGET}\n{PATCH}\n{RELATION_PATCH}\n{RELATION_REOPEN}\n{SCRIPT}"
    );
    let dir = std::env::temp_dir().join(format!("rh_dropped_statements_native_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("native.rb"), program).unwrap();
    let out = Command::new("ruby")
        .args(["-W0", "native.rb"])
        .current_dir(&dir)
        .env("GLIBC_TUNABLES", "glibc.rtld.optional_static_tls=262144")
        .output()
        .expect("ruby is on PATH");
    assert!(out.status.success(), "native run failed:\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect()
}

#[test]
fn included_blocks_class_body_writers_and_file_prepends_run_as_in_cruby() {
    assert_eq!(native(), EXPECTED, "native control");

    let run = app().run_ruby(SCRIPT);
    run.assert_passes();
    let lines: Vec<String> = run.stdout.lines().map(str::to_string).collect();
    assert_eq!(lines, EXPECTED, "emitted Ruby diverges from CRuby\n{}", run.emitted.display());
}
