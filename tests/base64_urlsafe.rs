//! `Base64.urlsafe_encode64` / `urlsafe_decode64` in the ruby-family
//! runtime shim.
//!
//! The shim is deliberately grown on demand (its own header says so),
//! and campfire's QR-code helper is the demand:
//! `Base64.urlsafe_encode64(url)` in the helper, `urlsafe_decode64` in
//! the controller. Without them the constant resolved — we define
//! `Base64` ourselves, which correctly suppresses the bundled-library
//! require — but the method did not exist, and the spinel build stopped
//! there.
//!
//! Behaviour is pinned against CRuby's own stdlib rather than restated:
//! encode is byte-identical across all 256 byte values, and the shim
//! decodes what the stdlib encodes.

use std::process::Command;

#[test]
fn urlsafe_matches_the_cruby_stdlib() {
    let script = r#"
require "base64"
load "runtime/spinel/base64.rb"
inputs = [
  "https://example.com/rooms/1?a=b&c=d",
  "\xff\xfe\x00binary".b,
  "", "a", "ab", "abc",
  "campfire!~?/+",
  (0..255).map(&:chr).join.b,
]
bad = []
inputs.each do |s|
  mine   = Base64.urlsafe_encode64(s)
  theirs = ::Base64.urlsafe_encode64(s)
  bad << "encode #{s.inspect[0, 30]}" if mine != theirs
  bad << "roundtrip #{s.inspect[0, 30]}" if Base64.urlsafe_decode64(mine).b != s.b
  # and it must read what the stdlib wrote
  bad << "decode #{s.inspect[0, 30]}" if Base64.urlsafe_decode64(theirs).b != s.b
end
print bad.empty? ? "OK" : bad.join("; ")
"#;
    let out = Command::new("ruby")
        .arg("-e")
        .arg(script)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout.trim(),
        "OK",
        "stdout={stdout}\nstderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The whole stdlib surface, pinned against CRuby's results captured
/// BEFORE the shim loads (it reopens the same module, so a comparison
/// after `load` would compare the shim with itself). Core's JWT code
/// passes `urlsafe_encode64(bin, padding: false)`, decodes the unpadded
/// form back, and relies on `strict_decode64` raising on bad input.
#[test]
fn whole_surface_matches_stdlib_results_captured_before_load() {
    let script = r#"
require "base64"
inputs = ["", "a", "ab", "abc", "abcd", "\xff\xfe\x00binary".b,
          (0..255).map(&:chr).join.b, "x" * 46, "y" * 90, "z" * 91]
want = {}
inputs.each do |s|
  want[[:enc, s]] = Base64.encode64(s)
  want[[:strict, s]] = Base64.strict_encode64(s)
  want[[:url, s]] = Base64.urlsafe_encode64(s)
  want[[:url_nopad, s]] = Base64.urlsafe_encode64(s, padding: false)
end
decodes = ["", "YQ==", "YWI=", "YWJj", "YQ", "YWI", "Y Q=\n=", "YWJj\nZGVm\n",
           "YQ==YWI=", "AB==", "AB=", "AC==", "ABC=", "ABD=", "A", "AAA*", "-_-_", "+/+/"]
decodes.each do |s|
  want[[:dec, s]] = Base64.decode64(s)
  want[[:sdec, s]] = (Base64.strict_decode64(s) rescue :raise)
  want[[:udec, s]] = (Base64.urlsafe_decode64(s) rescue :raise)
end
load "runtime/spinel/base64.rb"
bad = []
inputs.each do |s|
  bad << "encode64 #{s.inspect[0, 20]}" if Base64.encode64(s) != want[[:enc, s]]
  bad << "strict #{s.inspect[0, 20]}" if Base64.strict_encode64(s) != want[[:strict, s]]
  bad << "url #{s.inspect[0, 20]}" if Base64.urlsafe_encode64(s) != want[[:url, s]]
  nopad = Base64.urlsafe_encode64(s, padding: false)
  bad << "url_nopad #{s.inspect[0, 20]}" if nopad != want[[:url_nopad, s]]
  bad << "nopad alias #{s.inspect[0, 20]}" if Base64.urlsafe_encode64_nopad(s) != nopad
  bad << "nopad roundtrip #{s.inspect[0, 20]}" if Base64.urlsafe_decode64(nopad).b != s.b
end
decodes.each do |s|
  bad << "decode64 #{s.inspect}" if Base64.decode64(s).b != want[[:dec, s]].b
  got = (Base64.strict_decode64(s) rescue :raise)
  bad << "strict_decode64 #{s.inspect}" if (got == :raise ? got : got.b) != (want[[:sdec, s]] == :raise ? :raise : want[[:sdec, s]].b)
  got = (Base64.urlsafe_decode64(s) rescue :raise)
  bad << "urlsafe_decode64 #{s.inspect}" if (got == :raise ? got : got.b) != (want[[:udec, s]] == :raise ? :raise : want[[:udec, s]].b)
end
print bad.empty? ? "OK" : bad.join("; ")
"#;
    let out = Command::new("ruby")
        .arg("-e")
        .arg(script)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout.trim(),
        "OK",
        "stdout={stdout}\nstderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}
