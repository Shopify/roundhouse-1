//! Which source files make a constant defined before anything requires
//! it. `defined?(Const)` is folded true only for those
//! (`lower::defined_const_fold`).
//!
//! Core's essentials engine keeps `lib/verdict/evaluation_tracker.rb`
//! (`Verdict::EvaluationTracker`) outside every autoload root: a
//! to_prepare hook requires it. The path under `lib/` names the constant,
//! and that was taken for an autoload, so the guard in
//! `CapturesEventReporterContext#initialize` (`if defined?(Verdict::
//! EvaluationTracker)`) folded true; the emitted initialize read the
//! constant before the hook ran: `uninitialized constant
//! Verdict::EvaluationTracker`, which took down every boot hook that
//! built an exception, and with them the autoloads those hooks were
//! loading (`Hedwig::CommandTable::Redis::BaseCommandTable`).

use roundhouse::autoload_roots::AutoloadRoots;
use roundhouse::App;

#[test]
fn only_an_autoload_root_maps_a_source_to_its_constant() {
    let app = App::default();
    let roots = AutoloadRoots::of(&app);
    let names = |source: &str, constant: &str| roots.source_names(source, constant);
    assert!(names("app/models/gadget.rb", "gadget"));
    assert!(names("components/x/app/models/verdict/flag.rb", "verdict/flag"));
    // The app's own lib/ (config.autoload_lib).
    assert!(names("lib/outer.rb", "outer"));
    // An engine's lib/ is on the load path, which a `require` resolves.
    assert!(!names("components/platform/essentials/lib/verdict/evaluation_tracker.rb", "verdict/evaluation_tracker"));
    // Both are still a root for the emitted index's `require` resolution.
    assert!(roots.is_root(&["components", "platform", "essentials", "lib"]));
}
