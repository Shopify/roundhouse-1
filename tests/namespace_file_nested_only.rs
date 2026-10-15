//! A file at an autoload root's path for a namespace (`app/utils/crypto.rb`
//! for `Crypto`) whose module declares only nested constants
//! (`Crypto::InvalidSignature`, `Crypto::Symmetric::Encryption`) is the
//! namespace's autoload, as Zeitwerk makes it. Core's
//! components/platform/essentials/app/utils/crypto.rb was registered as
//! no constant's autoload (each declaration named a path no file had),
//! and under a lazy boot it never loaded: `SecurityTokenManager#initialize`
//! raised `uninitialized constant SecurityTokenManager::Crypto`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const LAZY: &str = "Rails.application.configure do\n  config.eager_load = false\nend\n";

const CRYPTO: &str = "require \"openssl\"\n\nmodule Crypto\n  class InvalidSignature < StandardError; end\n\n  module Symmetric\n    class Encryption\n      def initialize(key)\n        @key = key\n      end\n\n      def describe\n        \"enc:#{@key}\"\n      end\n    end\n  end\nend\n";

#[test]
fn a_namespace_file_declaring_only_nested_constants_loads_on_the_namespaces_first_reference() {
    let run = emit_and_run::empty_app()
        .write("config/environments/development.rb", LAZY)
        .write("app/utils/crypto.rb", CRYPTO)
        // A nested constant of the same name elsewhere must not stand in for it.
        .write("app/models/captcha/crypto.rb", "module Captcha\n  class Crypto\n    def self.tag\n      \"captcha\"\n    end\n  end\nend\n")
        .write(
            "app/models/token_manager.rb",
            "class TokenManager\n  def initialize\n    @enc = Crypto::Symmetric::Encryption.new(\"k\")\n  end\n\n  def describe\n    @enc.describe\n  end\nend\n",
        )
        .run_ruby(
            "puts TokenManager.new.describe\n\
             puts Captcha::Crypto.tag\n\
             p Crypto::InvalidSignature.ancestors.include?(StandardError)\n",
        );
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "enc:k\ncaptcha\ntrue\n", "{}", run.stderr);
}
