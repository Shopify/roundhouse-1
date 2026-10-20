//! A path built from `__FILE__`'s directory resolves through every
//! directory it names. The Ruby family relocates `app/lib/widgets/loader.rb`
//! to `app/models/widgets/loader.rb`, so the source's `app/lib/widgets/`
//! holds no file in the tree and nothing created it; the kernel cannot
//! resolve the `..` of `File.join(File.dirname(__FILE__), "..", "data",
//! "loader.json")` through a directory that is not there (web_pixels_manager
//! 0.52.0's `LOADER_CODE`). Native Ruby is the oracle.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const LOADER: &str = r#"module Widgets
  module Loader
    PATH = File.join(File.dirname(__FILE__), "..", "data", "loader.json")
    CODE = File.read(PATH)
  end
end
"#;

const DATA: &str = "{\"loader\": \"found-beside-the-source\"}\n";

const REPORT: &str = "puts Widgets::Loader::CODE.strip\n";

fn elsewhere(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().canonicalize().unwrap().join(format!("roundhouse-srcdir-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn native_ruby_reads_the_data_beside_the_source() {
    let app = elsewhere("native-app");
    for (path, text) in [("app/lib/widgets/loader.rb", LOADER), ("app/lib/data/loader.json", DATA)] {
        let full = app.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
    let output = std::process::Command::new("ruby")
        .arg("-e")
        .arg(format!("require {:?}\n{REPORT}", app.join("app/lib/widgets/loader").display().to_string()))
        .output()
        .expect("native Ruby control");
    let _ = std::fs::remove_dir_all(&app);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "{\"loader\": \"found-beside-the-source\"}");
}

#[test]
fn emitted_ruby_reads_the_data_beside_the_relocated_source() {
    let (emitted, errors) = emit_and_run::real_blog()
        .write("app/lib/widgets/loader.rb", LOADER)
        .write("app/lib/data/loader.json", DATA)
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    assert!(emitted.join("app/models/widgets/loader.rb").exists(), "the source is relocated");
    assert!(
        emitted.join("app/lib/widgets").is_dir(),
        "the source's own directory exists in the tree, so `..` resolves through it"
    );
    let main = emitted.join("main").display().to_string();
    let output = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("require {main:?}\nMain.configure_default_adapter!\n{REPORT}"))
        .env("BLOG_DB", ":memory:")
        .output()
        .expect("spawn ruby");
    assert!(
        output.status.success(),
        "emitted program failed in {}:\n{}",
        emitted.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "{\"loader\": \"found-beside-the-source\"}");
}
