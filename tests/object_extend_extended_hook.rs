//! Emitted-program regression: `base.extend(Mod)` inside a module's
//! `self.extended(base)` hook restructures the CLASS being extended, so
//! the Ruby-family tree runs it as written, and
//! `Rails.application.config.eager_load` answers the environment's value.
//!
//! Core's sharded models get their lazy-loading verifier this way
//! (podding/database/concerns/sharded_table.rb, `ClassMethods.extended`:
//! `base.extend(Podding::Verifier::LazyLoadingConcern) unless
//! Rails.application.config.eager_load`). The Object#extend stub replaced
//! the call with a nil, which dropped the verifier, and the guard then
//! raised NoMethodError (`eager_load` for Rails::Configuration) in every
//! sharded model's class body, the boot fatal of unit-1000 round 0.
//!
//! The expected values are what the pinned Rails 8.2 (railties
//! `attr_accessor :eager_load`, booted through `initialize!`) prints for
//! the same modules: `[true, true, false]` with `config.eager_load =
//! false`, `[false, true, true]` with `true`.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::path::Path;

const PROBE: &str = r##"module ExtendedHookProbe
  module LazyVerifier
    def lazy_verified?
      true
    end
  end

  module TableMethods
    def self.extended(base)
      base.extend(LazyVerifier) unless Rails.application.config.eager_load
    end

    def table_method?
      true
    end
  end

  class Table
    extend TableMethods
  end
end
"##;

/// A concern's `included` hook as well: the strict lanes restub hook
/// extends only where the app has an `included`/`prepended`/`inherited`
/// hook (`project::without_runtime_mixins`), as Core does.
const CONCERN: &str = r##"module ExtendedHookConcern
  def self.included(base)
    base.extend(Comparable)
  end
end
"##;

const SCRIPT: &str = "t = ExtendedHookProbe::Table\n\
    p [t.singleton_class.include?(ExtendedHookProbe::LazyVerifier), t.table_method?, Rails.application.config.eager_load]\n";

/// Every emitted file of the probe's namespace, concatenated.
fn probe_source(emitted: &Path) -> String {
    fn walk(dir: &Path, out: &mut String) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rb") {
                out.push_str(&std::fs::read_to_string(&path).unwrap_or_default());
            }
        }
    }
    let mut out = std::fs::read_to_string(emitted.join("app/models/extended_hook_probe.rb")).unwrap_or_default();
    walk(&emitted.join("app/models/extended_hook_probe"), &mut out);
    out
}

fn run(emitted: &Path, env: &[(&str, &str)]) -> String {
    let output = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("require File.expand_path(\"main\", Dir.pwd)\nMain.configure_default_adapter!\n{SCRIPT}"))
        .current_dir(emitted)
        .env("BLOG_DB", ":memory:")
        .env_remove("CI")
        .envs(env.iter().copied())
        .output()
        .expect("spawn ruby");
    assert!(
        output.status.success(),
        "emitted program failed ({env:?}):\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim_end().to_string()
}

#[test]
fn extend_in_an_extended_hook_runs_on_the_ruby_tree() {
    let (emitted, errors) = emit_and_run::real_blog()
        .write("app/models/extended_hook_probe.rb", PROBE)
        .write("app/models/extended_hook_concern.rb", CONCERN)
        .emit(roundhouse::project::BuildTarget::Ruby);
    assert!(errors.is_empty(), "analysis or emit reports errors:\n{}", errors.join("\n"));
    let src = probe_source(&emitted);
    assert!(src.contains("base.extend(LazyVerifier)") || src.contains("base.extend(ExtendedHookProbe::LazyVerifier)"),
        "the extended hook's extend was stubbed:\n{src}");
    assert!(!src.contains("Object#extend not supported"), "the extended hook's extend was stubbed:\n{src}");

    // real-blog: development.rb `config.eager_load = false`; test.rb
    // `config.eager_load = ENV["CI"].present?`.
    assert_eq!(run(&emitted, &[("RAILS_ENV", "development")]), "[true, true, false]");
    assert_eq!(run(&emitted, &[("RAILS_ENV", "test"), ("CI", "1")]), "[false, true, true]");
}

/// The targets without runtime mixins keep the raise stub for the hook's
/// extend, as before (`object_extend::stub_module_hook_extends`).
#[test]
fn extend_in_an_extended_hook_stays_stubbed_on_spinel() {
    let (emitted, _errors) = emit_and_run::real_blog()
        .write("app/models/extended_hook_probe.rb", PROBE)
        .write("app/models/extended_hook_concern.rb", CONCERN)
        .emit(roundhouse::project::BuildTarget::Spinel);
    let src = probe_source(&emitted);
    assert!(src.contains("def self.extended(base)"), "no extended hook emitted:\n{src}");
    assert!(!src.contains(".extend(LazyVerifier)") && !src.contains(".extend(ExtendedHookProbe::LazyVerifier)"),
        "the hook's extend reached the spinel tree:\n{src}");
}

/// An `extended` hook that reassigns its parameter proves nothing about
/// what it extends: the instance-form stub stays.
#[test]
fn extend_on_a_reassigned_extended_hook_param_is_still_stubbed() {
    let (emitted, _errors) = emit_and_run::real_blog()
        .write(
            "app/models/extended_hook_probe.rb",
            "module ExtendedHookProbe\n  module Reassigning\n    def self.extended(base)\n      base = Object.new\n      base.extend(Comparable)\n    end\n  end\nend\n",
        )
        .emit(roundhouse::project::BuildTarget::Ruby);
    let src = probe_source(&emitted);
    assert!(src.contains("Object#extend not supported"), "reassigned-param extend not stubbed:\n{src}");
}
