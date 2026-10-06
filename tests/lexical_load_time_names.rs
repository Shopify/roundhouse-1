//! Two load-time names the emitted app must resolve the way Ruby's
//! lexical scope does, from Core's `Podding` tree:
//!
//! * `class KateProvider < Provider` inside `module Podish::Database`
//!   names `Podish::Database::Provider`. The emitted file must require
//!   that sibling before the class opens; the aggregator loads files in
//!   name order, and `kate_provider` sorts first.
//! * A constant assigned inside `class << self` is read by bare name
//!   from the block's own methods. Ingest hoists it to the enclosing
//!   module, so the qualified reference must name that module, not
//!   Rubydex's singleton segment (`Locking::<Locking>::ERR` is not Ruby).
//!
//! Native Ruby is the oracle.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const PROVIDER: &str = r#"module Podish
  module Database
    class Provider
      def kind
        "base"
      end
    end
  end
end
"#;

const KATE_PROVIDER: &str = r#"module Podish
  module Database
    class KateProvider < Provider
      def kind
        "kate:" + super
      end
    end
  end
end
"#;

const LOCKING: &str = r#"module Podish
  module Locking
    class << self
      def check(on_lock)
        raise ArgumentError, ON_LOCK_ERROR unless [:ignore, :raise].include?(on_lock)
        on_lock
      end

      ON_LOCK_ERROR = "Please provide a correct on_lock mechanism!"
    end
  end
end
"#;

const REPORT: &str = r#"puts Podish::Database::KateProvider.new.kind
puts Podish::Locking.check(:raise)
begin
  Podish::Locking.check(:other)
rescue ArgumentError => e
  puts e.message
end
"#;

const EXPECTED: &str = "kate:base\nraise\nPlease provide a correct on_lock mechanism!\n";

const FILES: &[(&str, &str)] = &[
    ("app/lib/podish/database/provider.rb", PROVIDER),
    ("app/lib/podish/database/kate_provider.rb", KATE_PROVIDER),
    ("app/lib/podish/locking.rb", LOCKING),
];

#[test]
fn native_ruby_resolves_lexical_names() {
    let app = std::env::temp_dir().join(format!("roundhouse-lexical-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&app);
    let mut requires = String::new();
    for (path, text) in FILES {
        let full = app.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, text).unwrap();
        requires.push_str(&format!("require {:?}\n", full.display().to_string()));
    }
    let output = std::process::Command::new("ruby")
        .arg("-e")
        .arg(format!("{requires}{REPORT}"))
        .output()
        .expect("native Ruby control");
    let _ = std::fs::remove_dir_all(&app);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8_lossy(&output.stdout), EXPECTED);
}

#[test]
fn emitted_ruby_resolves_lexical_names() {
    let mut fixture = emit_and_run::real_blog();
    for (path, text) in FILES {
        fixture = fixture.write(path, text);
    }
    let (emitted, errors) = fixture.emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    let main = emitted.join("main").display().to_string();
    let output = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("require {main:?}\nMain.configure_default_adapter!\n{REPORT}"))
        .current_dir(&emitted)
        .env("BLOG_DB", ":memory:")
        .output()
        .expect("spawn ruby");
    assert!(
        output.status.success(),
        "emitted program failed in {}:\n{}",
        emitted.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), EXPECTED);
}
