// ActiveSupport::Cache::MemCacheStore (runtime/spinel/
// active_support_mem_cache_store.rb) against activesupport 8.1.4, both on
// the dalli fork in Core's pinned gem home: tests/mem_cache_store_runtime.rb
// runs the same observations natively and on the runtime port and requires
// them equal. The live half runs against a memcached the script starts on
// a free localhost port when one is installed; without one the script
// says so and only the offline half (construction, options, keys, the
// entry coder, the down-server fallback) is compared.

use std::path::Path;
use std::process::Command;

#[test]
fn mem_cache_store_answers_like_activesupport() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new("ruby")
        .arg(root.join("tests/mem_cache_store_runtime.rb"))
        .arg("compare")
        .current_dir(root)
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("Native ActiveSupport and the emitted MemCacheStore agree"),
        "MemCacheStore diverged from activesupport\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    // Not a vacuous agreement: both sides observed the whole offline script,
    // and the live script too unless no memcached was found.
    let lines = stdout.lines().count();
    if stdout.contains("live: untested") {
        assert!(lines > 60, "too few offline observations:\n{stdout}");
    } else {
        assert!(lines > 95, "too few observations:\n{stdout}");
    }
}
