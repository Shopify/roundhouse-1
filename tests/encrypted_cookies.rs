//! `cookies.encrypted` (ruby_overlay/runtime/encrypted_cookies_cruby.rb) is
//! Rails' encrypted cookie: what the jar writes, Core's pinned activesupport
//! decrypts with the application's key generator (AES-256-GCM, the
//! "authenticated encrypted cookie" salt, the cookie name as purpose), what
//! it writes the jar reads, and a cookie that is absent, forged or minted
//! for another name reads as nil.

use std::path::Path;
use std::process::Command;

const SECRET: &str = "4f1569197865520b4efb0dbb9a7bb12b99866504d5f06ed7f179032712e6663cd136a07787128a2266d9256f4a75768af02f97146468feb72abd9c2d30533c0c";

/// The oracle re-executes its script under jemalloc, which a `-e` program
/// cannot survive: the script goes to a file. `RH_ROOT` is the checkout.
fn ruby(root: &Path, name: &str, script: &str, args: &[&str]) -> String {
    let dir = std::env::temp_dir().join(format!("rh_encrypted_cookies_{}_{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.rb"));
    std::fs::write(&file, script).unwrap();
    let out = Command::new("ruby")
        .arg("-W0")
        .arg(&file)
        .args(args)
        .env("RH_ROOT", root)
        .current_dir(root)
        .env("GLIBC_TUNABLES", "glibc.rtld.optional_static_tls=262144")
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    std::fs::remove_dir_all(&dir).ok();
    assert!(out.status.success(), "ruby failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    stdout
}

/// Our jar, over the tree's messages port, with Core's marshal serializer.
const OURS: &str = r##"
require "openssl"; require "json"; require "base64"
require File.join(ENV.fetch("RH_ROOT"), "runtime/spinel/active_support_messages")
secret = ARGV[0]
module Rails
  Options = Struct.new(:cookies_serializer)
  class Config; def action_dispatch; @options ||= Options.new(:marshal); end; end
  class App
    attr_accessor :secret_key_base
    def config; @config ||= Config.new; end
  end
  def self.application; @app ||= App.new; end
end
Rails.application.secret_key_base = secret
require File.join(ENV.fetch("RH_ROOT"), "runtime/ruby/action_controller/cookies")
require File.join(ENV.fetch("RH_ROOT"), "runtime/spinel/scaffold/ruby_overlay/runtime/encrypted_cookies_cruby")

def check(label)
  raise "#{label}: #{yield.inspect}" unless yield == true
end

json = { "email" => "someone@shopify.com", "shipping_address" => { "country" => "CA" } }.to_json
jar = ActionController::CookieJar.new({})
jar.encrypted[:checkout_params] = json
check("round trip") { jar.encrypted[:checkout_params] == json }
check("string key") { jar.encrypted["checkout_params"] == json }
check("absent") { jar.encrypted[:nothing].nil? }
check("not plaintext") { !jar.raw(:checkout_params).include?("someone") }
check("written to the pending map") { jar.pending.key?("checkout_params") }

# Another cookie's value, a forged one, and a plain one read as nil.
other = ActionController::CookieJar.new({ "stolen" => jar.raw(:checkout_params), "plain" => json })
check("another name") { other.encrypted[:stolen].nil? }
check("plain value") { other.encrypted[:plain].nil? }
forged = jar.raw(:checkout_params).sub(/.\z/) { |c| c == "a" ? "b" : "a" }
check("forged") { ActionController::CookieJar.new({ "checkout_params" => forged }).encrypted[:checkout_params].nil? }
check("same name, inbound") { ActionController::CookieJar.new({ "checkout_params" => jar.raw(:checkout_params) }).encrypted[:checkout_params] == json }

# Options form and permanent.
jar.encrypted[:flagged] = { value: "v", httponly: false, same_site: :lax }
check("options value") { jar.encrypted[:flagged] == "v" }
check("httponly opt-out") { !jar.flag_httponly?(:flagged) }
jar.permanent.encrypted[:long] = "keep"
check("permanent") { jar.encrypted[:long] == "keep" && jar.flag_expires(:long) != "" }
jar.encrypted.delete(:long)
check("deleted") { jar.encrypted[:long].nil? }
check("permanent view") { jar.permanent.encrypted.is_a?(ActionController::EncryptedCookieJar) }

# A cookie a client of ours sends back: the value a native Rails minted.
native = ARGV[1].to_s
unless native.empty?
  name, value = native.split("=", 2)
  check("native cookie") { ActionController::CookieJar.new({ name => value }).encrypted[name] == json }
end
puts jar.raw(:checkout_params)
puts "ALL OK"
"##;

/// Core's pinned activesupport: the application's key generator
/// (`config/application.rb`: iterations 1000), MessageEncryptor over it.
const NATIVE: &str = r##"
require File.join(ENV.fetch("RH_ROOT"), "tests/support/rails_oracle.rb")
RailsOracle.activate("activesupport")
require "active_support"
require "active_support/message_encryptor"
require "active_support/key_generator"
require "json"
secret, mode, payload = ARGV
generator = ActiveSupport::KeyGenerator.new(secret, iterations: 1000)
key = generator.generate_key("authenticated encrypted cookie", 32)
encryptor = ActiveSupport::MessageEncryptor.new(
  key, cipher: "aes-256-gcm", serializer: ActiveSupport::Messages::SerializerWithFallback[:marshal]
)
json = { "email" => "someone@shopify.com", "shipping_address" => { "country" => "CA" } }.to_json
case mode
when "decrypt"
  value = encryptor.decrypt_and_verify(payload, purpose: "cookie.checkout_params")
  abort "native decrypted #{value.inspect}" unless value == json
  puts "NATIVE OK"
when "encrypt"
  puts "checkout_params=" + encryptor.encrypt_and_sign(json, purpose: "cookie.checkout_params")
end
"##;

#[test]
fn encrypted_cookies_are_rails_encrypted_cookies() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    // Minted by Core's activesupport, read by the jar.
    let native_cookie = ruby(root, "native_encrypt", NATIVE, &[SECRET, "encrypt"]).trim().to_string();
    assert!(native_cookie.starts_with("checkout_params="), "{native_cookie}");
    // Minted by the jar, read by Core's activesupport.
    let ours = ruby(root, "ours", OURS, &[SECRET, &native_cookie]);
    assert!(ours.contains("ALL OK"), "{ours}");
    let minted = ours.lines().next().unwrap();
    let decrypted = ruby(root, "native_decrypt", NATIVE, &[SECRET, "decrypt", minted]);
    assert!(decrypted.contains("NATIVE OK"), "{decrypted}");
}
