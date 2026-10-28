//! The app's own railties and engines are carried compiled on every CRuby
//! tree, not only the PostgreSQL one (`lower::unported_rails_subclasses`
//! keeps them; `project::without_carried_railties` drops them for every
//! other target). A file nested in an in-repo engine reads the engine's
//! Rails::Engine API as it loads: a protobuf gem's inflector builds its
//! paths from `Engine.root`. Before, the non-PostgreSQL tree dropped the
//! engine class, the nested file opened `Engine` as a bare module, and
//! boot stopped with NoMethodError: undefined method 'root'.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const LOCK: &str = "PATH\n  remote: components/billing\n  specs:\n    billing (0.1.0)\n\nGEM\n  remote: https://rubygems.org/\n  specs:\n\nDEPENDENCIES\n  billing!\n";

const ENGINE: &str = "module Billing\n  class << self\n    attr_accessor :booted\n  end\n\n  class Engine < ::Rails::Engine\n    initializer \"billing.flag\" do\n      Billing.booted = true\n    end\n  end\nend\n";

const ENTRY: &str = "require_relative \"billing/engine\"\nrequire_relative \"billing/engine/inflector\"\n";

const INFLECTOR: &str = "module Billing\n  class Engine < ::Rails::Engine\n    class Inflector\n      DATA_PATH = \"#{Billing::Engine.root.join(\"data\")}/\".freeze\n\n      def self.data_path\n        DATA_PATH\n      end\n    end\n  end\nend\n";

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("Gemfile", "source \"https://rubygems.org\"\ngem \"billing\", path: \"components/billing\"\n")
        .write("Gemfile.lock", LOCK)
        .write(
            "config/application.rb",
            "require_relative \"boot\"\nrequire \"rails/all\"\n\nBundler.require(*Rails.groups)\n\nmodule Oracleapp\n  class Application < Rails::Application\n  end\nend\n",
        )
        .write("components/billing/lib/billing.rb", ENTRY)
        .write("components/billing/lib/billing/engine.rb", ENGINE)
        .write("components/billing/lib/billing/engine/inflector.rb", INFLECTOR)
        .write("components/billing/data/.keep", "")
        .write("app/models/oracle.rb", "class Oracle\n  def path\n    Billing::Engine::Inflector.data_path\n  end\nend\n")
}

#[test]
fn an_in_repo_engine_is_carried_and_answers_its_root_on_the_non_postgresql_tree() {
    let run = app().run_ruby(
        "puts Billing::Engine.superclass\n\
         puts Billing::Engine::Inflector.data_path.end_with?(\"/components/billing/data/\")\n\
         puts Billing.booted.inspect\n",
    );
    run.assert_passes();
    assert_eq!(run.stdout, "Rails::Engine\ntrue\ntrue\n", "{}", run.stderr);
}

#[test]
fn another_target_drops_the_engine_by_name() {
    let (dir, _) = app().emit(BuildTarget::Spinel);
    let defines_engine = walk(&dir).into_iter().any(|path| {
        std::fs::read_to_string(&path).is_ok_and(|text| text.contains("class Engine < ::Rails::Engine") || text.contains("class Engine < Rails::Engine"))
    });
    assert!(!defines_engine, "the Spinel tree has no Rails::Engine to subclass");
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else if path.extension().is_some_and(|e| e == "rb") {
            out.push(path);
        }
    }
    out
}
