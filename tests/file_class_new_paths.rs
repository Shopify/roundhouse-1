//! A file-level `A::B = Class.new(Parent)` is a class declaration, as
//! the plain-constant form is (tests/class_new_constant.rs), and a
//! file's top-level `ActiveSupport.on_load(:hook) do … end` for a hook
//! the ingest does not reopen is registered as written. Shopify Core's
//! components/platform/essentials/lib/patches/active_record/
//! granular_exceptions.rb is both: six `ActiveRecord::X =
//! Class.new(...)` error classes and a Trilogy-only load hook. Before,
//! the file's classes were not compiled, and every reader of them was a
//! gap-attributed note.
//!
//! CRuby is the oracle: the same script runs on the source (with Rails
//! 8.1.4's own lazy_load_hooks) and on the emitted code, and the outputs
//! must agree.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

use roundhouse::project::BuildTarget;

/// The lexical rule the path form follows: the write runs at the top
/// level, so `Fallback` is `::Fallback`, not `Errs::Fallback`.
const ERRS: &str = r##"class Fallback < RuntimeError; end

module Errs
  class Fallback < StandardError; end

  def self.boom(kind)
    case kind
    when :timedout then raise Errs::Timedout, "timed out"
    when :deep then raise Errs::Deep, "deep"
    else raise Errs::Unavailable, "unavailable"
    end
  end
end

Errs::Timedout = Class.new(Errs::Fallback)
Errs::Unavailable = Class.new(Fallback)
::Errs::Deep = Class.new(Errs::Timedout)
"##;

const SCRIPT: &str = r##"
p Errs::Timedout.name, Errs::Timedout.superclass.name, Errs::Unavailable.superclass.name, Errs::Deep.superclass.name
p Errs::Unavailable.ancestors.include?(RuntimeError), Errs::Unavailable.ancestors.include?(Errs::Fallback)
[:timedout, :deep, :other].each do |kind|
  begin
    Errs.boom(kind)
  rescue Errs::Fallback, Fallback => e
    p [e.class.name, e.message, e.is_a?(Errs::Timedout)]
  end
end
"##;

#[test]
fn file_level_class_new_path_writes_are_classes() {
    let expected = "\"Errs::Timedout\"\n\"Errs::Fallback\"\n\"Fallback\"\n\"Errs::Timedout\"\ntrue\nfalse\n\
                    [\"Errs::Timedout\", \"timed out\", true]\n[\"Errs::Deep\", \"deep\", true]\n\
                    [\"Errs::Unavailable\", \"unavailable\", false]\n";
    let native = Command::new("ruby")
        .arg("-W0")
        .arg("-e")
        .arg(format!("{ERRS}\n{SCRIPT}"))
        .output()
        .expect("native Ruby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(String::from_utf8_lossy(&native.stdout), expected);

    let run = emit_and_run::real_blog().write("app/lib/errs.rb", ERRS).run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout, expected);
}

const GRANULAR: &str = r##"# typed: true
# frozen_string_literal: true

ActiveRecord::ConnectionTimedout = Class.new(ActiveRecord::ConnectionNotEstablished)
ActiveRecord::ConnectionUnavailable = Class.new(ActiveRecord::ConnectionNotEstablished)
ActiveRecord::TransactionTooLargeError = Class.new(ActiveRecord::AdapterError)

module TrilogyAdapterExceptions
  private

  def translate_exception(exception, *, **)
    if exception.message.include?("did not complete within")
      ActiveRecord::ConnectionTimedout.new(exception.message)
    else
      ActiveRecord::TransactionTooLargeError.new(exception.message)
    end
  end
end

ActiveSupport.on_load(:active_record_trilogyadapter) do
  include(TrilogyAdapterExceptions)
end
"##;

/// After the file loads: the classes exist, nothing has the mixin, and
/// firing the hook (as the Trilogy adapter's class body would) mixes it
/// into the base.
const HOOK_SCRIPT: &str = r##"
p ActiveRecord::ConnectionTimedout.superclass.name, ActiveRecord::TransactionTooLargeError.superclass.name
p ObjectSpace.each_object(Class).any? { |k| k.include?(TrilogyAdapterExceptions) }
Adapter = Class.new
ActiveSupport.run_load_hooks(:active_record_trilogyadapter, Adapter)
p Adapter.include?(TrilogyAdapterExceptions), Adapter.private_method_defined?(:translate_exception)
p Adapter.new.send(:translate_exception, RuntimeError.new("did not complete within 5s")).class.name
"##;

