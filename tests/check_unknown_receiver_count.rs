//! `roundhouse check` counts the sends nothing checked: an explicit
//! receiver of unknown or gradual type consults no method table, so a
//! missing method passes. In a library class nothing else reports them.
//! The count sits beside the error total so that an error that goes
//! away by becoming such a send is visible as a reclassification.
use std::process::Command;

fn check(args: &[&str], files: &[(&str, &str)]) -> String {
    let root = std::env::temp_dir().join(format!("rh_unknown_sends_{}_{}", std::process::id(), args.len()));
    let _ = std::fs::remove_dir_all(&root);
    for (path, text) in [
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
        ("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n"),
        ("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table \"probes\" do |t|\n    t.string \"title\"\n  end\nend\n"),
    ].iter().chain(files) {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let output = Command::new(env!("CARGO_BIN_EXE_roundhouse")).arg("check").args(args).arg(&root).output().unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

const LIB: &str = "class Lookup\n  def has?(value)\n    value.include?(1)\n  end\n\n  def loose(value)\n    T.unsafe(value).frobnicate\n  end\n\n  def known\n    [1].include?(1)\n  end\nend\n";

#[test]
fn library_sends_on_unknown_and_gradual_receivers_are_counted() {
    let text = check(&[], &[("app/lib/lookup.rb", LIB)]);
    assert!(
        text.contains("roundhouse-check: 1 send(s) on unknown receivers, 1 on gradual receivers"),
        "{text}"
    );
}

#[test]
fn each_unchecked_send_can_be_listed() {
    let text = check(&["--unknown-sends"], &[("app/lib/lookup.rb", LIB)]);
    assert!(text.contains("send `include?` on an unknown receiver is unchecked"), "{text}");
    assert!(text.contains("send `frobnicate` on a gradual (untyped) receiver is unchecked"), "{text}");
    // A typed receiver is checked, so it is not listed.
    assert_eq!(text.matches("is unchecked").count(), 2, "{text}");
}
