//! ActiveSupport.on_load / run_load_hooks and active_support.rb's
//! autoload entries (runtime/spinel/active_support_lazy_load_hooks.rb) on
//! the PostgreSQL CRuby tree, against the Rails native Core runs
//! (Shopify/rails 5813559f4aed; tests/support/rails_oracle.rb).
//! tests/active_support_lazy_load_hooks_runtime.rb lays out a tree the
//! way the emit does and requires the shared observations equal: every
//! declared autoload is one of active_support.rb's and resolves to the
//! same kind of constant with only the hooks file loaded, Concern works,
//! hooks run in Rails' order, and the tree's own failing hook raises.
//!
//! Core's boot needed both. 46 file statements failed with
//! `uninitialized constant ActiveSupport::Concern` (identifications,
//! through country_db and the component lib files), which Rails
//! autoloads; and a native gem's hook for a framework class the tree
//! ports in part (turbo-rails' `include Turbo::Broadcastable` into
//! ActiveRecord::Base) stopped the whole boot. Now such a hook is named,
//! error[load_hook_not_run], and boot goes on.
use std::path::Path;
use std::process::Command;

#[test]
fn load_hooks_and_active_support_autoloads_agree_with_rails_and_native_gem_hooks_are_named() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new("ruby")
        .arg("-W0")
        .arg(root.join("tests/active_support_lazy_load_hooks_runtime.rb"))
        .arg("compare")
        .current_dir(root)
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("Native ActiveSupport and the emitted hooks runtime agree"),
        "the hooks runtime diverged from activesupport\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    for line in [
        // The Core boot's 46 NameErrors: Rails autoloads Concern.
        "= autoload Concern: active_support.rb declares it",
        "= Concern resolves: Module; autoload? after: nil",
        "= ActiveSupport::Concern: included_ran class_method StandardError",
        "! require \"active_support/concern\" after the autoload: loaded once, no warnings",
        // A native gem's failing hook is named and boot goes on.
        "! a gem's hook that raises NameError: roundhouse: error[load_hook_not_run]: fakegem-1.0.0/lib/fakegem/engine.rb:1 on_load(:probe_gem) did not run: NameError: uninitialized constant Fakegem::Missing; the next hook ran; run_load_hooks returned",
        "on_load(:probe_gem_load) did not run: LoadError: cannot load such file -- active_support/core_ext/array/extract_options",
        // A hook the tree's railtie or engine code registers is covered
        // by that code's convention; the tree's other hooks raise.
        "(registered by a railtie or engine body statement, error[railtie_body_statement_not_run])",
        "(registered by a railtie or engine initializer, error[gem_railtie_not_run])",
        "= the tree's own hook that raises: raises NameError: uninitialized constant AppMissing",
        "! the tree's hook registered outside a railtie statement still raises NameError: uninitialized constant TopLevelMissing",
        // Fired where the test helper fires the test-case hooks, the same
        // hook is named and the next one runs.
        "on_load(:probe_top_level) did not run: NameError: uninitialized constant TopLevelMissing (fired at activesupport/lib/active_support/test_case.rb:408); the next hook ran",
        // A test base's hooks wait for its first subclass, as Rails loads
        // the base where a test first names it.
        "! a test base's hooks: none before its first subclass, then once, after the bases its file requires; roundhouse: error[load_hook_not_run]: app/models/probe_cases.rb:2 on_load(:probe_view_case) did not run: NameError: uninitialized constant CaseMissing (fired at actionview/lib/action_view/test_case.rb:256); a hook registered after runs at once",
        // A Behavior base's hooks also run on each class that includes its
        // Behavior, as Rails runs them in Behavior's `included do`.
        "! a Behavior base's hooks: the first includer fires the base's and the bases its file requires, then its own; each includer gets them; a subclass of the base runs none; roundhouse: error[load_hook_not_run]: app/models/probe_behavior.rb:2 on_load(:probe_behavior_case) did not run: NameError: uninitialized constant BehaviorMissing (fired at actionview/lib/action_view/test_case.rb:256)",
    ] {
        assert!(stdout.contains(line), "missing observation {line:?}:\n{stdout}");
    }
}
