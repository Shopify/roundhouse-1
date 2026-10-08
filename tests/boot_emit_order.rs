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

/// Core's gems/component: `Component::Config.inherited` autoloads
/// `Config::Test` from `config/test.rb` beside the file that defined the
/// subclass (`Object.const_source_location`). merchant_workflows writes
/// `class Test < Config` in that file of its own and an empty
/// `class Config < ::Component::Config` body. Spliced into the
/// subclass's file, `class Test` ran right after `inherited` had
/// registered the autoload, and Ruby loaded the autoload's path for
/// it, which the emitted tree did not have (LoadError at boot).
const COMPONENT_CONFIG: &str = "module Component\n  class Config\n    class << self\n      def inherited(config)\n        return super unless config.superclass == Config\n\n        if (class_name = config.name) &&\n            (config_location = Object.const_source_location(class_name)) &&\n            (parent_dirname = File.dirname(config_location[0]))\n          config_dirname = File.join(parent_dirname, \"config\")\n          config.autoload(:Test, File.join(config_dirname, \"test.rb\"))\n        end\n\n        super\n      end\n\n      def label = \"workflows\"\n\n      def for(env)\n        const_get(env.capitalize)\n      end\n    end\n  end\nend\n";
const WORKFLOWS_CONFIG: &str = "require \"component/config\"\n\nmodule MerchantWorkflowsComponent\n  class Config < ::Component::Config\n  end\nend\n";
const WORKFLOWS_CONFIG_TEST: &str = "module MerchantWorkflowsComponent\n  class Config\n    class Test < Config\n    end\n  end\nend\n";

#[test]
fn a_nested_class_written_in_its_own_file_keeps_the_file_its_autoload_names() {
    let files = [
        ("lib/component/config.rb", COMPONENT_CONFIG),
        ("lib/merchant_workflows_component/config.rb", WORKFLOWS_CONFIG),
        ("lib/merchant_workflows_component/config/test.rb", WORKFLOWS_CONFIG_TEST),
    ];
    // The emitted boot loads eagerly, which resolves the autoload; one
    // still pending names a file that exists, lazily (native) or not.
    let script = "c = MerchantWorkflowsComponent::Config\npending = c.autoload?(:Test)\np pending.nil? || File.exist?(pending)\nt = c.for(\"test\")\np t, t.superclass, t.label\n";
    let expected = native(&files, &format!("require \"merchant_workflows_component/config\"\np File.basename(MerchantWorkflowsComponent::Config.autoload?(:Test))\n{script}"));
    assert_eq!(expected, "\"test.rb\"\ntrue\nMerchantWorkflowsComponent::Config::Test\nMerchantWorkflowsComponent::Config\n\"workflows\"\n");
    let expected = expected.strip_prefix("\"test.rb\"\n").unwrap();
    let mut app = emit_and_run::empty_app();
    for (path, text) in files {
        app = app.write(path, text);
    }
    let run = app.run_ruby(script);
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, expected, "{}", run.stderr);
}

