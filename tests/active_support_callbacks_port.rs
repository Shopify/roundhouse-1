//! activesupport's `ActiveSupport::Callbacks` (Shopify/rails 5813559f,
//! 8.2.0.alpha: callbacks.rb). Shopify Core includes it in
//! Plus::Plan::Change (components/billing), whose subclasses register
//! `before_perform` / `after_perform` blocks through `set_callback` and
//! whose `perform` runs them around `execute!` with `run_callbacks`, and
//! in the Payments::Vault AfterRevocation and AroundRedaction concerns
//! (`define_callbacks`, `set_callback ..., if:`, `run_callbacks { super }`).
//! The runtime had no such module, so each include was refused (5 check
//! rows) and a class body's `define_callbacks` was dropped.
//!
//! The CRuby and JRuby trees carry the port
//! (runtime/spinel/active_support_callbacks.rb) where app code names the
//! module, and replay the class-body calls of the class methods it mixes
//! in (`emit::ruby::library::runtime_mixin_macro_calls`). Plus::Plan::Change
//! with Core's callback surface verbatim (`define_callbacks :perform`, the
//! `before_perform` / `after_perform` macros, `perform`), a subclass that
//! uses both macros, and a class that uses the rest of the surface (around, `if:` / `unless:`,
//! `prepend:`, an object filter, a halting terminator, `skip_callback` in a
//! subclass) run natively on Core's pinned activesupport
//! (tests/support/rails_oracle.rb) and emitted on the port, and must print
//! the same, as does Core's FormBuilderField use of the DescendantsTracker
//! the port extends its includers with (its own port,
//! runtime/spinel/active_support_descendants_tracker.rb). Spinel refuses
//! the include and the extend by name.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use std::process::Command;

const CHANGE: &str = r#"module Plus
  module Plan
    class Change
      include ActiveSupport::Callbacks

      define_callbacks :perform

      class << self
        #: { [self: instance] -> void } -> void
        def before_perform(&block)
          set_callback(:perform, :before) do |instance|
            instance.instance_exec(&block)
          rescue StandardError => e
            instance.error!(e)
            raise
          end
        end

        #: { [self: instance] -> void } -> void
        def after_perform(&block)
          set_callback(:perform, :after, &block)
        end
      end

      #: -> void
      def initialize
        @executed = false #: bool
        @success = false #: bool
        @error = false #: bool
        @error_message = nil #: String?
        @events = [] #: Array[Symbol]
      end

      #: Array[Symbol]
      attr_reader :events

      #: -> self
      def perform
        return self if executed?

        run_callbacks(:perform) do
          execute!
          success!
        rescue StandardError => exception
          error!(exception)
        end

        self
      end

      #: -> bool
      def executed?
        @executed
      end

      #: -> bool
      def success?
        @success
      end

      #: -> String?
      def error_message
        @error_message
      end

      #: -> void
      def success!
        @executed = true
        @success = true
      end

      #: (StandardError) -> void
      def error!(exception)
        @executed = true
        @error = true
        @error_message = exception.message
      end
    end
  end
end
"#;

const UPGRADE: &str = r#"module Plus
  module Plan
    class Upgrade < Change
      before_perform { events << :before }
      after_perform { events << :after }

      #: -> void
      def execute!
        events << :execute
      end
    end
  end
end
"#;

const PIPELINE: &str = r#"class Pipeline
  include ActiveSupport::Callbacks

  class Audit
    def after(target)
      target.trail << :audited
    end
  end

  define_callbacks :run
  define_callbacks :save, terminator: ->(_target, result_lambda) { result_lambda.call == false }, skip_after_callbacks_if_terminated: true

  set_callback :run, :before, :first
  set_callback :run, :around, :wrap
  set_callback :run, :after, :last
  set_callback :run, :before, :guarded, if: :enabled?
  set_callback :run, :before, :never, unless: -> { true }
  set_callback :run, :before, :prepended, prepend: true
  set_callback :run, :after, Audit.new
  set_callback :save, :before, :check
  set_callback :save, :after, :saved

  def initialize(enabled)
    @enabled = enabled
    @trail = []
  end

  attr_reader :trail

  def enabled?
    @enabled
  end

  def go
    run_callbacks(:run) do
      trail << :body
      :result
    end
  end

  def save(ok)
    @ok = ok
    run_callbacks(:save) do
      trail << :saving
      true
    end
  end

  private

  def first
    trail << :first
  end

  def prepended
    trail << :prepended
  end

  def guarded
    trail << :guarded
  end

  def never
    trail << :never
  end

  def wrap
    trail << :around_in
    yield
    trail << :around_out
  end

  def last
    trail << :last
  end

  def check
    trail << :check
    @ok
  end

  def saved
    trail << :saved
  end
