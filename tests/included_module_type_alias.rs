//! A sorbet type alias an included module declares is found the way Ruby
//! finds any constant: lexical scopes, then the ancestors of the class.
//!
//! ```ruby
//! module Hedwig::CommandTable::Bulking
//!   StaleInProgressBulkInfo = T.type_alias { { scope: String, ... } }
//! end
//!
//! class Hedwig::CommandTable::Memory::Bulking < BaseCommandTable
//!   include CommandTable::Bulking
//!   def initialize
//!     @bulk_in_progress_registry = T.let({}, T::Hash[String, [Float, StaleInProgressBulkInfo]])
//! ```
//!
//! Alias lookup only walked the lexical scopes, so the cast was refused
//! (`StaleInProgressBulkInfo has no indexed declaration or modeled
//! dependency`) and the emitted initializer raised. A signature naming the
//! mixin's alias read it as a class with no methods.

use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn check(extra: &str) -> String {
    let root = std::env::temp_dir().join(format!(
        "rh_included_alias_{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&root);
    let files = [
        ("db/schema.rb", "ActiveRecord::Schema.define do\nend\n".to_owned()),
        (
            "app/models/application_record.rb",
            "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n".to_owned(),
        ),
        ("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n".to_owned()),
        (
            "app/models/hq/table/bulking.rb",
            r#"# typed: true
module Hq
  module Table
    module Bulking
      extend T::Sig
      Info = T.type_alias { { scope: String, worker_id: String } }
    end
  end
end
"#
            .to_owned(),
        ),
        (
            "app/models/hq/table/memory/bulking.rb",
            format!(
                r#"# typed: true
module Hq
  module Table
    module Memory
      class Bulking
        extend T::Sig
        include Table::Bulking

        sig {{ void }}
        def initialize
          @registry = T.let({{}}, T::Hash[String, [Float, Info]])
        end

        sig {{ params(key: String).returns(T.nilable(String)) }}
        def worker(key)
          entry = @registry[key]
          return nil unless entry
          entry[1][:worker_id]
        end

        sig {{ params(info: Info).returns(String) }}
        def scope_of(info)
          info[:scope].upcase
        end
{extra}      end
    end
  end
end
"#
            ),
        ),
    ];
    for (path, text) in &files {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_roundhouse"))
        .args(["check", "--continue"])
        .arg(&root)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&root);
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

#[test]
fn a_cast_and_a_signature_read_an_included_modules_alias() {
    let output = check("");
    assert!(!output.contains("declared type not supported"), "{output}");
    assert!(output.contains(" 0 error(s)"), "{output}");
    assert!(output.contains("0 send(s) on unknown receivers"), "{output}");
}

#[test]
fn the_included_alias_still_refuses_what_it_lacks() {
    let output = check(
        r#"
        sig { params(info: Info).returns(String) }
        def broken(info)
          info[:scope].imaginary_method
        end

        def missing
          @missing = T.let(nil, T.nilable(Nowhere))
        end
"#,
    );
    // `info[:scope]` is a known String: the unknown method counts against
    // a known receiver, not an unknown one.
    assert!(output.contains("0 send(s) on unknown receivers"), "{output}");
    assert!(output.contains("1 unresolved on known receivers"), "{output}");
    assert!(output.contains("Nowhere has no indexed declaration or modeled dependency"), "{output}");
    assert!(!output.contains("Info has no indexed declaration"), "{output}");
}
