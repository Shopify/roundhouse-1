//! Rails' role and shard switches type on every model's class side.
//!
//! The CRuby PostgreSQL runtime carries `connected_to`, `connected_to?`,
//! `current_role` and the rest of ConnectionHandling. The analyzer only
//! knew `transaction`/`connection` on a model's class side, so Core's
//! `CoreGeneral2Record.connected_to(role: :reading) { … }` read as `no
//! known method`. Without a runtime that carries the switches (here a
//! SQLite app), the call is still refused at its site.

use std::process::Command;

fn check(name: &str, postgresql: bool) -> String {
    let root = std::env::temp_dir().join(format!("rh_connection_switch_typing_{}_{name}", std::process::id()));
    let (adapter_line, adapter) = if postgresql { ("database_adapter: postgresql\n", "postgresql") } else { ("", "sqlite3") };
    let files = [
        ("roundhouse.yml", format!("test_paths:\n  - test\n{adapter_line}")),
        ("config/database.yml", format!("test:\n  adapter: {adapter}\n  database: x\n")),
        ("db/structure.sql", "CREATE TABLE core.accounts (\n    id bigint NOT NULL\n);\n".to_string()),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n".to_string()),
        ("app/models/other_record.rb", "class OtherRecord < ApplicationRecord\n  self.abstract_class = true\nend\n".to_string()),
        (
            "app/models/account.rb",
            "class Account < OtherRecord\n  #: -> untyped\n  def use = [OtherRecord.connected_to(role: :reading) { 1 }, Account.connected_to?(role: :reading).zzz, ActiveRecord::Base.current_role.yyy]\nend\n".to_string(),
        ),
    ];
    for (path, text) in files {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let output = Command::new(env!("CARGO_BIN_EXE_roundhouse")).arg("check").arg(&root).output().unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

#[test]
fn the_switches_type_on_a_postgresql_model() {
    let text = check("pg", true);
    assert!(!text.contains("no known method `connected_to"), "{text}");
    assert!(!text.contains("connected_to not supported"), "{text}");
    assert!(text.contains("no known method `zzz` on bool"), "{text}");
    assert!(text.contains("no known method `yyy` on Symbol"), "{text}");
}

#[test]
fn without_a_runtime_for_them_the_switch_is_still_refused() {
    let text = check("sqlite", false);
    assert!(text.contains("account.rb:3:14: error[unsupported]: connected_to not supported"), "{text}");
    assert!(!text.contains("no known method `connected_to"), "{text}");
}
