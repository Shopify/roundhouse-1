//! A statement that a namespace module's body writes after a nested
//! declaration, in the file Zeitwerk loads for that declaration, runs
//! when that file loads, not when the namespace is first named.
//!
//! Core's components/content/metafields/app/models/metafields/owners/registry.rb:
//!
//! ```ruby
//! module Metafields
//!   module Owners
//!     module Registry … end
//!
//!     ActiveSupport.run_load_hooks(:metafields_owners_registry, Metafields)
//!
//!     if Rails.application.config.consider_all_requests_local
//!       OwnerConfigurationChecks.raise_on_unexpected_empty_access_scopes
//!       …
//! ```
//!
//! Ingest merges the reopen into the one `Metafields::Owners`, so the
//! `if` was written into metafields/owners.rb (a file Core does not
//! have): the first `Metafields::Owners::…` reference, a class body's
//! `include Metafields::Owners::AbstractOwner`, ran the checks, which
//! loaded the registry, whose hook's `register_owner` blocks loaded a
//! model requiring the file still loading: a circular require that
//! Core's StrictWarning raised at test boot.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

use roundhouse::project::BuildTarget;

const REGISTRY: &str = "module Metafields\n  module Owners\n    module Registry\n      def self.owners\n        @owners ||= []\n      end\n\n      def self.note(owner)\n        $noted = ($noted || 0) + 1\n        owners << owner\n      end\n    end\n\n    LIMIT = 3\n\n    if Registry.owners.size < LIMIT\n      Registry.note(\"checked\")\n    end\n  end\nend\n";
const ABSTRACT: &str = "module Metafields\n  module Owners\n    module AbstractOwner\n      def self.kind\n        \"abstract\"\n      end\n    end\n  end\nend\n";

/// Reads the sibling first, then the registry, printing how often the
/// registry's namespace statement ran after each. (`LIMIT` gives the
/// namespace a body of its own: a module whose body is only statements
/// is not carried at all.)
const SCRIPT: &str = "puts Metafields::Owners::AbstractOwner.kind\nputs $noted.inspect\nputs Metafields::Owners::Registry.owners.inspect\nputs $noted.inspect\n";

const EXPECTED: [&str; 4] = ["abstract", "nil", "[\"checked\"]", "1"];

fn lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes).lines().map(str::to_string).collect()
}

#[test]
fn a_namespace_bodys_statement_runs_with_the_file_that_wrote_it() {
    let root = std::env::temp_dir().join(format!("roundhouse-namespace-statement-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("metafields/owners")).unwrap();
    std::fs::write(root.join("metafields/owners/registry.rb"), REGISTRY).unwrap();
    std::fs::write(root.join("metafields/owners/abstract_owner.rb"), ABSTRACT).unwrap();
    // CRuby with Zeitwerk managing the directory, as Rails' autoloader:
    // `Metafields` and `Metafields::Owners` are implicit namespaces.
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("require \"zeitwerk\"\nloader = Zeitwerk::Loader.new\nloader.push_dir({:?})\nloader.setup\n{SCRIPT}", root.display().to_string()))
        .output()
        .expect("native Ruby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(lines(&native.stdout), EXPECTED, "native control");

    // A lazy boot, as Rails' development and test environments load.
    let (tree, errors) = emit_and_run::empty_app()
        .write("config/environments/development.rb", "Rails.application.configure do\n  config.eager_load = false\nend\n")
        .write("app/models/metafields/owners/registry.rb", REGISTRY)
        .write("app/models/metafields/owners/abstract_owner.rb", ABSTRACT)
        .emit(BuildTarget::Ruby);
    let read = |rel: &str| std::fs::read_to_string(tree.join(rel)).unwrap_or_default();
    let namespace = read("app/models/metafields/owners.rb");
    assert!(!namespace.contains("Registry.note"), "the namespace's file runs the registry file's statement:\n{namespace}\nerrors: {errors:#?}");
    let registry = read("app/models/metafields/owners/registry.rb");
    let declared = registry.find("def self.note").unwrap_or_else(|| panic!("registry.rb lacks its module:\n{registry}"));
    let statement = registry.find("Registry.note(\"checked\")").unwrap_or_else(|| panic!("registry.rb lacks the statement:\n{registry}\nowners.rb:\n{namespace}\nerrors: {errors:#?}"));
    assert!(declared < statement, "the statement precedes the declaration it follows:\n{registry}");

    let run = Command::new("ruby")
        .current_dir(&tree)
        .env("BLOG_DB", ":memory:")
        .arg("-e")
        .arg(format!("require File.expand_path(\"main\", Dir.pwd)\nMain.configure_default_adapter!\n{SCRIPT}"))
        .output()
        .expect("spawn ruby");
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "emitted tree failed in {}: {stderr}\nerrors: {errors:#?}", tree.display());
    assert_eq!(lines(&run.stdout), EXPECTED, "{stderr}\nerrors: {errors:#?}");
    let _ = std::fs::remove_dir_all(&root);
}
