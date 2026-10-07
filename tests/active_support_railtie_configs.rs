//! `config.load_defaults` and `config.active_support.{cache_format_version,
//! hash_digest_class}` reach the ActiveSupport::Cache port the way
//! activesupport's railtie initializers apply them (`set_configs`,
//! `set_hash_digest_class`). Shopify Core writes `load_defaults(8.1)` and
//! then `hash_digest_class = OpenSSL::Digest::SHA1`: cache format 7.1,
//! SHA1 key digests.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

fn app(application_body: &str) -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write(
            "config/application.rb",
            &format!("module Shop\n  class Application < Rails::Application\n{application_body}  end\nend\n"),
        )
        // Names the cache, so the tree carries the port.
        .write("lib/cache_box.rb", "class CacheBox\n  def store\n    ActiveSupport::Cache::MemoryStore.new\n  end\nend\n")
}

/// The emitted tree's boot, then the two settings as the port holds them.
fn booted_settings(overlay: emit_and_run::Overlay) -> String {
    let (dir, errors) = overlay.emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let out = std::process::Command::new("ruby")
        .current_dir(&dir)
        .args([
            "-e",
            "require_relative 'boot'; puts ActiveSupport::Cache.format_version.inspect; puts ActiveSupport::Digest.hash_digest_class.name",
        ])
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "{stdout}{}", String::from_utf8_lossy(&out.stderr));
    stdout.lines().rev().take(2).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")
}

#[test]
fn load_defaults_then_an_explicit_digest_class_is_cache_format_7_1_with_sha1() {
    let got = booted_settings(app(
        "    config.load_defaults(8.1)\n    config.active_support.hash_digest_class = OpenSSL::Digest::SHA1\n",
    ));
    assert_eq!(got, "7.1\nOpenSSL::Digest::SHA1");
}

#[test]
fn load_defaults_alone_is_its_cascade() {
    assert_eq!(booted_settings(app("    config.load_defaults 7.0\n")), "7.0\nOpenSSL::Digest::SHA256");
}

#[test]
fn no_load_defaults_leaves_the_library_defaults() {
    assert_eq!(booted_settings(app("    config.time_zone = \"UTC\"\n")), "7.0\nOpenSSL::Digest::MD5");
}
