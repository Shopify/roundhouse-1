//! One gap, one row. `diagnose` reports a send whose known receiver
//! answered no type as `send_dispatch_failed`, a fallback for sites
//! nothing else explains. A send the typer already refused carries its
//! own, more specific row: `plain.as_json` is ActiveSupport's Object
//! extension, which Ruby has (`Plain.new.respond_to?(:as_json)`) and
//! the runtime has not ported. Reporting "no known method `as_json` on
//! Plain" beside it counted the one gap twice, and the second row was
//! false. Core's GraphQL user-error classes and controllers had these
//! pairs (`GiftCards::RecipientParams#as_json`). A send that nothing
//! else covers still gets the fallback.

use std::process::Command;

fn check(files: &[(&str, &str)]) -> String {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("rh_one_row_per_gap_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (path, text) in [
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
        ("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n"),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ]
    .iter()
    .chain(files)
    {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let output = Command::new(env!("CARGO_BIN_EXE_roundhouse")).arg("check").arg(&root).output().unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

#[test]
fn a_refused_send_is_not_also_a_missing_method() {
    let native = Command::new("ruby")
        .args(["-W0", "-e", "require \"active_support\"\nrequire \"active_support/core_ext/object/json\"\nclass Plain; end\np [Plain.new.respond_to?(:as_json), Plain.new.respond_to?(:frobnicate)]"])
        .output()
        .expect("ruby");
    assert_eq!(String::from_utf8_lossy(&native.stdout), "[true, false]\n", "{}", String::from_utf8_lossy(&native.stderr));

    let controller = "class ThingsController < ApplicationController\n  #: (Plain) -> untyped\n  def flash_of(d) = d.as_json\n\n  #: (Plain) -> untyped\n  def missing_of(d) = d.frobnicate\nend\n";
    let text = check(&[("app/lib/plain.rb", "class Plain\nend\n"), ("app/controllers/things_controller.rb", controller)]);
    let rows = |line: &str| -> Vec<String> {
        text.lines()
            .filter(|l| l.contains(&format!("things_controller.rb:{line}:")))
            .map(|l| l.split(": ").nth(1).unwrap_or("").to_string())
            .collect()
    };
    assert_eq!(rows("3"), ["error[unsupported]"], "{text}");
    assert!(text.contains("Object extension not supported (all targets): as_json"), "{text}");
    assert_eq!(rows("6"), ["error[send_dispatch_failed]"], "{text}");
    assert!(text.contains("no known method `frobnicate` on Plain"), "{text}");
    assert!(!text.contains("no known method `as_json`"), "{text}");
}