end
"#;

const QUIET_PIPELINE: &str = r#"class QuietPipeline < Pipeline
  skip_callback :run, :before, :first
  skip_callback :run, :after, :last, if: :enabled?
  set_callback :run, :after, :quiet

  private

  def quiet
    trail << :quiet
  end
end
"#;

/// Core's merchant_marketing FormBuilderField shape: a class that extends
/// the tracker and asks it for its descendants.
const FORM_FIELD: &str = r#"class FormField
  extend ActiveSupport::DescendantsTracker

  def self.kinds
    ActiveSupport::DescendantsTracker.descendants(self).map(&:name).sort
  end
end
"#;

const TEXT_FIELD: &str = "class TextField < FormField\nend\n";

const SECRET_FIELD: &str = "class SecretField < TextField\nend\n";

/// Plus::Plan subclasses whose callbacks raise, defined after the files
/// load, and the observations; run natively and emitted.
const OBSERVE: &str = r#"module Plus
  module Plan
    class Failing < Change
      before_perform { raise ArgumentError, "refused" }
      after_perform { events << :after }
      def execute!; events << :never; end
    end

    class Broken < Change
      after_perform { events << :after_broken }
      def execute!; raise "execute failed"; end
    end
  end
end
upgrade = Plus::Plan::Upgrade.new.perform
p upgrade.events, upgrade.success?, upgrade.executed?
p upgrade.perform.events
failing = Plus::Plan::Failing.new
begin
  failing.perform
rescue ArgumentError => e
  p [:raised, e.message]
end
p failing.executed?, failing.success?, failing.error_message, failing.events
broken = Plus::Plan::Broken.new.perform
p broken.success?, broken.error_message, broken.events
p Plus::Plan::Change._perform_callbacks.count, Plus::Plan::Upgrade._perform_callbacks.map(&:kind)
[true, false].each do |enabled|
  pipeline = Pipeline.new(enabled)
  p pipeline.go, pipeline.trail
end
[true, false].each do |ok|
  pipeline = Pipeline.new(true)
  p pipeline.save(ok), pipeline.trail
end
[true, false].each do |enabled|
  quiet = QuietPipeline.new(enabled)
  p quiet.go, quiet.trail
end
p Pipeline._run_callbacks.map { |c| [c.kind, c.filter.is_a?(Symbol) ? c.filter : c.filter.class.name] }
p QuietPipeline._run_callbacks.count, Pipeline._run_callbacks.count
p Pipeline.new(true).respond_to?(:_run_save_callbacks), Plus::Plan::Change.new.respond_to?(:_run_perform_callbacks)
p FormField.kinds, TextField.descendants, Pipeline.descendants
"#;

const EXPECTED: &str = r#"[:before, :execute, :after]
true
true
[:before, :execute, :after]
[:raised, "refused"]
true
false
"refused"
[]
false
"execute failed"
[:after_broken]
0
[:before, :after]
"#;

const FILES: &[(&str, &str, &str)] = &[
    ("lib/plus/plan/change.rb", "app/models/plus/plan/change.rb", CHANGE),
    ("lib/plus/plan/upgrade.rb", "app/models/plus/plan/upgrade.rb", UPGRADE),
    ("lib/pipeline.rb", "app/models/pipeline.rb", PIPELINE),
    ("lib/quiet_pipeline.rb", "app/models/quiet_pipeline.rb", QUIET_PIPELINE),
    ("lib/form_field.rb", "app/models/form_field.rb", FORM_FIELD),
    ("lib/text_field.rb", "app/models/text_field.rb", TEXT_FIELD),
    ("lib/secret_field.rb", "app/models/secret_field.rb", SECRET_FIELD),
];

fn app(postgresql: bool) -> emit_and_run::Overlay {
    let mut app = emit_and_run::empty_app();
    for (path, _, text) in FILES {
        app = app.write(path, text);
    }
    if postgresql {
        app.write("roundhouse.yml", "test_paths:\n  - test\ndatabase_adapter: postgresql\n")
            .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_active_support_callbacks\n")
    } else {
        app.write("roundhouse.yml", "test_paths:\n  - test\n")
    }
}

fn requires() -> String {
    FILES.iter().map(|(_, emitted, _)| format!("require_relative {:?}\n", emitted.trim_end_matches(".rb"))).collect()
}