fn pg_app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\ndatabase_adapter: postgresql\n")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_file_class_new_paths\n")
        .write("lib/patches/active_record/granular_exceptions.rb", GRANULAR)
}

#[test]
fn a_file_load_hook_registers_as_written_on_the_postgresql_tree() {
    let expected = "\"ActiveRecord::ConnectionNotEstablished\"\n\"ActiveRecord::AdapterError\"\nfalse\ntrue\ntrue\n\
                    \"ActiveRecord::ConnectionTimedout\"\n";
    let native = Command::new("ruby")
        .arg("-W0")
        .arg("-e")
        .arg(format!("require \"active_record\"\n{GRANULAR}\n{HOOK_SCRIPT}"))
        .output()
        .expect("native Ruby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(String::from_utf8_lossy(&native.stdout), expected);

    let (dir, errors) = pg_app().emit(BuildTarget::Ruby);
    let errors: Vec<&String> = errors.iter().filter(|e| !e.contains("database_adapter")).collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let read = |rel: &str| {
        std::fs::read_to_string(dir.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
    };
    let timedout = read("app/models/active_record/connection_timedout.rb");
    // Rooted: Ruby reads the parent at the top level, where the write runs.
    assert_eq!(timedout, "module ActiveRecord\n  class ConnectionTimedout < ::ActiveRecord::ConnectionNotEstablished\n  end\nend\n");
    // The file program: the hook statement, run where the file loads.
    let program = read("app/models/roundhouse_files/lib/patches/active_record/granular_exceptions.rb");
    let statement = program
        .lines()
        .find(|line| line.trim_start().starts_with("ActiveSupport.on_load(:active_record_trilogyadapter)"))
        .unwrap_or_else(|| panic!("no hook registration:\n{program}"))
        .trim();
    // The PostgreSQL tree cannot boot without the `pg` gem, so the
    // emitted pieces run in their load order on CRuby: the runtime's
    // lazy_load_hooks and Active Record, the emitted classes, the
    // emitted module, the emitted registration.
    let emitted_script = format!(
        "%w[runtime/active_support_lazy_load_hooks runtime/active_record \
         app/models/active_record/connection_timedout app/models/active_record/connection_unavailable \
         app/models/active_record/transaction_too_large_error app/models/trilogy_adapter_exceptions]\
         .each {{ |f| require_relative File.join(Dir.pwd, f) }}\n{statement}\n{HOOK_SCRIPT}"
    );
    let emitted = Command::new("ruby").arg("-W0").arg("-e").arg(&emitted_script).current_dir(&*dir).output().expect("ruby");
    assert!(emitted.status.success(), "{}\n{emitted_script}", String::from_utf8_lossy(&emitted.stderr));
    assert_eq!(String::from_utf8_lossy(&emitted.stdout), expected);
}

#[test]
fn a_tree_without_the_lazy_load_hooks_port_refuses_the_registration() {
    // No database configuration: the SQLite tree.
    let tree: roundhouse::det::HashMap<std::path::PathBuf, Vec<u8>> = [(
        std::path::PathBuf::from("lib/patches/active_record/granular_exceptions.rb"),
        GRANULAR.as_bytes().to_vec(),
    )]
    .into_iter()
    .collect();
    let app = roundhouse::ingest::ingest_app_from_tree(tree).expect("ingest");
    assert!(
        app.file_programs.iter().any(|p| p.source.ends_with("granular_exceptions.rb")),
        "the file program is carried"
    );
    for target in [BuildTarget::Ruby, BuildTarget::Spinel] {
        let (result, diagnostics) = roundhouse::emit::diagnostics::scope(|| {
            roundhouse::project::target_files(&app, std::path::Path::new("not-a-fixture"), target)
        });
        let error = result.err().unwrap_or_else(|| panic!("{} emitted the registration", target.as_str()));
        assert!(error.contains("`ActiveSupport.on_load` registration needs the PostgreSQL CRuby tree"), "{error}");
        assert!(
            diagnostics.iter().any(|d| d.message.contains("load_hook_registration")
                && d.message.contains("`ActiveSupport.on_load(:active_record_trilogyadapter)`")),
            "{diagnostics:#?}"
        );
    }
}
