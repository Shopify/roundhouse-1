//! ActiveSupport's `core_ext/hash/keys.rb` in the CRuby overlay
//! (runtime/spinel/scaffold/ruby_overlay/runtime/active_support_core_ext.rb).
//! activemerchant 1.137.0 requires `hash/indifferent_access`, which loads
//! it natively, and `Billing::Response#initialize` calls
//! `params.stringify_keys` on an untyped Hash: the emitted tree raised
//! NoMethodError on the first gateway response.
//!
//! The control is CRuby with the pinned ActiveSupport
//! (tests/support/rails_oracle.rb); the overlay alone must print the same.

use std::process::Command;

const ROOT: &str = env!("CARGO_MANIFEST_DIR");

const SCRIPT: &str = r##"h = { a: 1, "b" => { c: [{ d: 2 }] }, 3 => :x }
p [h.stringify_keys, h.symbolize_keys, h.to_options, h.deep_stringify_keys, h.deep_symbolize_keys]
p h.deep_transform_keys { |k| k.to_s.upcase }
g = h.dup; p [g.stringify_keys!.equal?(g), g]
g = h.dup; p [g.symbolize_keys!, g.to_options!]
g = { "x" => { "y" => 1 } }; g.deep_symbolize_keys!; p g
g = { x: [{ y: 1 }] }; g.deep_stringify_keys!; p g
p({ a: 1 }.assert_valid_keys(:a, [:b]))
begin; { a: 1, z: 2 }.assert_valid_keys(:a); rescue ArgumentError => e; p e.message; end
w = { a: 1, "b" => 2 }.with_indifferent_access
p [w.stringify_keys.class, w.symbolize_keys, w.to_options!.equal?(w), w.respond_to?(:symbolize_keys!), w.respond_to?(:deep_symbolize_keys!), w.deep_symbolize_keys]
p [{}.respond_to?(:_deep_transform_keys_in_object), {}.private_methods.include?(:_deep_transform_keys_in_object!)]
"##;

/// Runs from a file: the oracle re-executes its script to pin the gems.
fn ruby(label: &str, prelude: &str) -> String {
    let dir = std::env::temp_dir().join(format!("rh_hash_keys_overlay_{}_{label}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.rb"), format!("{prelude}\n{SCRIPT}")).unwrap();
    let out = Command::new("ruby").arg("main.rb").current_dir(&dir).output().expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{label} failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    stdout
}

#[test]
fn the_overlay_s_hash_key_conversions_answer_as_active_support() {
    let native = ruby("native", &format!(
        "require {:?}\nRailsOracle.activate(\"activesupport\")\nrequire \"active_support\"\n\
         require \"active_support/core_ext/hash/indifferent_access\"",
        format!("{ROOT}/tests/support/rails_oracle.rb")
    ));
    assert!(native.contains("\"Unknown key: :z. Valid keys are: :a\""), "native control: {native}");
    let overlay = ruby("overlay", &format!(
        "require {:?}",
        format!("{ROOT}/runtime/spinel/scaffold/ruby_overlay/runtime/active_support_core_ext")
    ));
    assert_eq!(overlay, native);
}
