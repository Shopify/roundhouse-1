//! The native side of a PostgreSQL probe test: writes the probe app's
//! files, runs `tests/support/pg_probe_driver.rb` over them and the
//! emitted tree, and answers the lines each printed.

use std::path::{Path, PathBuf};
use std::process::Command;

/// `(native, emitted)` lines of `probe.lines`, each run on a fresh
/// `database`. `loads` are the app files native Rails requires, in order.
pub fn native_and_emitted(
    name: &str,
    files: &[(&str, &str)],
    emitted: &Path,
    database: &str,
    probe: &str,
    loads: &[&str],
) -> (Vec<String>, Vec<String>) {
    assert!(std::env::var_os("ROUNDHOUSE_PG_TEST_URL").is_some(), "set ROUNDHOUSE_PG_TEST_URL");
    let native_root: PathBuf = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-native"));
    let _ = std::fs::remove_dir_all(&native_root);
    for (path, text) in files {
        let full = native_root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
    // The pg gem and Rails' native extensions come from Core's gem home,
    // as the oracle (tests/support/rails_oracle.rb) loads them.
    let gem_home = std::env::var("CORE_GEM_HOME")
        .unwrap_or_else(|_| "/nix/store/kr4swvh5mw01fs3kixfa9afy57zsyihg-ruby-gems-dev/lib/ruby/gems/4.0.0".into());
    let jemalloc = std::env::var("RH_JEMALLOC").unwrap_or_else(|_| {
        "/nix/store/c04j2i0fkin1bw5fab35gx896558knsd-jemalloc-5.3.0-unstable-2025-09-12/lib/libjemalloc.so.2".into()
    });
    let result = Command::new("ruby")
        .arg("tests/support/pg_probe_driver.rb")
        .arg(&native_root)
        .arg(emitted)
        .arg(database)
        .arg(probe)
        .args(loads)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("GEM_HOME", &gem_home)
        .env("GEM_PATH", &gem_home)
        .env("LD_PRELOAD", &jemalloc)
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(result.status.success(), "stdout={stdout}\nstderr={}", String::from_utf8_lossy(&result.stderr));
    let answers: serde_json::Value = serde_json::from_str(stdout.trim()).expect("driver prints JSON");
    let lines = |side: &str| -> Vec<String> {
        let run = &answers[side];
        assert_eq!(run["ok"], true, "{side} run failed:\n{}", run["stderr"].as_str().unwrap_or(""));
        run["lines"].as_array().unwrap().iter().map(|l| l.as_str().unwrap().to_string()).collect()
    };
    (lines("native"), lines("emitted"))
}
