//! `__FILE__` and `__dir__` name the loaded file's absolute location, so
//! a path built from them does not depend on the process's cwd. Core's
//! `BootLib::ROOT = File.expand_path("../../../../..", __FILE__)` finds
//! `config/secrets.*.json` that way. The Ruby family emits
//! `app/lib/locator.rb` as `app/models/locator.rb`, so the emitted
//! literal is the source's app-relative path anchored on the emitted
//! file's own `__dir__`: the app root maps to the output root, where the
//! carried config files are.
//!
//! Native Ruby is the oracle. Both runs start from a cwd outside the app.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const LOCATOR: &str = r#"module Locator
  ROOT = File.expand_path("../../..", __FILE__)
  HERE = __dir__

  def self.file
    __FILE__
  end

  def self.marker
    JSON.parse(File.read(File.join(ROOT, "config", "locator.json")))["marker"]
  end
end
"#;

const LOCATOR_JSON: &str = "{\"marker\": \"found-from-root\"}\n";

const REPORT: &str = r#"puts Locator.marker
puts Locator.file.start_with?("/")
puts File.expand_path("app/lib/locator.rb", Locator::ROOT) == Locator.file
puts Locator::HERE == File.dirname(Locator.file)
puts Dir.pwd != Locator::ROOT
"#;

const EXPECTED: &str = "found-from-root\ntrue\ntrue\ntrue\ntrue\n";

fn elsewhere(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("roundhouse-cwd-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn native_ruby_answers_from_another_cwd() {
    let app = elsewhere("native-app");
    for (path, text) in [("app/lib/locator.rb", LOCATOR), ("config/locator.json", LOCATOR_JSON)] {
        let full = app.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
    let cwd = elsewhere("native-cwd");
    let output = std::process::Command::new("ruby")
        .arg("-rjson")
        .arg("-e")
        .arg(format!("require {:?}\n{REPORT}", app.join("app/lib/locator").display().to_string()))
        .current_dir(&cwd)
        .output()
        .expect("native Ruby control");
    let _ = std::fs::remove_dir_all(&app);
    let _ = std::fs::remove_dir_all(&cwd);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8_lossy(&output.stdout), EXPECTED);
}

#[test]
fn emitted_ruby_answers_the_same_from_another_cwd() {
    let (emitted, errors) = emit_and_run::real_blog()
        .write("app/lib/locator.rb", LOCATOR)
        .write("config/locator.json", LOCATOR_JSON)
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    let cwd = elsewhere("emitted-cwd");
    let main = emitted.join("main").display().to_string();
    let output = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("require {main:?}\nMain.configure_default_adapter!\n{REPORT}"))
        .current_dir(&cwd)
        .env("BLOG_DB", ":memory:")
        .output()
        .expect("spawn ruby");
    let _ = std::fs::remove_dir_all(&cwd);
    assert!(
        output.status.success(),
        "emitted program failed in {}:\n{}",
        emitted.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), EXPECTED);
}
