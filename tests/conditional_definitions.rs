//! A class body's load-time conditional definitions run as Ruby runs
//! them, and a concern carrier's own `extended` hook runs when the
//! carrier extends an includer.
//!
//! Core's `patches/active_record/log_no_op_rollback.rb` defines
//! `diagnostic_sampling_enabled?` inside `class << self; if
//! Rails.env.test? … else … end`, and `active_support/cache/
//! proxy_cache_store.rb` defines `clear` per environment behind
//! `if … elsif … else`. Ingest refused both ("conditional or dynamic
//! visibility declarations are not modeled") and left the files out of
//! the emitted app. `podding/database/concerns/sharded_table.rb`'s
//! `module ClassMethods` defines `class << self; def extended(base)`,
//! which was refused too, dropping `Podding::Database::Concerns::
//! ShardedTable`, and with it `Shop`'s boot.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::path::Path;
use std::process::Command;

const LAZY: &str = "Rails.application.configure do\n  config.eager_load = false\nend\n";

#[test]
fn the_branch_ruby_selects_at_load_defines_the_method() {
    let run = emit_and_run::empty_app()
        .write("config/environments/development.rb", LAZY)
        .write(
            "lib/sampling.rb",
            r#"module Sampling
  RATE = 0.0001

  class << self
    if Rails.env.test?
      #: () -> String
      def diagnostic_sampling = "test"
    else
      #: () -> String
      def diagnostic_sampling
        RATE > 1 ? "sampled" : "other"
      end
    end
  end
end
"#,
        )
        .write(
            "lib/cache_store.rb",
            r#"class CacheStore
  if Rails.env.production?
    def clear
      "production"
    end
  elsif Rails.env.development?
    def clear
      "development"
    end
  else
    # Nothing clears the cache in production.
  end

  private

  unless Rails.env.development?
    def verify = "strict"
  else
    def verify = "lenient"
  end

  public

  # Every branch defines `mode`, so the change applies whichever ran.
  if Rails.env.production?
    def mode = "production"
  else
    def mode = "other"
  end
  private :mode

  def report
    [clear, verify, mode, CacheStore.private_method_defined?(:verify), CacheStore.public_method_defined?(:clear),
     CacheStore.private_method_defined?(:mode)]
  end
end
"#,
        )
        .run_ruby("p Sampling.diagnostic_sampling\np CacheStore.new.report\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "\"other\"\n[\"development\", \"lenient\", \"other\", true, true, true]\n");
}

fn app(root: &Path, files: &[(&str, &str)]) {
    let _ = std::fs::remove_dir_all(root);
    for (path, text) in [
        ("db/schema.rb", "ActiveRecord::Schema.define do\nend\n"),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
    ]
    .iter()
    .chain(files)
    {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
}

fn check(root: &Path) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_roundhouse")).arg("check").arg(root).output().unwrap();
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

#[test]
fn a_branch_that_changes_visibility_stays_refused_by_name() {
    let root = std::env::temp_dir().join(format!("roundhouse-conditional-definitions-{}", std::process::id()));
    app(
        &root,
        &[
            (
                "lib/marked.rb",
                "class Marked\n  if Rails.env.test?\n    private\n\n    def helper = 1\n  else\n    def helper = 2\n  end\nend\n",
            ),
            (
                "lib/accessor.rb",
                "class Accessor\n  if Rails.env.test?\n    def helper = 1\n    attr_reader :token\n  end\nend\n",
            ),
            ("lib/plain.rb", "class Plain\n  if Rails.env.test?\n    def helper = 1\n  end\nend\n"),
            // Ruby raises NameError at `private :helper` when the branch
            // that defines it did not run.
            (
                "lib/partial.rb",
                "class Partial\n  if Rails.env.test?\n    def helper = 1\n  end\n  private :helper\nend\n",
            ),
        ],
    );
    let text = check(&root);
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        text.contains("lib/marked.rb:3:5: error[ingest_unsupported]: a visibility declaration inside a load-time \
                       conditional definition is not modeled"),
        "{text}"
    );
    assert!(
        text.contains("lib/accessor.rb:4:5: error[ingest_unsupported]: an accessor or alias declared in a load-time \
                       conditional definition is not modeled"),
        "{text}"
    );
    assert!(
        text.contains("lib/partial.rb:5:3: error[ingest_unsupported]: visibility change for `helper`, which a \
                       load-time conditional defines in only some branches, is not modeled"),
        "{text}"
    );
    assert!(!text.contains("lib/plain.rb"), "{text}");
}

#[test]
fn a_carrier_modules_extended_hook_runs_when_it_extends_an_includer() {
    let run = emit_and_run::empty_app()
        .write("config/environments/development.rb", LAZY)
        .write(
            "lib/sharded.rb",
            r#"module Sharded
  class << self
    def included(base)
      base.extend(ClassMethods)
    end
  end

  module ClassMethods
    class << self
      def extended(base)
        base.instance_variable_set(:@sharded_hook, "extended #{base.name}")
      end
    end

    def sharded_table?
      true
    end

    def sharded_hook
      @sharded_hook
    end
  end
end
"#,
        )
        // The same hook on an ActiveSupport::Concern's carrier.
        .write(
            "lib/tracked.rb",
            r#"module Tracked
  extend ActiveSupport::Concern

  module ClassMethods
    class << self
      def extended(base)
        base.instance_variable_set(:@tracked_hook, "tracked #{base.name}")
      end
    end

    def tracked_hook
      @tracked_hook
    end
  end
end
"#,
        )
        .write("lib/widget.rb", "class Widget\n  include Sharded\n  include Tracked\nend\n")
        // A model includer, as `Shop` includes ShardedTable.
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table \"shops\", force: :cascade do |t|\n    t.string \"name\"\n  end\nend\n")
        .write("app/models/shop.rb", "class Shop < ApplicationRecord\n  include Sharded\n  include Tracked\nend\n")
        .run_ruby(
            "p [Widget.sharded_table?, Widget.sharded_hook, Widget.tracked_hook]\n\
             p [Shop.sharded_table?, Shop.sharded_hook, Shop.tracked_hook]\n\
             p Sharded::ClassMethods.singleton_methods\n",
        );
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(
        run.stdout,
        "[true, \"extended Widget\", \"tracked Widget\"]\n[true, \"extended Shop\", \"tracked Shop\"]\n[:extended]\n"
    );
}
