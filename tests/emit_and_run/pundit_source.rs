use std::path::Path;

use roundhouse::project::BuildTarget;

use super::emit_and_run;

const GEM: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/pundit_gem");

const APPLICATION_POLICY: &str = "class ApplicationPolicy
  attr_reader :user, :record

  def initialize(user, record)
    @user = user
    @record = record
  end

  def edit?
    false
  end

  class Scope
    def initialize(user, scope)
      @user = user
      @scope = scope
    end

    def resolve
      raise NoMethodError, \"You must define #resolve in #{self.class}\"
    end

    private

    attr_reader :user, :scope
  end
end
";

const ARTICLE_POLICY: &str = "class ArticlePolicy < ApplicationPolicy
  def edit?
    user == \"admin\" && record.class.exists?(id: record.id)
  end

  class Scope < ApplicationPolicy::Scope
    def resolve
      return scope.none if user == \"nobody\"
      return scope.all if user == \"admin\"
      scope.where(title: \"Published\").or(scope.where.not(title: \"Hidden draft\"))
    end
  end
end
";

fn with_gem(mut overlay: emit_and_run::Overlay, dir: &Path, rel: &str) -> emit_and_run::Overlay {
    let mut entries: Vec<_> = std::fs::read_dir(dir).expect("read pundit_gem").map(|e| e.unwrap().path()).collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().unwrap().to_str().unwrap();
        let rel = format!("{rel}/{name}");
        overlay = if path.is_dir() {
            with_gem(overlay, &path, &rel)
        } else {
            overlay.write(&rel, &std::fs::read_to_string(&path).unwrap())
        };
    }
    overlay
}

fn app() -> emit_and_run::Overlay {
    let overlay = emit_and_run::real_blog()
        .edit(
            "Gemfile.lock",
            "    debug (1.11.1)\n",
            "    pundit (2.5.2)\n      activesupport (>= 3.0.0)\n    debug (1.11.1)\n",
        )
        .edit("Gemfile.lock", "DEPENDENCIES\n", "DEPENDENCIES\n  pundit\n")
        .write(".bundle/config", "---\nBUNDLE_PATH: \"vendor/bundle\"\n")
        .write("roundhouse.yml", "extensions:\n  - config/roundhouse\n")
        .write("config/roundhouse/gems.yml", "source_gems:\n  - {gem: \"pundit\"}\n")
        .edit(
            "app/controllers/application_controller.rb",
            "class ApplicationController < ActionController::Base\n",
            "class ApplicationController < ActionController::Base\n  include Pundit::Authorization\n\n\
             \x20 rescue_from Pundit::NotAuthorizedError do\n    head :forbidden\n  end\n\n\
             \x20 def current_user\n    params[:user]\n  end\n",
        )
        .edit(
            "app/controllers/articles_controller.rb",
            "@articles = Article.includes(:comments).order(created_at: :desc)",
            "@articles = policy_scope(Article).order(created_at: :desc)",
        )
        .edit("app/controllers/articles_controller.rb", "  def edit\n  end", "  def edit\n    authorize @article\n  end")
        .write("app/policies/application_policy.rb", APPLICATION_POLICY)
        .write("app/policies/article_policy.rb", ARTICLE_POLICY);
    with_gem(overlay, Path::new(GEM), "vendor/bundle/ruby/3.4.0/gems/pundit-2.5.2")
}

