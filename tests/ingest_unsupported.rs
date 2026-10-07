//! `check` reports a support-dir file whose ingest failed.
//!
//! The walk over `lib/`, an engine's `lib/` and `app/helpers/` goes on
//! past a file it cannot ingest, so one gap does not abort the app. That
//! left the file's classes out of the IR with nothing said: a Core
//! `check` reported zero errors for Podding files the emitted app did
//! not contain. Each such file is now an `ingest_unsupported` error at
//! the construct that failed it; `--continue` lists the same gap in its
//! survey punch list instead.

use std::path::Path;
use std::process::Command;

fn app(root: &Path) {
    let _ = std::fs::remove_dir_all(root);
    for (path, text) in [
        ("db/schema.rb", "ActiveRecord::Schema.define do\nend\n"),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
        ("lib/tally.rb", "module Tally\n  def self.sum(xs)\n    total = 0\n    for x in xs\n      total += x\n    end\n    total\n  end\nend\n"),
        ("lib/counter.rb", "module Counter\n  def self.one\n    1\n  end\nend\n"),
    ] {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
}

fn check(root: &Path, args: &[&str]) -> (bool, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_roundhouse")).arg("check").args(args).arg(root).output().unwrap();
    (
        output.status.success(),
        format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr)),
    )
}

#[test]
fn a_dropped_lib_file_is_an_error_at_the_construct() {
    let root = std::env::temp_dir().join(format!("roundhouse-ingest-unsupported-{}", std::process::id()));
    app(&root);
    let (ok, text) = check(&root, &[]);
    let (_, survey) = check(&root, &["--continue"]);
    let _ = std::fs::remove_dir_all(&root);
    assert!(!ok, "check passed with a dropped file:\n{text}");
    assert!(
        text.contains("lib/tally.rb:4:5: error[ingest_unsupported]: `for` requires enclosing-scope lowering"),
        "no ingest_unsupported error at the `for`:\n{text}"
    );
    assert!(text.contains("left this file's classes, modules and constants out of the emitted app"), "{text}");
    assert_eq!(text.matches("error[ingest_unsupported]").count(), 1, "only the failed file:\n{text}");
    // Survey mode keeps going through the gap and lists it once, as a
    // gap rather than a second, per-file error.
    assert!(!survey.contains("error[ingest_unsupported]"), "{survey}");
    assert!(survey.contains("`for` requires enclosing-scope lowering"), "{survey}");
}

/// Core's `Podding::Database::Concerns::ShardedTable::ClassMethods`
/// carries a `class << self; def extended(base)` hook. That singleton is
/// the carrier module's own, not the includers' class side the carrier
/// is flattened into, so it is refused by name at its line.
#[test]
fn a_class_methods_carrier_singleton_is_refused_by_name() {
    let root = std::env::temp_dir().join(format!("roundhouse-ingest-carrier-{}", std::process::id()));
    app(&root);
    let file = root.join("lib/sharded.rb");
    std::fs::write(
        &file,
        "module Sharded\n  extend ActiveSupport::Concern\n\n  module ClassMethods\n    class << self\n      def extended(base)\n        base\n      end\n    end\n\n    def sharded?\n      true\n    end\n  end\nend\n",
    )
    .unwrap();
    let (_, text) = check(&root, &[]);
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        text.contains(
            "lib/sharded.rb:5:5: error[ingest_unsupported]: `class << self` in a concern's `ClassMethods` defines the carrier module's own singleton methods"
        ),
        "{text}"
    );
}
