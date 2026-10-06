//! The CRuby tree loads app files the way Rails' autoloader does.
//!
//! With `config.eager_load` false for the environment (the blog's
//! test.rb: `ENV["CI"].present?`), a file loads when its constant is
//! first referenced, so a file whose class body raises at load does not
//! stop boot unless something names it. With it true every file loads at
//! boot and the same error surfaces there, as Rails' `eager_load!` does.
//! The decision is the app's own expression, evaluated at boot.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const NEVER_LOADED: &str = r#"module Broken
  class NeverLoaded
    TOKEN = Integer("not a number")
  end
end
"#;

const GREETER: &str = r#"module Podish
  module Discovery
    class Greeter
      def self.hello
        "hello from " + name
      end
    end
  end
end
"#;

const REPORT: &str = r#"loaded = -> { $LOADED_FEATURES.count { |f| f.include?("/app/models/podish/") } }
puts loaded.call
puts Podish::Discovery::Greeter.hello
puts loaded.call
puts $LOADED_FEATURES.any? { |f| f.end_with?("/app/models/broken/never_loaded.rb") }
"#;

fn emitted() -> emit_and_run::Emitted {
    let (emitted, errors) = emit_and_run::real_blog()
        .write("app/lib/broken/never_loaded.rb", NEVER_LOADED)
        .write("app/lib/podish/discovery/greeter.rb", GREETER)
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    emitted
}

fn run(emitted: &std::path::Path, ci: Option<&str>) -> std::process::Output {
    let main = emitted.join("main").display().to_string();
    let mut command = emit_and_run::ruby();
    command
        .arg("-e")
        .arg(format!("require {main:?}\nMain.configure_default_adapter!\n{REPORT}"))
        .current_dir(emitted)
        .env("BLOG_DB", ":memory:")
        .env("RAILS_ENV", "test")
        .env_remove("CI");
    if let Some(ci) = ci {
        command.env("CI", ci);
    }
    command.output().expect("spawn ruby")
}

#[test]
fn lazy_environment_loads_a_file_on_first_reference() {
    let emitted = emitted();
    let output = run(&emitted, None);
    assert!(
        output.status.success(),
        "emitted program failed in {}:\n{}",
        emitted.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "0\nhello from Podish::Discovery::Greeter\n1\nfalse\n");
}

#[test]
fn eager_environment_loads_every_file_at_boot() {
    let emitted = emitted();
    let output = run(&emitted, Some("1"));
    assert!(!output.status.success(), "eager boot should load broken/never_loaded.rb");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("never_loaded.rb") && stderr.contains("invalid value for Integer(): \"not a number\""),
        "{stderr}"
    );
}