#[test]
fn authorize_and_policy_scope_run_from_the_gem_source() {
    app()
        .run_ruby(
            r##"
def get(path, query = "")
  status, _headers, body = Main.run_rack("REQUEST_METHOD" => "GET", "PATH_INFO" => path, "QUERY_STRING" => query, "rack.input" => StringIO.new(""))
  [status, body.join]
end
def check(label, got, want)
  raise "#{label}: got #{got.inspect}, want #{want.inspect}" unless got == want
end
published = Article.create!(title: "Published", body: "body number one is long enough")
Article.create!(title: "Hidden draft", body: "body number two is long enough")
check("edit as anyone", get("/articles/#{published.id}/edit")[0], 403)
check("edit as admin", get("/articles/#{published.id}/edit", "user=admin")[0], 200)
_, body = get("/articles")
check("index as anyone", [body.include?("Published"), body.include?("Hidden draft")], [true, false])
_, body = get("/articles", "user=admin")
check("index as admin", [body.include?("Published"), body.include?("Hidden draft")], [true, true])
_, body = get("/articles", "user=nobody")
check("index as nobody", [body.include?("Published"), body.include?("Hidden draft")], [false, false])
check("rooted name", RoundhouseConstants.safe_constantize("::ArticlePolicy"), ArticlePolicy)
puts "ok"
"##,
        )
        .assert_passes();
}

#[test]
fn safe_constantize_is_a_closed_case_on_ruby_and_refused_elsewhere() {
    let dir = std::env::temp_dir().join(format!("rh-constantize-targets-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (path, text) in [
        (
            "db/schema.rb",
            "ActiveRecord::Schema.define do\n  create_table \"posts\", force: :cascade do |t|\n    t.string \"title\"\n  end\nend\n",
        ),
        ("config/routes.rb", "Rails.application.routes.draw do\n  get \"/posts\", to: \"posts#index\"\nend\n"),
        ("app/models/post.rb", "class Post < ApplicationRecord\nend\n"),
        ("app/models/post_policy.rb", "class PostPolicy\nend\n"),
        (
            "app/models/finder.rb",
            "class Finder\n  def self.policy(name)\n    \"#{name}Policy\".safe_constantize\n  end\nend\n",
        ),
        (
            "app/controllers/posts_controller.rb",
            "class PostsController < ApplicationController\n  def index\n    Finder.policy(\"Post\")\n    render plain: \"ok\"\n  end\nend\n",
        ),
    ] {
        let full = dir.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
    let mut app = roundhouse::ingest::ingest_app(&dir).expect("ingest");
    roundhouse::session::analyze_and_lower(&mut app);
    let emit = |target: BuildTarget| {
        let (files, diags) =
            roundhouse::emit::diagnostics::scope(|| roundhouse::project::target_files(&app, &dir, target));
        let errors: Vec<String> = diags
            .into_iter()
            .filter(|d| d.severity == roundhouse::diagnostic::Severity::Error)
            .map(|d| d.message)
            .collect();
        (errors, files.unwrap_or_default())
    };
    let (errors, files) = emit(BuildTarget::Ruby);
    assert!(!errors.iter().any(|e| e.contains("safe_constantize")), "ruby refused the call:\n{}", errors.join("\n"));
    let text = |file: &str| files.iter().find(|(path, _)| path.ends_with(file)).map(|(_, text)| text.clone()).unwrap_or_default();
    let finder = text("finder.rb");
    assert!(finder.contains("RoundhouseConstants.safe_constantize("), "ruby did not route the call to the lookup:\n{finder}");
    let lookup = text("roundhouse_constants.rb");
    assert!(lookup.contains("when \"PostPolicy\""), "ruby did not emit the closed case:\n{lookup}");
    for target in [BuildTarget::Spinel, BuildTarget::Typescript, BuildTarget::Rust, BuildTarget::Python] {
        let (errors, _) = emit(target);
        assert!(
            errors.iter().any(|e| e.contains("safe_constantize")),
            "{target:?} did not refuse safe_constantize:\n{}",
            errors.join("\n")
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn only_what_require_loads_is_ingested() {
    let (emitted, errors) = app().emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "errors: {errors:#?}");
    let models = emitted.join("app/models/pundit");
    assert!(models.join("policy_finder.rb").exists(), "the required gem files were not emitted");
    for unrequired in ["rspec.rb", "policy_generator.rb"] {
        assert!(!walk(&emitted).iter().any(|p| p.ends_with(unrequired)), "{unrequired} was ingested");
    }
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}
