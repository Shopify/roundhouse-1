//! A callback block's `next` after the block becomes a method.
//!
//! `after_create do next if x; … end` was folded into `def after_create`
//! with the body copied verbatim. A `next` that is not inside a block or
//! loop is a syntax error in a method body, so the emitted model did not
//! parse (`Invalid next`; Core's `MerchantMetricDashboardMetric`). Any
//! `return` would end the shared hook method and skip the callbacks
//! folded after it, so a body that leaves early becomes its own method.
//! The hook calls that method, and the method's own `next` becomes
//! `return`. A `next` that belongs to a nested block, loop or lambda is
//! left as it is.
//!
//! The expected values are native Active Record 8.1's (SQLite, same
//! model and script). The run shows the guarded callback ends early and
//! the second `after_create` still runs.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const VALIDATES: &str = "  validates :title, presence: true\n";

const HOOKS: &str = concat!(
    "  after_create do\n",
    "    next if title == \"skip\"\n",
    "    [1, 2].each do |i|\n",
    "      next if i == 1\n",
    "      self.body = \"#{body} each#{i}\"\n",
    "    end\n",
    "    self.body = \"#{body} first\"\n",
    "  end\n",
    "  after_create { self.body = \"#{body} second\" }\n",
    "  before_save -> { next if title == \"skip\"; self.body = \"#{body} lambda\" }\n",
    "\n",
    "  validates :title, presence: true\n",
);

fn assert_parses(path: &std::path::Path) -> String {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let errors: Vec<String> = {
        let parsed = ruby_prism::parse(text.as_bytes());
        parsed.errors().map(|e| e.message().to_string()).collect()
    };
    assert!(errors.is_empty(), "{} does not parse: {errors:?}\n{text}", path.display());
    text
}

#[test]
fn a_next_in_a_hook_block_ends_only_that_callback() {
    let run = emit_and_run::real_blog()
        .edit("app/models/article.rb", VALIDATES, HOOKS)
        .run_ruby(concat!(
            "a = Article.create!(title: \"go\", body: \"a long enough body\")\n",
            "p a.body, Article.find(a.id).body\n",
            "b = Article.create!(title: \"skip\", body: \"a long enough body\")\n",
            "p b.body, Article.find(b.id).body\n",
        ));
    let model = assert_parses(&run.emitted.join("app/models/article.rb"));
    run.assert_passes();
    assert_eq!(
        run.stdout.trim_end(),
        concat!(
            "\"a long enough body lambda each2 first second\"\n",
            "\"a long enough body lambda\"\n",
            "\"a long enough body second\"\n",
            "\"a long enough body\"",
        )
    );

    // The guarded body is its own method, which the shared hook calls
    // before the next folded callback.
    let hook = model
        .split("def after_create\n")
        .nth(1)
        .and_then(|rest| rest.split("\n  end\n").next())
        .unwrap_or_else(|| panic!("no `def after_create`:\n{model}"));
    let lifted = hook
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("_article_after_create_callback_"))
        .unwrap_or_else(|| panic!("hook does not call a lifted callback:\n{hook}"));
    let lifted_def = model
        .split(&format!("def {lifted}\n"))
        .nth(1)
        .and_then(|rest| rest.split("\n  end\n").next())
        .unwrap_or_else(|| panic!("no `def {lifted}`:\n{model}"));
    assert!(lifted_def.contains("return if"), "{lifted_def}");
    assert!(!lifted_def.contains("next if self.title"), "{lifted_def}");
    assert!(lifted_def.contains("next if i == 1"), "nested `next` must stay:\n{lifted_def}");
}

/// A block's own `break` raises LocalJumpError in Rails (`break from
/// proc-closure`), and a method has no equivalent. The callback is
/// refused by name; its text is not copied into the hook.
#[test]
fn a_break_in_a_hook_block_is_refused_by_name() {
    let (emitted, errors) = emit_and_run::real_blog()
        .edit(
            "app/models/article.rb",
            VALIDATES,
            "  after_create do\n    break if title == \"x\"\n  end\n\n  validates :title, presence: true\n",
        )
        .emit(BuildTarget::Ruby);
    assert!(
        errors.iter().any(|e| e.contains("`after_create` on `Article`")
            && e.contains("own `break` has no hook-method equivalent")),
        "{errors:#?}"
    );
    let model = assert_parses(&emitted.join("app/models/article.rb"));
    assert!(!model.contains("break"), "{model}");
}

/// A `normalizes` lambda's `next v` is its value; the normalizer
/// method ends with `return v`. Native: "SHOUT", "hi".
#[test]
fn a_next_in_a_normalizes_lambda_returns_its_value() {
    let run = emit_and_run::real_blog()
        .edit(
            "app/models/article.rb",
            VALIDATES,
            concat!(
                "  normalizes :title, with: ->(t) { next t.delete(\"!\").upcase if t.start_with?(\"!\"); t.strip }\n",
                "  validates :title, presence: true\n",
            ),
        )
        .run_ruby(concat!(
            "p Article.create!(title: \"!shout\", body: \"a long enough body\").title\n",
            "p Article.create!(title: \"  hi  \", body: \"a long enough body\").title\n",
        ));
    assert_parses(&run.emitted.join("app/models/article.rb"));
    run.assert_passes();
    assert_eq!(run.stdout.trim_end(), "\"SHOUT\"\n\"hi\"");
}

/// Filter blocks run inline in the controller's dispatcher, where a
/// `return` would skip the action. A top-level guard clause becomes a
/// branch. Every authored request takes one of the two guards, so a
/// filter that ended the dispatcher would fail the authored assertions.
#[test]
fn a_guard_next_in_a_filter_block_still_runs_the_action() {
    let run = emit_and_run::real_blog()
        .edit(
            "app/controllers/articles_controller.rb",
            "  before_action :set_article, only: %i[ show edit update destroy ]\n",
            concat!(
                "  before_action :set_article, only: %i[ show edit update destroy ]\n",
                "  before_action do\n",
                "    next if params[:id]\n",
                "    @collection_request = true\n",
                "  end\n",
                "  before_action do\n",
                "    next unless params[:id]\n",
                "    @member_request = true\n",
                "  end\n",
                "  after_action do\n",
                "    next unless params[:id]\n",
                "    @member_request = false\n",
                "  end\n",
            ),
        )
        .run_test("test/controllers/articles_controller_test.rb");
    let controller = assert_parses(&run.emitted.join("app/controllers/articles_controller.rb"));
    run.assert_passes();
    assert!(controller.contains("@collection_request = true"), "{controller}");
    assert!(controller.contains("@member_request = false"), "{controller}");
}
