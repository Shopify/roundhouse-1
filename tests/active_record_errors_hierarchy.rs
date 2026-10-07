//! Rails 8.2's Active Record exception hierarchy on the CRuby tree,
//! against the same probe run natively on Core's pinned activerecord
//! (`tests/support/rails_oracle.rb`).
//!
//! The probe (`tests/support/active_record_errors_probe.rb`) prints, for
//! every class of activerecord's errors.rb (and associations/errors.rb's
//! AssociationNotFoundError, validations.rb's RecordInvalid), its class
//! ancestry up to StandardError and its bare message, then Rails'
//! constructors and readers (`connection_pool:`, `sql:`/`binds:`,
//! `set_pool`, `set_query`, RecordNotFound's model/primary_key/id,
//! RecordNotSaved's record, the NoDatabaseError and
//! DatabaseConnectionError factories), rescues by ancestry, and the
//! constants Core's essentials patch
//! (`lib/patches/active_record/granular_exceptions.rb`) derives with
//! `Class.new(ActiveRecord::AdapterError)` from app code. Every line of
//! the emitted run must equal the native run's.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

const PROBE: &str = include_str!("support/active_record_errors_probe.rb");

/// Core's granular_exceptions.rb constants, as Core writes them.
const GRANULAR: &str = "# typed: true\n\
ActiveRecord::ConnectionTimedout = Class.new(ActiveRecord::ConnectionNotEstablished)\n\
ActiveRecord::ConnectionUnavailable = Class.new(ActiveRecord::ConnectionNotEstablished)\n\
ActiveRecord::TransactionTooLargeError = Class.new(ActiveRecord::AdapterError)\n";

/// Rails' errors.rb classes (60), AssociationNotFoundError and
/// RecordInvalid: an ancestry line each, a bare message for all but
/// RecordInvalid and MismatchedForeignKey, then 29 observations.
const LINES: usize = 62 + 60 + 29;

fn native() -> String {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let probe = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/active_record_errors_probe.rb");
    let dir = std::env::temp_dir().join(format!("rh_ar_errors_native_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // A booted Rails app has ActiveSupport's String#squish, which
    // MismatchedForeignKey's message uses.
    let program = format!(
        "require {oracle:?}\nRailsOracle.activate(\"activerecord\")\nrequire \"active_record\"\n\
         require \"active_support/core_ext/string/filters\"\n\
         abort \"native: activerecord is not Core's\" unless Gem.loaded_specs[\"activerecord\"].full_gem_path.start_with?(RailsOracle::CORE_GEM_HOME)\n\
         {GRANULAR}load {probe:?}\n"
    );
    std::fs::write(dir.join("native.rb"), program).unwrap();
    let out = Command::new("ruby").arg("native.rb").current_dir(&dir).output().expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "native run failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    stdout
}

#[test]
fn the_error_hierarchy_answers_like_rails() {
    // The probe runs outside the emitted tree and names runtime methods
    // the app does not (the NoDatabaseError and DatabaseConnectionError
    // factories), which the tree shake would drop. The only test in this
    // binary, so no other thread reads the variable.
    unsafe { std::env::set_var("ROUNDHOUSE_NO_TREESHAKE", "1") };
    let emitted = emit_and_run::real_blog()
        .write("lib/patches/active_record/granular_exceptions.rb", GRANULAR)
        .run_ruby(PROBE);
    emitted.assert_passes();
    let native = native();
    let (emitted_lines, native_lines): (Vec<&str>, Vec<&str>) =
        (emitted.stdout.lines().collect(), native.lines().collect());
    assert_eq!(native_lines.len(), LINES, "native:\n{native}");
    for (e, n) in emitted_lines.iter().zip(&native_lines) {
        assert_eq!(e, n, "emitted:\n{}\nnative:\n{native}", emitted.stdout);
    }
    assert_eq!(emitted_lines.len(), LINES, "emitted:\n{}", emitted.stdout);
    for expected in [
        "StatementInvalid: ActiveRecord::StatementInvalid < ActiveRecord::AdapterError < ActiveRecord::ActiveRecordError",
        "RecordNotUnique: ActiveRecord::RecordNotUnique < ActiveRecord::WrappedDatabaseException < ActiveRecord::StatementInvalid < ActiveRecord::AdapterError < ActiveRecord::ActiveRecordError",
        "RecordNotFound is_a ActiveRecordError: true",
        "ConnectionTimeoutError < ConnectionNotEstablished: true",
        "RecordNotSaved record: [\"m\", :r]",
        "granular TransactionTooLargeError: [ActiveRecord::AdapterError, true]",
        "rescue ActiveRecordError: \"caught ActiveRecord::ReadOnlyRecord: ActiveRecord::ReadOnlyRecord\"",
    ] {
        assert!(native_lines.contains(&expected), "missing {expected:?} in native:\n{native}");
    }
}
