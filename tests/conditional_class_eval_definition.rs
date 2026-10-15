//! A load-time conditional whose branch writes a method with a literal
//! `class_eval` heredoc, as sprockets-rails 3.5.2's
//! `Sprockets::Rails::Helper` does:
//!
//! ```ruby
//! private
//!   if RUBY_VERSION >= "2.7"
//!     class_eval <<-RUBY, __FILE__, __LINE__ + 1
//!       def find_asset(path, options = {})
//!         @env[path, **options]
//!       end
//!     RUBY
//!   else
//!     def find_asset(path, options = {})
//!       @env[path, options]
//!     end
//!   end
//! ```
//!
//! Ingest refused the file ("a load-time conditional definition whose
//! branch holds a statement other than a `def`"), so
//! `require "sprockets/rails/helper"` found nothing to load. The branch
//! Ruby selects when the body loads is the one that defines the method;
//! the string's own visibility scope makes its methods public, where the
//! plain `def` below `private` is private.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const HELPER: &str = r#"module AssetHelper
  def lookup(path)
    find_asset(path)
  end

  private

  if RUBY_VERSION >= "2.7"
    class_eval <<-RUBY, __FILE__, __LINE__ + 1
      def find_asset(path, options = {})
        [:keywords, path, options]
      end
    RUBY
  else
    def find_asset(path, options = {})
      [:positional, path, options]
    end
  end

  if RUBY_VERSION < "2.0"
    class_eval <<-RUBY, __FILE__, __LINE__ + 1
      def legacy = :legacy
    RUBY
  else
    def current = :current
  end

  def precompiled?(path) = path.end_with?(".js")
end

class Host
  include AssetHelper
end
"#;

#[test]
fn the_selected_branch_writes_the_class_eval_methods() {
    let run = emit_and_run::empty_app().write("lib/asset_helper.rb", HELPER).run_ruby(
        r#"host = Host.new
p host.lookup("a.js")
p host.find_asset("b.js", x: 1)
p Host.private_method_defined?(:current)
p Host.method_defined?(:legacy)
p Host.private_method_defined?(:precompiled?)
"#,
    );
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "[:keywords, \"a.js\", {}]\n[:keywords, \"b.js\", {x: 1}]\ntrue\nfalse\ntrue\n");
}

#[test]
fn a_class_eval_string_holding_more_than_defs_is_refused_by_name() {
    let (_, errors) = emit_and_run::empty_app()
        .write(
            "lib/asset_helper.rb",
            r#"module AssetHelper
  if RUBY_VERSION >= "2.7"
    class_eval <<-RUBY, __FILE__, __LINE__ + 1
      attr_accessor :env
      FLAG = true
    RUBY
  else
    def find_asset = nil
  end
end
"#,
        )
        .emit(roundhouse::project::BuildTarget::Ruby);
    assert!(errors.iter().any(|e| e.contains("holds a `class_eval` string holding a statement other than a `def`")), "{errors:?}");
}
