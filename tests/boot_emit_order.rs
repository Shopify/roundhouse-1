//! Declarations load in the order Ruby evaluates them. Boot blockers
//! from Core's e2e boot (r16-pgb6), each checked here against native
//! Ruby running the same file.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

fn native(files: &[(&str, &str)], script: &str) -> String {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("rh_boot_emit_order_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (path, text) in files {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let output = emit_and_run::ruby().arg("-I").arg(root.join("lib")).arg("-e").arg(script).output().unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Core's oid_service.rb declares `module Validation` above
/// `class Local`, which does `extend(Validation)`. The file's
/// declarations were loaded classes first at each depth, so Local's
/// body ran before Validation existed (NameError at boot).
const OID_SERVICE: &str = "require \"socket\"\n\nmodule OidService\n  class Error < StandardError; end\n\n  SCHEMA_NAME = \"core\"\n\n  module Validation\n    MAX_COUNT = 65535\n\n    def check(n) = n <= MAX_COUNT\n  end\n\n  class Local\n    extend(Validation)\n\n    def self.ok = check(3)\n  end\nend\n";

#[test]
fn a_module_declared_above_a_class_loads_before_it() {
    let script = "p OidService::Local.ok\np OidService::Error.superclass\n";
    let expected = native(&[("lib/oid_service.rb", OID_SERVICE)], &format!("require \"oid_service\"\n{script}"));
    assert_eq!(expected, "true\nStandardError\n");
    let run = emit_and_run::empty_app().write("lib/oid_service.rb", OID_SERVICE).run_ruby(script);
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, expected, "{}", run.stderr);
}

