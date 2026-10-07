//! A namespace a gem already defined (observe-instrument's `Observe`),
//! reopened by the app's own file whose body runs code at load.
//!
//! Core's lib/observe.rb seeds `@chronicle_event_subscriber =
//! ChronicleEventSubscriber.new(@resource)` in its module body, and that
//! constructor reads `JSONEncoder`, a sibling file nothing required yet.
//! Zeitwerk sets up an already-defined namespace's child autoloads when
//! the loader is set up, so the read autoloads it. The emitted
//! `app/models.rb` registered the children only after requiring the
//! namespace's file, and the body met `uninitialized constant
//! Observe::JSONEncoder`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const PROBE: &str = r##"module Probe
  @sub = Probe::Sub.new

  def self.sub
    @sub
  end
end
"##;

const SUB: &str = r##"module Probe
  class Sub
    def initialize
      @encoder = Encoder.new
    end

    def encoder
      @encoder
    end
  end
end
"##;

const ENCODER: &str = r##"module Probe
  class Encoder
    def kind
      "encoder"
    end
  end
end
"##;

#[test]
fn a_predefined_namespace_s_children_autoload_while_its_file_runs() {
    let (dir, _) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("app/models/probe.rb", PROBE)
        .write("app/models/probe/sub.rb", SUB)
        .write("app/models/probe/encoder.rb", ENCODER)
        .emit(BuildTarget::Ruby);
    // The gem's namespace exists before the app's code loads.
    let script = "module Probe; end\n\
                  require File.expand_path(\"main\", Dir.pwd)\n\
                  Main.configure_default_adapter!\n\
                  p [Probe.sub.class.name, Probe.sub.encoder.kind]\n";
    let out = emit_and_run::ruby()
        .args(["-e", script])
        .current_dir(&*dir)
        .env("BLOG_DB", ":memory:")
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stdout, "[\"Probe::Sub\", \"encoder\"]\n");
}

const REAPER: &str = r##"module Reaper
  DEFAULT_COLUMN = :deleted_at
end
"##;

const REAPER_MODEL: &str = r##"module Reaper
  module Model
    def self.column
      DEFAULT_COLUMN
    end
  end
end
"##;

const REAPER_CLASS_METHODS: &str = r##"module Reaper
  module Model
    module ClassMethods
    end
  end
end
"##;

/// Core's platform/reaper/model.rb, required before platform/reaper.rb:
/// its `module Reaper` line autoloads the parent, whose definition
/// registers `Reaper::Model`'s autoload to the file loading now. The
/// registration's `const_added` resolved the child (a namespace, with
/// `ClassMethods` under it) and met `uninitialized constant
/// Reaper::Model`.
#[test]
fn a_child_file_required_before_its_parent_s_file_loads() {
    let run = emit_and_run::empty_app()
        // Lazy, as Core's local test boot is (`run_ruby` boots development).
        .write("config/environments/development.rb", "Rails.application.configure do\n  config.eager_load = false\nend\n")
        .write("app/models/reaper.rb", REAPER)
        .write("app/models/reaper/model.rb", REAPER_MODEL)
        .write("app/models/reaper/model/class_methods.rb", REAPER_CLASS_METHODS)
        .run_ruby(
            "require File.expand_path(\"app/models/reaper/model\", Dir.pwd)\n\
             p [Reaper::Model.name, Reaper::Model.column, Reaper::Model::ClassMethods.name]\n",
        );
    run.assert_passes();
    assert_eq!(run.stdout, "[\"Reaper::Model\", :deleted_at, \"Reaper::Model::ClassMethods\"]\n");
}