fn native() -> String {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let dir = std::env::temp_dir().join(format!("rh_active_support_callbacks_native_{}", std::process::id()));
    // The emitted tree's layout, so the one `requires()` serves both.
    for (_, emitted, text) in FILES {
        let file = dir.join(emitted);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let program = format!(
        "require {oracle:?}\nRailsOracle.activate(\"activesupport\")\n\
         require \"active_support\"\nrequire \"active_support/callbacks\"\nrequire \"active_support/descendants_tracker\"\n\
         abort \"native: activesupport is not Core's\" unless Gem.loaded_specs[\"activesupport\"].full_gem_path.start_with?(RailsOracle::CORE_GEM_HOME)\n\
         {}{OBSERVE}",
        requires()
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
fn callbacks_run_on_the_postgresql_tree_like_activesupport() {
    let (dir, errors) = app(true).emit(BuildTarget::Ruby);
    // `check` names the PostgreSQL adapter as having no runtime of its
    // own in target-independent analysis; the CRuby tree carries one.
    let errors: Vec<_> = errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect();
    assert!(errors.is_empty(), "{errors:#?}");
    assert!(dir.join("runtime/active_support_callbacks.rb").exists());
    let facades = std::fs::read_to_string(dir.join("runtime/gem_facades.rb")).expect("gem_facades.rb");
    assert!(facades.contains("require_relative \"active_support_callbacks\""), "{facades}");
    for (_, file, _) in FILES {
        let text = std::fs::read_to_string(dir.join(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
        assert!(!text.contains("roundhouse:"), "{file}: {text}");
    }
    let change = std::fs::read_to_string(dir.join("app/models/plus/plan/change.rb")).unwrap();
    assert!(change.contains("include ActiveSupport::Callbacks") && change.contains("define_callbacks :perform"), "{change}");
    let quiet = std::fs::read_to_string(dir.join("app/models/quiet_pipeline.rb")).unwrap();
    assert!(quiet.contains("skip_callback :run, :after, :last, if: :enabled?"), "{quiet}");

    let script = format!("require_relative 'runtime/active_support_callbacks'\n{}{OBSERVE}", requires());
    let out = emit_and_run::ruby().current_dir(&*dir).args(["-W:no-deprecated", "-e", &script]).output().expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.starts_with(EXPECTED), "{stdout}");
    assert_eq!(stdout, native(), "emitted on the port vs native activesupport");
}

/// The SQLite CRuby tree carries and loads the port too (gem_facades.rb);
/// Spinel has none and refuses the include by name.
#[test]
fn the_sqlite_tree_boots_it_and_spinel_refuses_the_include() {
    let (dir, errors) = app(false).emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let script = format!("require File.expand_path('main', Dir.pwd)\n{}{OBSERVE}", requires());
    let out = emit_and_run::ruby().current_dir(&*dir).args(["-W0", "-e", &script]).env("BLOG_DB", ":memory:").output().expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.starts_with(EXPECTED), "{stdout}");

    let (dir, errors) = app(false).emit(BuildTarget::Spinel);
    assert!(!dir.join("runtime/active_support_callbacks.rb").exists());
    for class in ["Plus::Plan::Change", "Pipeline"] {
        let expected = format!(
            "{class} includes ActiveSupport::Callbacks, declared by the Ruby-family runtime (runtime/active_support_callbacks.rb), which spinel does not emit"
        );
        assert!(errors.iter().any(|e| e.contains(&expected)), "spinel {class}: {errors:#?}");
    }
    assert!(
        errors.iter().any(|e| e.contains(
            "FormField extends ActiveSupport::DescendantsTracker, declared by the Ruby-family runtime (runtime/active_support_descendants_tracker.rb), which spinel does not emit"
        )),
        "{errors:#?}"
    );
}

/// An app that names only the tracker carries it without the callbacks
/// port, and the class body's `extend` is replayed.
#[test]
fn the_descendants_tracker_rides_alone_where_only_it_is_named() {
    let (dir, errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("lib/form_field.rb", FORM_FIELD)
        .write("lib/text_field.rb", TEXT_FIELD)
        .write("lib/secret_field.rb", SECRET_FIELD)
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    assert!(dir.join("runtime/active_support_descendants_tracker.rb").exists());
    assert!(!dir.join("runtime/active_support_callbacks.rb").exists());
    let field = std::fs::read_to_string(dir.join("app/models/form_field.rb")).unwrap();
    assert!(field.contains("extend ActiveSupport::DescendantsTracker"), "{field}");
    let script = "require File.expand_path('main', Dir.pwd)\nrequire_relative 'app/models/form_field'\n\
                  require_relative 'app/models/text_field'\nrequire_relative 'app/models/secret_field'\n\
                  p FormField.kinds, ActiveSupport::DescendantsTracker.subclasses(FormField)\n";
    let out = emit_and_run::ruby().current_dir(&*dir).args(["-W0", "-e", script]).env("BLOG_DB", ":memory:").output().expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stdout, "[\"SecretField\", \"TextField\"]\n[TextField]\n");
}
