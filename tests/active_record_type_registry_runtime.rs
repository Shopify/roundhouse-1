// The type registries (the end of runtime/spinel/active_model_type.rb:
// ActiveModel::Type.register/lookup, ActiveRecord::Type.register/lookup/
// add_modifier with adapter-specific registrations) against activemodel /
// activerecord 8.2.0.alpha (Core's pinned Shopify/rails 5813559f, from
// CORE_GEM_HOME): tests/active_record_type_registry_runtime.rb runs the same
// observations natively and on the port and requires every one equal.

use std::path::Path;
use std::process::Command;

#[test]
fn type_registries_answer_like_activerecord() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new("ruby")
        .arg(root.join("tests/active_record_type_registry_runtime.rb"))
        .arg("compare")
        .current_dir(root)
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("Native and emitted type registries agree"),
        "type registries diverged from activerecord\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    // Not a vacuous agreement: the adapter, override, block, modifier and
    // conflict paths all ran.
    for seen in [
        "probe_uuid on postgresql: ProbeType(args=[], kwargs={})",
        "probe_uuid on trilogy: ActiveModel::Type::BigInteger",
        "probe_uuid on sqlite3: ArgumentError: Unknown type :probe_uuid",
        "probe_block: ProbeType(args=[:probe_block, 1], kwargs={limit: 8})",
        "ActiveRecord::TypeConflictError: Type probe_any was registered for all adapters, but shadows a native type with the same name for sqlite3",
        "text override: false on postgresql: ProbeType",
        "big_integer array on postgresql: ProbeArray(ActiveModel::Type::BigInteger",
        "late in copy: ArgumentError: Unknown type :probe_late",
        "AR integer limit: 2: ActiveModel::Type::Integer(type=:integer, limit=2",
    ] {
        assert!(stdout.contains(seen), "missing observation {seen:?}\n{stdout}");
    }
}
