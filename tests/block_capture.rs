//! A block's or lambda's own block parameter (`|*args, &blk|`,
//! `->(a, &blk)`) is kept.
//!
//! Core's `Podding::Database::ConnectionProxy#method_missing` forwards
//! the caller's block through `do |*inner_args, &inner_block| …
//! block.call(*inner_args, &inner_block)`. Ingest refused the capture
//! (dropping it would have let the body read an outer local of the same
//! name), and the support-dir walk dropped the whole file. The capture
//! now rides on the `Lambda`, shadows the outer name, and the Ruby
//! emitter writes it back; targets whose closures have no slot for it
//! refuse it by name. Native Ruby on the same source is the oracle.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const RELAY: &str = r#"module Relay
  def self.with_block(&outer)
    outer.call(1, 2) { |v| v * 10 }
  end

  def self.forwards
    with_block do |*args, &inner|
      args.sum + inner.call(3)
    end
  end

  def self.shadows
    inner = :outer
    seen = [1].map { |x, &inner| inner.nil? }
    [inner, seen]
  end

  def self.lambda_capture
    f = ->(a, &blk) { blk ? blk.call(a) : :none }
    [f.call(2) { |v| v + 1 }, f.call(2)]
  end
end
"#;

const REPORT: &str = "p Relay.forwards\np Relay.shadows\np Relay.lambda_capture\n";

#[test]
fn emitted_block_capture_matches_native_ruby() {
    let dir = std::env::temp_dir().join(format!("roundhouse-block-capture-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("relay.rb");
    std::fs::write(&file, RELAY).unwrap();
    let native = std::process::Command::new("ruby")
        .arg("-e")
        .arg(format!("require {:?}\n{REPORT}", file.display().to_string()))
        .output()
        .expect("native Ruby control");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = String::from_utf8_lossy(&native.stdout).into_owned();
    assert_eq!(expected, "33\n[:outer, [true]]\n[3, :none]\n", "native control");

    let (emitted, errors) = emit_and_run::real_blog().write("app/lib/relay.rb", RELAY).emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    let main = emitted.join("main").display().to_string();
    let output = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("require {main:?}\nMain.configure_default_adapter!\n{REPORT}"))
        .current_dir(&emitted)
        .env("BLOG_DB", ":memory:")
        .output()
        .expect("spawn ruby");
    assert!(
        output.status.success(),
        "emitted program failed in {}:\n{}",
        emitted.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), expected);
}

#[test]
fn a_target_without_a_slot_for_it_refuses_the_capture() {
    let (_, errors) = emit_and_run::real_blog().write("app/lib/relay.rb", RELAY).emit(BuildTarget::Spinel);
    let text = errors.join("\n");
    assert!(text.contains("block parameter `&inner` of a block or lambda is not lowered for spinel"), "{text}");
    assert!(text.contains("block parameter `&blk` of a block or lambda is not lowered for spinel"), "{text}");
}
