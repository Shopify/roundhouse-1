//! A Concern a boot-time `ActiveSupport.on_load(:hook) { include M }`
//! mixes into the hooked base at run time keeps its `ClassMethods`.
//!
//! ActiveSupport::Concern's `append_features` extends the includer with
//! `ClassMethods` (activesupport concern.rb). An app module's includers
//! get that at compile time, but an include a lazy-load hook runs on a
//! base the app never writes (`ActiveSupport::TestCase` through
//! `on_load(:active_support_test_case)`) has no includer to splice into,
//! so the emitted module has to keep its `extend ActiveSupport::Concern`
//! (ingest/base_includes.rs; the same as a module included into
//! ActiveRecord::Base). Without it the base had none of the module's
//! class macros: `use_cart_next_store` raised NameError in the test class
//! body and its setup never ran.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const HELPER: &str = r#"module Probe
  module Helper
    extend(ActiveSupport::Concern)

    module ClassMethods
      def probe_macro(label)
        (@probes ||= []) << label
      end

      def probes
        @probes
      end
    end

    def instance_probe
      "instance"
    end
  end
end
"#;

/// shopify-money's railtie shape: the hook registered in a railtie body.
const RAILTIE: &str = r#"module Probe
  class Railtie < Rails::Railtie
    ActiveSupport.on_load(:probe_hook) do
      include Probe::Helper
    end
  end
end
"#;

#[test]
fn a_concern_included_by_a_load_hook_extends_the_base_with_its_class_methods() {
    let (dir, _) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\ndatabase_adapter: postgresql\n")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_load_hook_concern\n")
        .write("lib/oracle.rb", "class Oracle\nend\n")
        .write("lib/probe/helper.rb", HELPER)
        .write("lib/probe/railtie.rb", RAILTIE)
        .write("config/application.rb", "require \"rails\"\n\nmodule ProbeApp\n  class Application < Rails::Application\n  end\nend\n")
        .emit(BuildTarget::Ruby);
    let helper = std::fs::read_to_string(dir.join("app/models/probe/helper.rb")).expect("helper.rb");
    assert!(helper.contains("extend(ActiveSupport::Concern)"), "{helper}");
    let out = emit_and_run::ruby()
        .current_dir(&*dir)
        .args([
            "-e",
            "require_relative 'runtime/active_support_lazy_load_hooks'\n\
             require_relative 'runtime/active_support_concern'\n\
             require_relative 'app/models/probe/helper'\n\
             require_relative 'app/models/probe/helper/class_methods'\n\
             class ProbeBase; end\n\
             ActiveSupport.on_load(:probe_hook) { include Probe::Helper }\n\
             ActiveSupport.run_load_hooks(:probe_hook, ProbeBase)\n\
             ProbeBase.probe_macro(:one)\n\
             p [ProbeBase.probes, ProbeBase.new.instance_probe, ProbeBase.singleton_class.include?(Probe::Helper::ClassMethods)]\n",
        ])
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stdout}\n{stderr}");
    assert_eq!(stdout.trim(), "[[:one], \"instance\", true]", "{stderr}");
}
