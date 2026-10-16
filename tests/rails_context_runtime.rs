// `Rails.context` on the ruby-family trees (runtime/spinel/rails_configuration.rb).
// Core gets it from the gurgel gem (`Gurgel::RailsContextExtension#context`):
// the name of the RAILS_CONTEXT context ("all" by default) as an inquirer.
// Before, `Rails.context` was a NoMethodError, so every Core controller test
// that boots application.rb's `prune_autoload_paths` initializer died on
// `Rails.context.essentials?` (the 8 Admin::OrderRisksControllerTest ids).

use std::path::Path;
use std::process::Command;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn rails_context(env: Option<&str>) -> String {
    let script = r#"
require "./runtime/ruby/rails"
$LOADED_FEATURES << File.expand_path("runtime/spinel/rails.rb")
require "./runtime/spinel/rails_railtie"
require "./runtime/spinel/active_support_string_inquirer"
require "./runtime/spinel/rails_configuration"
c = Rails.context
puts [c.inspect, c.all?, c.essentials?, c.server_intents?, c != "essentials", c == "all",
      Rails.context.equal?(c), c.is_a?(ActiveSupport::StringInquirer)].join(" ")
"#;
    let mut cmd = Command::new("ruby");
    cmd.arg("-e").arg(script).current_dir(root()).env_remove("RAILS_CONTEXT");
    if let Some(value) = env {
        cmd.env("RAILS_CONTEXT", value);
    }
    let out = cmd.output().expect("ruby is on PATH");
    assert!(
        out.status.success(),
        "ruby failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn rails_context_defaults_to_all() {
    assert_eq!(rails_context(None), "\"all\" true false false true true true true");
    // An empty RAILS_CONTEXT is no selection: the default applies.
    assert_eq!(rails_context(Some("")), "\"all\" true false false true true true true");
}

#[test]
fn rails_context_answers_the_selected_context() {
    assert_eq!(
        rails_context(Some("essentials")),
        "\"essentials\" false true false false false true true"
    );
    assert_eq!(
        rails_context(Some("server_intents")),
        "\"server_intents\" false false true true false true true"
    );
}
