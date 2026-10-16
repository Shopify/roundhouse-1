//! `cookies.delete(name, options)` takes Rails' options Hash.
//!
//! Core's `ClientDetails` clears its shared discount-code cookie with
//! `jar.delete(NAME, domain: :all)`; the jar's one-argument `delete` died
//! `wrong number of arguments (given 2, expected 1)`.

use std::process::Command;

const SCRIPT: &str = r#"
require "./runtime/ruby/action_controller/cookies"
jar = ActionController::CookieJar.new
jar["plain"] = "1"
jar.delete(:plain, domain: :all)
jar.signed.delete(:signed, domain: :all)
jar.permanent.delete(:forever, path: "/")
jar.delete("bare")
expected = { "plain" => "", "signed" => "", "forever" => "", "bare" => "" }
raise "pending #{jar.pending.inspect}" unless jar.pending == expected
puts "cookie delete options passed"
"#;

#[test]
fn delete_accepts_an_options_hash_on_every_jar() {
    let out = Command::new("ruby")
        .args(["-W0", "-e", SCRIPT])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("ruby is on PATH");
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
