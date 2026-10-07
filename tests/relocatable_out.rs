//! The emitted tree does not depend on the directory the app sits in.
//!
//! Two run directories emitting the same Core slice disagreed in 56 files:
//! a stored class-body local's constant (`RH_BODY_LOCAL_<name>_<hash>`)
//! hashed the absolute source path with std's `DefaultHasher`, and
//! boot.rb's `on_load` registrations named their initializer by absolute
//! path. The same app emitted from two directories must give the same
//! files, byte for byte.

use std::path::{Path, PathBuf};

const PG_RB: &str = "ActiveSupport.on_load(:active_record_postgresqladapter) do\n  self.create_unlogged_tables = true if Rails.env.test?\nend\n";

const STORED: &str = r#"module Stored
  store = Object.new
  label = "Label"

  DEFAULT = store
  ALSO = store
  LABEL = label
end
"#;

const DATABASE_YML: &str = "default: &default\n  adapter: postgresql\n\ntest:\n  <<: *default\n  database: relocatable_probe\n";

struct Dir(PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &to.join(entry.file_name()));
        } else {
            std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
        }
    }
}

fn probe_app(tag: &str) -> Dir {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/tiny-blog");
    let dir = Dir(std::env::temp_dir().join(format!("roundhouse-relocatable-{tag}-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    copy_tree(&fixture, &dir.0);
    let write = |rel: &str, text: &str| {
        let path = dir.0.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write(
        "config/application.rb",
        "require_relative \"boot\"\nrequire \"rails/all\"\n\nmodule Hp\n  class Application < Rails::Application\n  end\nend\n",
    );
    write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n");
    write("app/models/stored.rb", STORED);
    write("config/initializers/pg.rb", PG_RB);
    write("config/database.yml", DATABASE_YML);
    write("roundhouse.yml", "database_adapter: postgresql\n");
    dir
}

fn emitted(dir: &Dir) -> Vec<(String, String)> {
    let mut app = roundhouse::ingest::ingest_app(&dir.0).expect("ingest probe");
    roundhouse::session::analyze_and_lower(&mut app);
    let mut files = roundhouse::project::target_files(&app, &dir.0, roundhouse::project::BuildTarget::Ruby).expect("package");
    files.sort();
    files
}

#[test]
fn the_same_app_emits_the_same_tree_from_two_directories() {
    let (a, b) = (probe_app("a"), probe_app("elsewhere-b"));
    let (left, right) = (emitted(&a), emitted(&b));
    // The probe exercises both sites: a stored body local and a hook.
    let stored = left.iter().find(|(n, _)| n.ends_with("models/stored.rb")).expect("stored.rb");
    assert!(stored.1.contains("RH_BODY_LOCAL_store_"), "{}", stored.1);
    let boot = left.iter().find(|(n, _)| n == "boot.rb").expect("boot.rb");
    assert!(boot.1.contains("# config/initializers/pg.rb\n"), "{}", boot.1);

    let names = |files: &[(String, String)]| files.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>();
    assert_eq!(names(&left), names(&right));
    for ((name, l), (_, r)) in left.iter().zip(&right) {
        assert!(l == r, "{name} differs between the two directories:\n--- a\n{l}\n--- b\n{r}");
    }
}
