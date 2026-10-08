//! An `ActiveSupport.on_load` inside a support file's file-level
//! `if`/`unless` registers when Ruby runs that branch, as the bare
//! statement does (tests/file_class_new_paths.rs). Shopify Core has the
//! shape four times: paquito 1.0.2's `if defined? ActiveSupport`,
//! secure_headers 5.2.0's view_helper.rb, and the essentials patches
//! schema_context.rb and sprockets_debug_ext.rb.
//!
//! Before: a branch that also declares a module dropped its hook in
//! silence (`library_class::program_items` skipped every `on_load`); a
//! tree without initializers never fired the framework hooks, so a lib
//! file's registration never ran; and a tree without the lazy_load_hooks
//! port emitted the conditional registration instead of refusing it.
//!
//! CRuby is the oracle: the same files load on Rails 8.1.4's own
//! lazy_load_hooks, then Active Record's base loads; the emitted file
//! programs load on the runtime's port, then boot's framework hooks fire.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

use roundhouse::project::BuildTarget;

/// A branch holding only the registration: carried as the statement.
const DIGESTY: &str = r##"module Digesty
  def digesty = :yes
end

if defined? ActiveSupport
  ActiveSupport.on_load(:active_record) do
    include Digesty
  end
end
"##;

/// Branches holding declarations: carried as a branch item, and only
/// the branch Ruby runs registers its hook.
const STAMPED: &str = r##"if defined?(ActiveSupport)
  module Stamped
    def stamped = :stamped
  end

  ActiveSupport.on_load(:active_record) do
    include Stamped
  end
else
  module Unstamped
  end

  ActiveSupport.on_load(:active_record) do
    include Unstamped
  end
end
"##;

const SCRIPT: &str = r##"
p ActiveRecord::Base.include?(Digesty), ActiveRecord::Base.include?(Stamped), defined?(Unstamped)
p ActiveRecord::Base.instance_method(:digesty).owner.name, ActiveRecord::Base.instance_method(:stamped).owner.name
"##;

const EXPECTED: &str = "true\ntrue\nnil\n\"Digesty\"\n\"Stamped\"\n";

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app().write("lib/ext/digesty.rb", DIGESTY).write("lib/ext/stamped.rb", STAMPED)
}

#[test]
fn a_hook_in_a_file_level_branch_registers_where_the_branch_runs() {
    // Registered as the files load, run when Active Record's base loads.
    let native = Command::new("ruby")
        .arg("-W0")
        .arg("-e")
        .arg(format!("require \"active_record\"\n{DIGESTY}\n{STAMPED}\nActiveRecord::Base\n{SCRIPT}"))
        .output()
        .expect("native Ruby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(String::from_utf8_lossy(&native.stdout), EXPECTED);

    let (dir, errors) = app()
        .write("roundhouse.yml", "test_paths:\n  - test\ndatabase_adapter: postgresql\n")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_file_branch_load_hooks\n")
        .emit(BuildTarget::Ruby);
    let errors: Vec<&String> = errors.iter().filter(|e| !e.contains("database_adapter")).collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let read = |rel: &str| std::fs::read_to_string(dir.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
    let stamped = read("app/models/roundhouse_files/lib/ext/stamped.rb");
    assert_eq!(stamped.matches("ActiveSupport.on_load(:active_record)").count(), 2, "{stamped}");
    // No initializer fires the hooks here: boot does, for the files'
    // registrations.
    let boot = read("boot.rb");
    let fire = boot
        .lines()
        .find(|line| line.starts_with("ActiveSupport.run_load_hooks(:active_record, ActiveRecord::Base)"))
        .unwrap_or_else(|| panic!("boot fires no :active_record hook:\n{boot}"));
    // The PostgreSQL tree cannot boot without the `pg` gem, so the
    // emitted pieces run in their load order on CRuby: the runtime's
    // lazy_load_hooks and Active Record, the emitted autoloader's
    // file-program helpers (app/models.rb's RoundhouseAutoload, without
    // its boot), the two file programs, and boot's hook firing.
    let models = read("app/models.rb");
    let autoload_end = models.find("\nend\n").expect("RoundhouseAutoload module") + "\nend\n".len();
    std::fs::write(dir.join("roundhouse_autoload_only.rb"), format!("{}\n", &models[..autoload_end]).replace("__dir__", "File.join(Dir.pwd, \"app\")"))
        .unwrap();
    let emitted_script = format!(
        "%w[runtime/active_support_lazy_load_hooks runtime/active_record roundhouse_autoload_only \
         app/models/roundhouse_files/lib/ext/digesty app/models/roundhouse_files/lib/ext/stamped]\
         .each {{ |f| require_relative File.join(Dir.pwd, f) }}\n{fire}\n{SCRIPT}"
    );
    let emitted = Command::new("ruby").arg("-W0").arg("-e").arg(&emitted_script).current_dir(&*dir).output().expect("ruby");
    assert!(emitted.status.success(), "{}\n{emitted_script}", String::from_utf8_lossy(&emitted.stderr));
    assert_eq!(String::from_utf8_lossy(&emitted.stderr), "");
    assert_eq!(String::from_utf8_lossy(&emitted.stdout), EXPECTED);
}

#[test]
fn a_tree_without_the_lazy_load_hooks_port_refuses_a_conditional_registration() {
    let tree: roundhouse::det::HashMap<std::path::PathBuf, Vec<u8>> = [
        (std::path::PathBuf::from("lib/ext/digesty.rb"), DIGESTY.as_bytes().to_vec()),
        (std::path::PathBuf::from("lib/ext/stamped.rb"), STAMPED.as_bytes().to_vec()),
    ]
    .into_iter()
    .collect();
    let app = roundhouse::ingest::ingest_app_from_tree(tree).expect("ingest");
    for target in [BuildTarget::Ruby, BuildTarget::Spinel] {
        let (result, diagnostics) = roundhouse::emit::diagnostics::scope(|| {
            roundhouse::project::target_files(&app, std::path::Path::new("not-a-fixture"), target)
        });
        let error = result.err().unwrap_or_else(|| panic!("{} emitted the registration", target.as_str()));
        assert!(error.contains("`ActiveSupport.on_load` registration needs the PostgreSQL CRuby tree"), "{error}");
        let refused = diagnostics
            .iter()
            .filter(|d| d.message.contains("load_hook_registration") && d.message.contains("`ActiveSupport.on_load(:active_record)`"))
            .count();
        // digesty's one, and both of stamped's branches.
        assert_eq!(refused, 3, "{diagnostics:#?}");
    }
}
