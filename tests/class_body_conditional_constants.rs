//! Constants a class body writes in the branches of an `if` / `else`,
//! Core's `AgenticStorefronts::TranslationPlatformApi`:
//!
//! ```ruby
//! module TranslationPlatformApi
//!   if Rails.env.test? || Rails.env.development?
//!     URL = "https://translation-platform.shop.dev/graphql"
//!   else
//!     URL = "https://translation-platform.shopify.io/graphql"
//!   end
//!   API = Cusco::Client::GraphQL.api(http: { url: URL, … })
//! ```
//!
//! The statement was emitted as written, but its writes were no entries
//! of the constant registry, so every read of `URL` (in the class body
//! and in a method) was "constant not supported" and raised at load.
//! Each branch's write now declares the constant; the branches' value
//! types join.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

const API: &str = r##"module PlatformApi
  TIMEOUT = 5.0
  if ENV.key?("PLATFORM_PROD")
    URL = "https://prod.example/graphql"
    HEADERS = [{ key: "Authorization", value: "Basic x" }].freeze
    RETRIES = nil
  else
    URL = "https://dev.example/graphql"
    HEADERS = [].freeze
    RETRIES = 2
  end

  CONFIG = { url: URL, headers: HEADERS, timeout: TIMEOUT }.freeze

  def self.url
    URL
  end

  def self.retries
    RETRIES || 0
  end
end
"##;

const SCRIPT: &str = r##"p PlatformApi.url
p PlatformApi::CONFIG
p PlatformApi.retries
p PlatformApi::HEADERS.size
"##;

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("app/models/platform_api.rb", API)
}

fn native(prod: bool) -> Vec<String> {
    let mut cmd = Command::new("ruby");
    cmd.arg("-e").arg(format!("{API}\n{SCRIPT}"));
    if prod {
        cmd.env("PLATFORM_PROD", "1");
    } else {
        cmd.env_remove("PLATFORM_PROD");
    }
    let out = cmd.output().expect("ruby is on PATH");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect()
}

#[test]
fn branch_written_constants_resolve_and_read_like_the_native_run() {
    for prod in [false, true] {
        let env: &[(&str, &str)] = if prod { &[("PLATFORM_PROD", "1")] } else { &[] };
        let run = app().run_ruby_env(SCRIPT, env);
        let constants: Vec<_> = run.errors.iter().filter(|e| e.contains("constant not supported")).collect();
        assert!(constants.is_empty(), "{constants:#?}");
        let emitted = std::fs::read_to_string(run.emitted.join("app/models/platform_api.rb")).expect("platform_api.rb");
        assert!(!emitted.contains("roundhouse:"), "{emitted}");
        run.assert_passes();
        let expected = native(prod);
        assert_eq!(run.stdout.lines().collect::<Vec<_>>(), expected, "prod={prod}\n{emitted}");
    }
}
