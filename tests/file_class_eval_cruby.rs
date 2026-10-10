//! A file-level `class_eval` the file program carries as Ruby runs it.
//!
//! Core's essentials has two the program named `file_statement_
//! unsupported` instead: shopify_types_no_sig_eager_load_in_isolation.rb
//! wraps `Shopify::Types::Configuration.singleton_class.class_eval do def
//! run_all_sig_blocks! … end end` in a file-level `if` (a singleton-class
//! receiver, and a branch holding no class or module), and
//! binary_flag_store_migration_opt_in.rb writes `ActiveSupport::TestCase.
//! class_eval do setup do … end teardown do … end end` (a body of calls
//! alone, which declared no module for the body). The emitted tree must
//! print what CRuby prints.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

use roundhouse::project::BuildTarget;

const SETTINGS: &str = r#"module Settings
  #: () -> String
  def self.mode = "default"
end
"#;

const SETTINGS_PATCH: &str = r#"if ENV.fetch("RH_PATCH", "on") == "on"
  Settings.singleton_class.class_eval do
    def mode = "patched"
  end
end
"#;

const HOOKED: &str = r#"class Hooked
  #: () { () -> String } -> Array[untyped]
  def self.setup(&block)
    (@setups ||= []) << block
  end

  #: () -> Array[untyped]
  def self.setups
    @setups || []
  end
end
"#;

const HOOKED_PATCH: &str = r#"Hooked.class_eval do
  setup do
    "first"
  end

  setup { "second" }
end
"#;

const SCRIPT: &str = r#"
puts Settings.mode
puts Hooked.setups.map(&:call).inspect
"#;

const EXPECTED: [&str; 2] = ["patched", r#"["first", "second"]"#];

fn lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes).lines().map(str::to_string).collect()
}

#[test]
fn file_level_class_eval_runs_as_in_cruby() {
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("{SETTINGS}\n{SETTINGS_PATCH}\n{HOOKED}\n{HOOKED_PATCH}\n{SCRIPT}"))
        .output()
        .expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(lines(&native.stdout), EXPECTED, "native control");

    let (tree, errors) = emit_and_run::empty_app()
        .write("lib/settings.rb", SETTINGS)
        .write("lib/settings_patch.rb", SETTINGS_PATCH)
        .write("lib/hooked.rb", HOOKED)
        .write("lib/hooked_patch.rb", HOOKED_PATCH)
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let run = Command::new("ruby")
        .current_dir(&tree)
        .env("BLOG_DB", ":memory:")
        .arg("-e")
        .arg(format!("require File.expand_path(\"main\", Dir.pwd)\nMain.configure_default_adapter!\n{SCRIPT}"))
        .output()
        .expect("spawn ruby");
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "emitted tree failed in {}: {stderr}", tree.display());
    assert!(!stderr.contains("file_statement_not_carried"), "{stderr}");
    assert_eq!(lines(&run.stdout), EXPECTED, "emitted Ruby diverges from CRuby in {}", tree.display());
}

/// essentials' patches/action_view_ext.rb reopens `ActionView::Helpers::
/// AssetUrlHelper`, which the runtime does not define. Its `module_eval`
/// was written bare, so the NameError ended the file and the engine's
/// `lib/patches/**` glob that required it, and none of the patches after
/// it (the MessageEncryptor key-length one among them) loaded. Under
/// `file_statement` it is named and the file's next statement still runs.
const PARTIAL_PATCH: &str = r#"Absent::Helper.module_eval do
  def compute_asset_path(source) = source
end

Settings.singleton_class.class_eval do
  def mode = "patched"
end
"#;

#[test]
fn a_file_level_class_eval_on_an_absent_receiver_is_named_and_the_file_goes_on() {
    let (tree, _errors) = emit_and_run::empty_app()
        .write("lib/settings.rb", SETTINGS)
        .write("lib/partial_patch.rb", PARTIAL_PATCH)
        .emit(BuildTarget::Ruby);
    let run = Command::new("ruby")
        .current_dir(&tree)
        .env("BLOG_DB", ":memory:")
        .arg("-e")
        .arg("require File.expand_path(\"main\", Dir.pwd)\nMain.configure_default_adapter!\nputs Settings.mode")
        .output()
        .expect("spawn ruby");
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "emitted tree failed in {}: {stderr}", tree.display());
    assert!(
        stderr.contains("error[file_statement_not_run]") && stderr.contains("lib/partial_patch.rb:1") && stderr.contains("Absent"),
        "{stderr}"
    );
    assert_eq!(lines(&run.stdout), ["patched"], "emitted tree in {}", tree.display());
}
