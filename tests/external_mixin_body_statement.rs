//! A module body's `Gem::Klass.prepend(Mod)`, where `Mod` is the app's own
//! module and `Gem::Klass` one the app does not define, runs as written
//! when the module loads.
//!
//! Core's `ActiveRecordTestHelper::SharedPoolPatch` defines
//! `UniqueConnectionPoolList` and ends with
//! `ActiveRecord::ConnectionAdapters::ConnectionHandler.prepend(
//! UniqueConnectionPoolList)`. The emitted module dropped the statement,
//! so `connection_pool_list` answered a pool shared by two owners twice,
//! the fixtures pinned it twice, and a test's `open_transactions` read 2
//! (`ShopVersions::AtomicUpdateInTransactionError` in every version bump).

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

const PATCHES: &str = r##"module Cs
  module Patches
    module Shouting
      def write(text)
        super(text.upcase)
      end
    end

    module Counting
      def self.extended(base)
        base.instance_variable_set(:@extended_by, name)
      end
    end

    StringIO.prepend(Shouting)
    Tempfile.extend(Counting)
  end
end
"##;

const SCRIPT: &str = "require \"stringio\"\nrequire \"tempfile\"\nCs::Patches\nio = StringIO.new\nio.write(\"quiet\")\np io.string\np Tempfile.instance_variable_get(:@extended_by)\np StringIO.ancestors.first.name\n";

fn native() -> String {
    let dir = std::env::temp_dir().join(format!("rh_external_mixin_body_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let preamble = "require \"stringio\"\nrequire \"tempfile\"\n";
    std::fs::write(dir.join("main.rb"), format!("{preamble}{PATCHES}{SCRIPT}")).unwrap();
    let out = Command::new("ruby").arg("main.rb").current_dir(&dir).output().expect("ruby is on PATH");
    assert!(out.status.success(), "native failed:\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_module_body_mixin_into_a_gem_class_runs_as_written() {
    let expected = native();
    assert_eq!(
        expected.lines().collect::<Vec<_>>(),
        ["\"QUIET\"", "\"Cs::Patches::Counting\"", "\"Cs::Patches::Shouting\""],
        "native control"
    );
    let run = emit_and_run::real_blog().write("app/models/cs/patches.rb", PATCHES).run_ruby(SCRIPT);
    assert!(run.success, "emitted program failed:\n{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout.lines().collect::<Vec<_>>(), expected.lines().collect::<Vec<_>>());
}

/// The mixed-in module is a file of its own beside the one that mixes it
/// in (Core's `UniqueConnectionPoolList` stands in
/// `shared_pool_patch/unique_connection_pool_list.rb`, which Zeitwerk
/// loads when the statement names it): the emitted statement needs it
/// loaded first.
#[test]
fn a_module_body_mixin_of_a_module_in_its_own_file_loads_it_first() {
    let run = emit_and_run::real_blog()
        .write(
            "app/models/cs/patches.rb",
            "module Cs\n  module Patches\n    StringIO.prepend(Shouting)\n  end\nend\n",
        )
        .write(
            "app/models/cs/patches/shouting.rb",
            "module Cs\n  module Patches\n    module Shouting\n      def write(text)\n        super(text.upcase)\n      end\n    end\n  end\nend\n",
        )
        .run_ruby("require \"stringio\"\nCs::Patches\nio = StringIO.new\nio.write(\"quiet\")\np io.string\np StringIO.ancestors.first.name\n");
    assert!(run.success, "emitted program failed:\n{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout.lines().collect::<Vec<_>>(), ["\"QUIET\"", "\"Cs::Patches::Shouting\""]);
}
