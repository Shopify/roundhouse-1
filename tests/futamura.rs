//! `--target futamura`, stage 0: the identity.
//!
//! The target's mandate is partial evaluation — specialize what the
//! analysis resolves, leave the rest as residual Ruby running on the
//! app's real gems. Stage 0 specializes nothing, so the residue is the
//! whole app and the output must be the app as source control holds it.
//! These gates exist before the first specialization so that every
//! later one is measured against the same contract: the app's own
//! suite, run by Rails, passes against the emitted tree.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A path no other test in this process (or a concurrent run) shares: tests run on parallel
/// threads, so a name keyed by the pid alone is one directory for every test that uses it.
/// Not created; callers that need it present use `scratch`.
fn unique_dir(name: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!("roundhouse-futamura-{name}-{}-{n}", std::process::id()))
}

fn scratch(name: &str) -> PathBuf {
    let dir = unique_dir(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(root: &Path, rel: &str, content: &[u8]) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

/// A tree with source the identity must carry (dotfiles, `.keep`
/// placeholders, a binary asset, an executable) and run-time state it
/// must not (logs, databases, caches, the master key).
fn app_tree(root: &Path) {
    write(root, "Gemfile", b"source \"https://rubygems.org\"\ngem \"rails\"\n");
    write(root, ".ruby-version", b"ruby-4.0.2\n");
    write(root, ".gitignore", b"/log/*\n/tmp/*\n");
    write(root, "config/routes.rb", b"Rails.application.routes.draw do\n  get \"up\" => \"rails/health#show\", as: :rails_health_check\nend\n");
    write(root, "app/models/application_record.rb", b"class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n");
    write(root, "app/models/widget.rb", b"class Widget < ApplicationRecord\n  def method_missing(name, *args) = name\nend\n");
    write(root, "app/assets/images/logo.png", PNG);
    write(root, "bin/rails", b"#!/usr/bin/env ruby\n");
    write(root, "log/.keep", b"");
    write(root, "log/test.log", b"noise\n");
    write(root, "tmp/.keep", b"");
    write(root, "tmp/cache/x", b"cached\n");
    write(root, "storage/.keep", b"");
    write(root, "storage/test.sqlite3", b"SQLite format 3\0");
    write(root, "config/master.key", b"secret\n");
    // Not UTF-8, and nowhere an asset root: a test fixture, a doc image.
    write(root, "test/files/latin1.txt", b"caf\xe9 \xff\xfe\n");
    write(root, "docs/diagram.bin", b"\x00\x01\xc3\x28\xff");
    write(root, "docs/screenshots/shot.png", PNG);
    write(root, "bin/setup-data", b"\x7fELF\xff\xfe");
    write(root, "engines/x/node_modules/pkg/bin/tool", b"\x7fELF\xff\xfe");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for exec in ["bin/rails", "bin/setup-data"] {
            fs::set_permissions(root.join(exec), fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
}

#[test]
fn identity_carries_source_and_drops_run_time_state() {
    let src = scratch("src");
    let out = scratch("out");
    app_tree(&src);
    let status = Command::new(env!("CARGO_BIN_EXE_roundhouse"))
        .args(["--target", "futamura", "-o"])
        .arg(&out)
        .arg(&src)
        .output()
        .unwrap();
    assert!(status.status.success(), "{}", String::from_utf8_lossy(&status.stderr));
    // `method_missing` is outside what the strict targets type, and the
    // other targets drop Rails' own health controller; here both are
    // residue Rails runs, so neither is a diagnostic at all.
    let stderr = String::from_utf8_lossy(&status.stderr);
    assert!(!stderr.contains("error") && !stderr.contains("warning"), "{stderr}");

    for rel in [
        "Gemfile",
        ".ruby-version",
        ".gitignore",
        "config/routes.rb",
        "app/models/widget.rb",
        "app/assets/images/logo.png",
        "bin/rails",
        "test/files/latin1.txt",
        "docs/diagram.bin",
        "docs/screenshots/shot.png",
        "bin/setup-data",
        "log/.keep",
        "tmp/.keep",
        "storage/.keep",
    ] {
        assert_eq!(fs::read(out.join(rel)).ok(), fs::read(src.join(rel)).ok(), "{rel}");
    }
    for rel in ["engines/x/node_modules/pkg/bin/tool", "log/test.log", "tmp/cache/x", "storage/test.sqlite3", "config/master.key"] {
        assert!(!out.join(rel).exists(), "{rel} is run-time state, not source");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for rel in ["bin/rails", "bin/setup-data"] {
            let mode = fs::metadata(out.join(rel)).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "{rel} must stay executable");
        }
    }
    // Nothing is dropped silently: the exclusions are named once.
    let summary = stderr.lines().find(|l| l.contains("futamura: carried")).unwrap_or_else(|| panic!("{stderr}"));
    assert!(summary.contains("5 non-UTF-8 file(s)"), "{summary}");
    assert!(summary.contains("3 run-time state"), "{summary}");
    assert!(summary.contains("1 secrets"), "{summary}");
    assert!(summary.contains("1 dir(s) not walked"), "{summary}");
    // The source app ships no README, so the target supplies one.
    assert!(fs::read_to_string(out.join("README.md")).unwrap().contains("bin/rails test"));

    fs::remove_dir_all(&src).unwrap();
    fs::remove_dir_all(&out).unwrap();
}

/// The behavioral gate: `scripts/futamura-suite` emits `fixtures/real-blog`
/// and runs its own suite with Rails against a copy of the source and
/// against the emitted tree; the two must give the same result. The
/// script is the gate's one definition, also used for apps outside the
/// repo (campfire). Needs the generated fixture and its bundle.
#[test]
#[ignore = "requires fixtures/real-blog and its Rails bundle"]
fn real_blog_suite_gives_the_same_result_on_source_and_emitted_tree() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixture = root.join("fixtures/real-blog");
    assert!(fixture.join("Gemfile.lock").exists(), "generate fixtures/real-blog first");
    let mut cmd = Command::new(root.join("scripts/futamura-suite"));
    cmd.arg(&fixture)
        .env("ROUNDHOUSE_BIN", env!("CARGO_BIN_EXE_roundhouse"))
        .env_remove("BUNDLE_GEMFILE");
    // Whole-route replacement needs Ruby 4.1 started with RUBY_BOX=1.
    // Run this under such a Ruby with RUBY_BOX=1 set and the suite's GET
    // requests are served by the lowered app inside Rails; otherwise the
    // emitted tree runs with whole-route off, as plain Rails.
    if std::env::var("RUBY_BOX").as_deref() != Ok("1") {
        cmd.env("FUTAMURA_WHOLE_ROUTE", "off");
    }
    let run = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        run.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    // The fixture's suite is green, so SAME must mean zero failures.
    assert!(stdout.contains(" 0 failures, 0 errors"), "{stdout}");
}

/// Whole-route replacement: a GET action whose code reaches no error
/// under the `ruby` analysis is served by the lowered app; one that does
/// stays with Rails, and the ledger names the error and the path to it.
/// Writes are never considered.
#[test]
fn whole_route_lowers_clean_get_actions_and_leaves_the_rest_to_rails() {
    let src = scratch("wr-src");
    let out = scratch("wr-out");
    write(&src, "Gemfile", b"source \"https://rubygems.org\"\ngem \"rails\"\n");
    write(&src, "db/schema.rb", br#"ActiveRecord::Schema.define do
  create_table "posts", force: :cascade do |t|
    t.string "title", null: false
  end
end
"#);
    write(&src, "config/routes.rb", b"Rails.application.routes.draw do\n  resources :posts, only: [:index, :show, :create]\nend\n");
    write(&src, "app/models/application_record.rb", b"class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n");
    write(&src, "app/models/post.rb", b"class Post < ApplicationRecord\n  def shout\n    title.frobnicate\n  end\nend\n");
    write(&src, "app/controllers/application_controller.rb", b"class ApplicationController < ActionController::Base\nend\n");
    write(&src, "app/controllers/posts_controller.rb", br#"class PostsController < ApplicationController
  def index
    @posts = Post.all
  end

  def show
    @post = Post.find(params[:id])
    @shout = @post.shout
  end

  def create
    @post = Post.create!(title: params[:title])
    redirect_to posts_path
  end
end
"#);
    write(&src, "app/views/layouts/application.html.erb", b"<html><body><%= yield %></body></html>\n");
    write(&src, "app/views/posts/index.html.erb", b"<% @posts.each do |post| %><p><%= post.title %></p><% end %>\n");
    write(&src, "app/views/posts/show.html.erb", b"<h1><%= @post.title %></h1><p><%= @shout %></p>\n");

    let run = Command::new(env!("CARGO_BIN_EXE_roundhouse"))
        .args(["--target", "futamura", "-o"])
        .arg(&out)
        .arg(&src)
        .env("ROUNDHOUSE_FUTAMURA_LEDGER", "1")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "{stderr}");
    assert!(stderr.contains("whole-route: 1 GET action(s) served by the lowered app; 1 left to Rails"), "{stderr}");
    let residue = stderr.lines().find(|l| l.contains("rails ") && l.contains("posts#show")).unwrap_or_else(|| panic!("{stderr}"));
    assert!(residue.contains("app/models/post.rb:3") && residue.contains("frobnicate"), "{residue}");
    assert!(residue.contains("PostsController#show -> Post#shout"), "{residue}");

    let init = fs::read_to_string(out.join("config/initializers/futamura_routes.rb")).unwrap();
    assert!(init.contains("    posts#index\n"), "{init}");
    assert!(!init.contains("posts#show") && !init.contains("posts#create"), "{init}");
    assert!(out.join("vendor/futamura/bridge.rb").exists());
    assert!(out.join("vendor/futamura/lowered/main.rb").exists());
    // The lowered app's own tests are not part of the served program.
    assert!(!out.join("vendor/futamura/lowered/test").exists());
    // The app itself is still the app.
    assert_eq!(fs::read(out.join("app/models/post.rb")).unwrap(), fs::read(src.join("app/models/post.rb")).unwrap());
}

/// A minimal app whose `ruby` emit refuses one association (a `through:`
/// hop keyed on a non-id column, Mastodon's `Status.reblogged_by_accounts`).
fn app_with_a_ruby_refusal(src: &Path) {
    write(src, "Gemfile", b"source \"https://rubygems.org\"\ngem \"rails\"\n");
    write(src, "db/schema.rb", br#"ActiveRecord::Schema.define do
  create_table "users", force: :cascade do |t|
    t.string "name", null: false
  end
  create_table "posts", force: :cascade do |t|
    t.string "title", null: false
    t.integer "user_id", null: false
    t.integer "reblog_of_id"
  end
end
"#);
    write(src, "config/routes.rb", b"Rails.application.routes.draw do\n  resources :posts, only: [:index]\nend\n");
    write(src, "app/models/application_record.rb", b"class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n");
    write(src, "app/models/user.rb", b"class User < ApplicationRecord\nend\n");
    write(src, "app/models/post.rb", br#"class Post < ApplicationRecord
  belongs_to :user
  has_many :reblogs, foreign_key: "reblog_of_id", class_name: "Post"
  has_many :reblogged_by_users, through: :reblogs, class_name: "User", source: :user
end
"#);
    write(src, "app/controllers/application_controller.rb", b"class ApplicationController < ActionController::Base\nend\n");
    write(src, "app/controllers/posts_controller.rb", b"class PostsController < ApplicationController\n  def index\n    @posts = Post.all\n  end\nend\n");
    write(src, "app/views/posts/index.html.erb", b"<% @posts.each do |post| %><p><%= post.title %></p><% end %>\n");
}

/// The `ruby` emit's errors decide which routes stay on Rails; they are
/// not futamura's own, and must not fail its emit (Mastodon on 9de9af1e
/// exited 1 on `Status.reblogged_by_accounts`).
#[test]
fn ruby_emit_refusals_do_not_fail_the_futamura_emit() {
    let src = scratch("leak-src");
    let out = scratch("leak-out");
    app_with_a_ruby_refusal(&src);
    let run = Command::new(env!("CARGO_BIN_EXE_roundhouse"))
        .args(["--target", "futamura", "-o"])
        .arg(&out)
        .arg(&src)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "{stderr}");
    assert!(!stderr.contains("association_primary_key"), "{stderr}");
}

mod query_specialization {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use roundhouse::analyze::Analyzer;
    use roundhouse::emit::futamura::{INITIALIZER, specialize};
    use roundhouse::ingest::ingest_app_from_tree;

    const SCHEMA: &str = r#"ActiveRecord::Schema.define do
  create_table "rooms", force: :cascade do |t|
    t.string "name", null: false
  end
  create_table "messages", force: :cascade do |t|
    t.integer "room_id", null: false
    t.integer "creator_id", null: false
    t.boolean "pinned", default: false
    t.datetime "created_at", null: false
  end
  create_table "users", force: :cascade do |t|
    t.string "name", null: false
  end
end
"#;

    const MESSAGE: &str = r#"class Message < ApplicationRecord
  PAGE_SIZE = 40

  belongs_to :room
  belongs_to :creator, class_name: "User"

  scope :ordered, -> { order(:created_at) }
  scope :with_creator, -> { preload(:creator) }
  scope :last_page, -> { ordered.last(PAGE_SIZE) }
  scope :before, ->(message) { where("created_at < ?", message.created_at) }
end
"#;

    const CONTROLLER: &str = r#"class MessagesController < ApplicationController
  def index
    @room = Room.find(params[:room_id])
    @messages = @room.messages.with_creator.last_page
    @pinned = Message.where(room_id: @room.id, pinned: true).order(created_at: :desc).first(3)
    @older = @room.messages.before(@messages.first).to_a
  end
end
"#;

    fn app_and_files() -> (roundhouse::App, Vec<(String, String)>) {
        let files: Vec<(&str, &str)> = vec![
            ("db/schema.rb", SCHEMA),
            ("app/models/room.rb", "class Room < ApplicationRecord\n  has_many :messages\nend\n"),
            ("app/models/user.rb", "class User < ApplicationRecord\nend\n"),
            ("app/models/message.rb", MESSAGE),
            ("app/controllers/messages_controller.rb", CONTROLLER),
        ];
        let tree: roundhouse::det::HashMap<PathBuf, Vec<u8>> =
            files.iter().map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec())).collect();
        let mut app = ingest_app_from_tree(tree).expect("ingest");
        Analyzer::new(&app).analyze(&mut app);
        (app, files.iter().map(|(p, c)| (p.to_string(), c.to_string())).collect())
    }

    fn file<'f>(files: &'f [(String, String)], path: &str) -> &'f str {
        &files.iter().find(|(p, _)| p == path).unwrap_or_else(|| panic!("{path} not emitted")).1
    }

    #[test]
    fn association_chain_through_scopes_becomes_a_statement_with_its_owner_bound() {
        let (app, mut files) = app_and_files();
        let report = specialize(&app, &mut files).unwrap();
        let controller = file(&files, "app/controllers/messages_controller.rb");
        let init = file(&files, INITIALIZER);
        // The call site binds the owner's id (in a lambda the runtime calls once the owner is a record) and keeps the chain as fallback.
        // The scope's constant limit is read per call (Ruby evaluates `limit(PAGE_SIZE)` each time), so the call site passes it fixed.
        assert!(controller.contains(
            "@messages = Futamura.records(:controllers_messages_controller_4_17, -> { [@room.id, Futamura.fixed(::Message::PAGE_SIZE)] }, owner: @room, \
             association: :messages) { @room.messages.with_creator.last_page }"
        ), "{controller}");
        // Static scopes stay calls for Rails to build; the terminal scope is
        // inlined, its constant qualified so the initializer can name it,
        // and `last(n)` follows Rails' FinderMethods: ordered_relation,
        // limit, reverse_order, then the rows reversed. Every scope the chain
        // calls is a class method Rails answers by calling it on the class, so
        // the statement names them for the runtime to check (`via:`).
        assert!(init.contains(
            "Futamura.define(:controllers_messages_controller_4_17, \"Message\", reverse: true, via: [:with_creator, :last_page, :ordered], owner_keys: [\"room_id\"], owner_columns: [\"room_id\"], relation: true) \
             { |p| Message.all.where(room_id: p.bind).with_creator.ordered.send(:ordered_relation).limit(p.bind).reverse_order }"
        ), "{init}");
        assert!(report.specialized.iter().any(|s| s.ends_with("messages_controller.rb:4:17")), "{:?}", report.specialized);
    }

    #[test]
    fn constant_rooted_chain_binds_request_values_and_bakes_literals() {
        let (app, mut files) = app_and_files();
        specialize(&app, &mut files).unwrap();
        let controller = file(&files, "app/controllers/messages_controller.rb");
        let init = file(&files, INITIALIZER);
        assert!(controller.contains("@pinned = Futamura.records(:controllers_messages_controller_5_15, [(__fut_controllers_messages_controller_5_15_0 = (@room.id))]) { Message.where(room_id: __fut_controllers_messages_controller_5_15_0, pinned: true)"), "{controller}");
        assert!(init.contains(
            "Message.all.where(room_id: p.bind, pinned: true).order({ created_at: :desc }).send(:ordered_relation).limit(3) }"
        ), "{init}");
    }

    #[test]
    fn a_string_condition_over_a_record_is_left_to_rails_with_a_reason() {
        let (app, mut files) = app_and_files();
        let report = specialize(&app, &mut files).unwrap();
        let controller = file(&files, "app/controllers/messages_controller.rb");
        assert!(controller.contains("@older = @room.messages.before(@messages.first).to_a\n"), "{controller}");
        assert!(report.residue.iter().any(|r| r.contains("messages_controller.rb:6:")), "{:?}", report.residue);
    }
}

mod single_record_terminals {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use roundhouse::analyze::Analyzer;
    use roundhouse::emit::futamura::{INITIALIZER, specialize};
    use roundhouse::ingest::ingest_app_from_tree;

    #[test]
    fn first_without_order_takes_rails_implicit_order_and_find_by_is_where_take() {
        let schema = "ActiveRecord::Schema.define do\n  create_table \"stories\", force: :cascade do |t|\n    t.string \"short_id\", null: false\n  end\nend\n";
        let controller = "class StoriesController < ApplicationController\n  def show\n    @story = Story.where(short_id: params[:id].to_s).first\n    @same = Story.find_by(short_id: params[:id] || params[:story_id])\n  end\nend\n";
        let files: Vec<(&str, &str)> = vec![
            ("db/schema.rb", schema),
            ("app/models/story.rb", "class Story < ApplicationRecord\nend\n"),
            ("app/controllers/stories_controller.rb", controller),
        ];
        let tree: roundhouse::det::HashMap<PathBuf, Vec<u8>> =
            files.iter().map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec())).collect();
        let mut app = ingest_app_from_tree(tree).expect("ingest");
        Analyzer::new(&app).analyze(&mut app);
        let mut files: Vec<(String, String)> = files.iter().map(|(p, c)| (p.to_string(), c.to_string())).collect();
        specialize(&app, &mut files).unwrap();
        let get = |path: &str| files.iter().find(|(p, _)| p == path).unwrap().1.clone();
        let init = get(INITIALIZER);
        let out = get("app/controllers/stories_controller.rb");
        assert!(init.contains("\"Story\", one: true, via: :where) { |p| Story.all.where(short_id: p.bind).send(:ordered_relation).limit(1) }"), "{init}");
        assert!(init.contains("\"Story\", one: true, via: :find_by) { |p| Story.all.where(short_id: p.bind).limit(1) }"), "{init}");
        assert!(out.contains("[(__fut_controllers_stories_controller_3_14_0 = (params[:id].to_s))]) { Story.where(short_id: __fut_controllers_stories_controller_3_14_0).first }"), "{out}");
        assert!(out.contains("[Futamura.find_by_value((__fut_controllers_stories_controller_4_13_0 = (params[:id] || params[:story_id])))]) {"), "{out}");
    }
}

mod preload_scopes {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use roundhouse::analyze::Analyzer;
    use roundhouse::emit::futamura::{INITIALIZER, specialize};
    use roundhouse::ingest::ingest_app_from_tree;

    /// The runtime preloader builds an association's scope once, so only a
    /// scope Roundhouse can see is built from literals is certified to it.
    #[test]
    fn literal_association_scopes_are_certified_and_request_dependent_ones_are_not() {
        let schema = "ActiveRecord::Schema.define do\n  create_table \"users\", force: :cascade do |t|\n    t.string \"name\"\n  end\n  create_table \"posts\", force: :cascade do |t|\n    t.integer \"user_id\"\n    t.boolean \"published\"\n    t.integer \"viewer_id\"\n  end\nend\n";
        let user = "class User < ApplicationRecord\n  has_many :posts\n  has_many :published_posts, -> { where(published: true).order(:id) }, class_name: \"Post\"\n  has_many :seen_posts, -> { where(viewer_id: Current.user.id) }, class_name: \"Post\"\nend\n";
        let controller = "class UsersController < ApplicationController\n  def index\n    @users = User.preload(:published_posts).order(:id).first(10)\n  end\nend\n";
        let files: Vec<(&str, &str)> = vec![
            ("db/schema.rb", schema),
            ("app/models/user.rb", user),
            ("app/models/post.rb", "class Post < ApplicationRecord\n  belongs_to :user\nend\n"),
            ("app/controllers/users_controller.rb", controller),
        ];
        let tree: roundhouse::det::HashMap<PathBuf, Vec<u8>> =
            files.iter().map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec())).collect();
        let mut app = ingest_app_from_tree(tree).expect("ingest");
        Analyzer::new(&app).analyze(&mut app);
        let mut files: Vec<(String, String)> = files.iter().map(|(p, c)| (p.to_string(), c.to_string())).collect();
        specialize(&app, &mut files).unwrap();
        let init = &files.iter().find(|(p, _)| p == INITIALIZER).expect("initializer").1;
        assert!(init.contains("Futamura::Preload.static_scopes(\"User#published_posts\")\n"), "{init}");
        assert!(!init.contains("User#seen_posts"), "{init}");
    }
}

/// A pg_dump statement the structure.sql ingest does not model is
/// residue under futamura, not a refusal: Rails loads the dump itself,
/// so the file must still be carried byte for byte.
#[test]
fn unmodeled_structure_sql_statement_is_residue_not_a_refusal() {
    let src = scratch("structure-src");
    let out = scratch("structure-out");
    app_tree(&src);
    write(&src, "config/application.rb", b"module App\n  class Application < Rails::Application\n    config.active_record.schema_format = :sql\n  end\nend\n");
    let structure = b"CREATE TABLE public.widgets (\n    id bigint NOT NULL\n);\n\nPREPARE stmt AS SELECT 1;\n\nALTER TABLE ONLY public.widgets\n    ADD CONSTRAINT widgets_pkey PRIMARY KEY (id);\n";
    write(&src, "db/structure.sql", structure);
    let run = Command::new(env!("CARGO_BIN_EXE_roundhouse"))
        .args(["--target", "futamura", "-o"])
        .arg(&out)
        .arg(&src)
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(fs::read(out.join("db/structure.sql")).unwrap(), structure);
    assert!(out.join("app/models/widget.rb").exists());
}

/// Rails' multiple-database model: each configuration has its own dump
/// (`schema_dump:` or the `<name>_structure.sql` / `<name>_schema.rb`
/// default), and a model is looked up in the database its abstract base
/// class's `connects_to` names, not in the primary's.
mod multiple_databases {
    use std::path::PathBuf;

    use roundhouse::analyze::Analyzer;
    use roundhouse::emit::futamura::{INITIALIZER, specialize};
    use roundhouse::ingest::ingest_app_from_tree;

    const PRIMARY: &str = "ActiveRecord::Schema.define do\n  create_table \"rooms\", force: :cascade do |t|\n    t.string \"name\", null: false\n    t.integer \"owner_id\"\n  end\nend\n";
    // `<name>_structure.sql`: the default name of a non-primary SQL dump.
    const ANALYTICS: &str = "CREATE TABLE public.events (\n    id bigint NOT NULL,\n    kind character varying NOT NULL,\n    room_id bigint\n);\n";
    // Named by the configuration's `schema_dump:` key, merged from an anchor.
    const REPORTS: &str = "ActiveRecord::Schema.define do\n  create_table \"summaries\", force: :cascade do |t|\n    t.string \"period\", null: false\n  end\nend\n";
    const DATABASE_YML: &str = "\
reporting_defaults: &reporting_defaults
  schema_dump: \"reports_schema.rb\"
test:
  primary:
    adapter: postgresql
  analytics:
    adapter: postgresql
  reporting:
    adapter: postgresql
    <<: *reporting_defaults
";

    fn files(extra: &[(&str, &str)]) -> Vec<(String, String)> {
        let mut files: Vec<(&str, &str)> = vec![
            ("db/schema.rb", PRIMARY),
            ("db/analytics_structure.sql", ANALYTICS),
            ("db/reports_schema.rb", REPORTS),
            ("config/database.yml", DATABASE_YML),
            ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n"),
            ("app/models/analytics_record.rb", "class AnalyticsRecord < ApplicationRecord\n  self.abstract_class = true\n  connects_to database: { writing: :analytics, reading: :analytics }\nend\n"),
            ("app/models/reporting_record.rb", "class ReportingRecord < ApplicationRecord\n  self.abstract_class = true\n  connects_to database: { writing: :reporting }\nend\n"),
            ("app/models/room.rb", "class Room < ApplicationRecord\nend\n"),
            ("app/models/event.rb", "class Event < AnalyticsRecord\nend\n"),
            ("app/models/summary.rb", "class Summary < ReportingRecord\nend\n"),
        ];
        files.extend_from_slice(extra);
        files.iter().map(|(p, c)| (p.to_string(), c.to_string())).collect()
    }

    fn specialized(files: &mut Vec<(String, String)>) -> roundhouse::emit::futamura::Report {
        let tree: roundhouse::det::HashMap<PathBuf, Vec<u8>> =
            files.iter().map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec())).collect();
        let mut app = ingest_app_from_tree(tree).expect("ingest");
        Analyzer::new(&app).analyze(&mut app);
        specialize(&app, files).unwrap()
    }

    #[test]
    fn chains_on_every_database_specialize_against_that_databases_schema() {
        let mut files = files(&[(
            "app/models/report_card.rb",
            "class ReportCard < ApplicationRecord\n  def self.all_of(room, period)\n    [Room.where(name: room).to_a, Event.where(kind: room).to_a, Summary.where(period: period).to_a]\n  end\nend\n",
        )]);
        let report = specialized(&mut files);
        assert_eq!(report.specialized.len(), 3, "{:?}", report.residue);
        let init = &files.iter().find(|(p, _)| p == INITIALIZER).expect("initializer").1;
        for chain in ["Room.all.where(name: p.bind)", "Event.all.where(kind: p.bind)", "Summary.all.where(period: p.bind)"] {
            assert!(init.contains(chain), "{chain}: {init}");
        }
    }

    #[test]
    fn a_column_of_another_database_is_not_a_column_of_the_models_own() {
        // `events` has no `name` (the primary's `rooms` does) and `rooms` has
        // no `kind`: each model is checked against its own database only.
        let mut files = files(&[(
            "app/models/report_card.rb",
            "class ReportCard < ApplicationRecord\n  def self.mixed(x)\n    [Event.where(name: x).to_a, Room.where(kind: x).to_a]\n  end\nend\n",
        )]);
        let report = specialized(&mut files);
        assert!(report.specialized.is_empty(), "{:?}", report.specialized);
        assert!(report.residue.iter().any(|r| r.contains("`name`, not a column")), "{:?}", report.residue);
        assert!(report.residue.iter().any(|r| r.contains("`kind`, not a column")), "{:?}", report.residue);
    }

    #[test]
    fn a_database_without_a_dump_in_the_tree_leaves_its_models_to_rails_by_name() {
        let mut files = files(&[(
            "app/models/report_card.rb",
            "class ReportCard < ApplicationRecord\n  def self.of(x)\n    Event.where(kind: x).to_a\n  end\nend\n",
        )]);
        files.retain(|(p, _)| p != "db/analytics_structure.sql");
        let report = specialized(&mut files);
        assert!(report.specialized.is_empty(), "{:?}", report.specialized);
        assert!(
            report.residue.iter().any(|r| r.contains("database `analytics` has no schema dump") || r.contains("analytics_structure.sql")),
            "{:?}",
            report.residue
        );
    }

    #[test]
    fn a_table_the_primary_lacks_is_checked_against_the_dumps_that_hold_it() {
        // No `connects_to`: the app routes this model at run time.
        let mut files = files(&[
            ("app/models/rollup.rb", "class Rollup < ApplicationRecord\n  self.table_name = \"summaries\"\n  def self.of(x)\n    where(period: x).to_a\n  end\nend\n"),
            (
                "app/models/report_card.rb",
                "class ReportCard < ApplicationRecord\n  def self.of(x)\n    Rollup.where(period: x).to_a\n  end\nend\n",
            ),
        ]);
        let report = specialized(&mut files);
        assert!(report.specialized.iter().any(|s| s.contains("report_card.rb")), "{:?}", report.residue);
    }
}

mod rooted_constants {
    use std::path::PathBuf;

    use roundhouse::analyze::Analyzer;
    use roundhouse::emit::futamura::specialize;
    use roundhouse::ingest::ingest_app_from_tree;

    #[test]
    fn a_rooted_model_constant_roots_a_chain_like_the_bare_one() {
        let files: Vec<(&str, &str)> = vec![
            ("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table \"rooms\", force: :cascade do |t|\n    t.string \"name\", null: false\n  end\nend\n"),
            ("app/models/room.rb", "class Room < ApplicationRecord\nend\n"),
            ("app/services/lookup/finder.rb", "module Lookup\n  class Finder\n    def self.named(x)\n      ::Room.where(name: x).to_a\n    end\n  end\nend\n"),
        ];
        let tree: roundhouse::det::HashMap<PathBuf, Vec<u8>> =
            files.iter().map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec())).collect();
        let mut app = ingest_app_from_tree(tree).expect("ingest");
        Analyzer::new(&app).analyze(&mut app);
        let mut tree_files: Vec<(String, String)> = files.iter().map(|(p, c)| (p.to_string(), c.to_string())).collect();
        let report = specialize(&app, &mut tree_files).unwrap();
        assert_eq!(report.specialized.len(), 1, "{:?}", report.residue);
    }
}

mod dumps_with_unmodeled_statements {
    use std::path::PathBuf;

    use roundhouse::analyze::Analyzer;
    use roundhouse::emit::futamura::specialize;
    use roundhouse::ingest::ingest_app_from_tree;

    /// A pg_dump of a second database holds statements the SQL ingest does
    /// not model; the tables it does model still answer column lookups even
    /// when the caller did not switch survey mode on.
    #[test]
    fn a_second_databases_dump_with_a_prepare_statement_still_gives_its_columns() {
        let events = "CREATE TABLE public.events (\n    id bigint NOT NULL,\n    kind character varying NOT NULL\n);\n\nPREPARE stmt AS SELECT 1;\n\nALTER TABLE ONLY public.events\n    ADD CONSTRAINT events_pkey PRIMARY KEY (id);\n";
        let files: Vec<(&str, &str)> = vec![
            ("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table \"rooms\", force: :cascade do |t|\n    t.string \"name\"\n  end\nend\n"),
            ("db/events_structure.sql", events),
            ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n"),
            ("app/models/events_record.rb", "class EventsRecord < ApplicationRecord\n  self.abstract_class = true\n  connects_to database: { writing: :events }\nend\n"),
            ("app/models/event.rb", "class Event < EventsRecord\nend\n"),
            ("app/services/finder.rb", "class Finder\n  def self.of(x)\n    Event.where(kind: x).to_a\n  end\nend\n"),
        ];
        let tree: roundhouse::det::HashMap<PathBuf, Vec<u8>> =
            files.iter().map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec())).collect();
        let mut app = ingest_app_from_tree(tree).expect("ingest");
        Analyzer::new(&app).analyze(&mut app);
        assert!(!roundhouse::ingest::survey::is_active());
        let mut tree_files: Vec<(String, String)> = files.iter().map(|(p, c)| (p.to_string(), c.to_string())).collect();
        let report = specialize(&app, &mut tree_files).unwrap();
        assert_eq!(report.specialized.len(), 1, "{:?}", report.residue);
        assert!(!roundhouse::ingest::survey::is_active());
    }
}

/// A model's default scopes are Ruby that runs when `Model.all` is
/// evaluated (activerecord/lib/active_record/scoping/default.rb), possibly
/// reading request state or branching. The emit therefore does not decide
/// them: a chain on such a model is specialized like any other, and the
/// runtime has Rails evaluate the scopes at each call and bind what they
/// compare.
mod default_scope_runtime {
    use std::path::PathBuf;
    use std::process::Command;

    use roundhouse::analyze::Analyzer;
    use roundhouse::emit::futamura::{INITIALIZER, specialize};
    use roundhouse::ingest::ingest_app_from_tree;

    const SCHEMA: &str = "ActiveRecord::Schema.define do\n  create_table \"plain_things\", force: :cascade do |t|\n    t.string \"name\"\n  end\n  create_table \"scoped_things\", force: :cascade do |t|\n    t.string \"name\"\n  end\n  create_table \"nested_things\", force: :cascade do |t|\n    t.string \"name\"\n  end\n  create_table \"derived_things\", force: :cascade do |t|\n    t.string \"name\"\n  end\n  create_table \"dynamic_things\", force: :cascade do |t|\n    t.string \"name\"\n  end\nend\n";

    fn run() -> (roundhouse::emit::futamura::Report, Vec<(String, String)>) {
        let files: Vec<(&str, &str)> = vec![
            ("db/schema.rb", SCHEMA),
            ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n"),
            ("app/models/concerns/tenanted.rb", "module Tenanted\n  extend ActiveSupport::Concern\n  included do\n    default_scope { where(name: Current.name) }\n  end\nend\n"),
            ("app/models/concerns/wrapped.rb", "module Wrapped\n  extend ActiveSupport::Concern\n  included do\n    include Tenanted\n  end\nend\n"),
            ("app/models/base_thing.rb", "class BaseThing < ApplicationRecord\n  self.abstract_class = true\n  default_scope { order(:id) }\nend\n"),
            ("app/models/plain_thing.rb", "class PlainThing < ApplicationRecord\nend\n"),
            ("app/models/scoped_thing.rb", "class ScopedThing < ApplicationRecord\n  include Tenanted\nend\n"),
            ("app/models/nested_thing.rb", "class NestedThing < ApplicationRecord\n  include Wrapped\nend\n"),
            ("app/models/derived_thing.rb", "class DerivedThing < BaseThing\nend\n"),
            ("app/models/dynamic_thing.rb", "class DynamicThing < ApplicationRecord\n  include Module.new\nend\n"),
            (
                "app/services/finder.rb",
                "class Finder\n  def self.all_of(x)\n    [PlainThing.where(name: x).to_a, ScopedThing.where(name: x).to_a, NestedThing.where(name: x).to_a, DerivedThing.where(name: x).to_a, DynamicThing.where(name: x).to_a]\n  end\n  def self.bare(x)\n    ScopedThing.unscoped.where(name: x).to_a\n  end\nend\n",
            ),
        ];
        let tree: roundhouse::det::HashMap<PathBuf, Vec<u8>> =
            files.iter().map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec())).collect();
        let mut app = ingest_app_from_tree(tree).expect("ingest");
        Analyzer::new(&app).analyze(&mut app);
        let mut tree_files: Vec<(String, String)> = files.iter().map(|(p, c)| (p.to_string(), c.to_string())).collect();
        let report = specialize(&app, &mut tree_files).unwrap();
        (report, tree_files)
    }

    /// Reaching a `default_scope` (own, inherited, through a concern or
    /// through a module the walk cannot follow) no longer keeps a chain
    /// from being specialized: the runtime decides.
    #[test]
    fn a_chain_on_a_model_that_reaches_a_default_scope_is_specialized() {
        let (report, files) = run();
        assert_eq!(report.specialized.len(), 6, "{:?}", report.residue);
        assert!(!report.residue.iter().any(|r| r.contains("default_scope")), "{:?}", report.residue);
        let init = &files.iter().find(|(p, _)| p == INITIALIZER).unwrap().1;
        assert!(init.contains("\"ScopedThing\", via: :where) { |p| ScopedThing.all.where(name: p.bind) }"), "{init}");
    }

    /// `Model.unscoped` has no default scopes, which the runtime is told so
    /// it does not evaluate them for the statement.
    #[test]
    fn a_chain_rooted_at_unscoped_says_so() {
        let (_, files) = run();
        let init = &files.iter().find(|(p, _)| p == INITIALIZER).unwrap().1;
        assert!(init.contains("\"ScopedThing\", unscoped: true, via: :unscoped) { |p| ScopedThing.unscoped.where(name: p.bind) }"), "{init}");
        assert!(!init.contains("PlainThing, unscoped"), "{init}");
    }

    /// The run-time half, against real ActiveRecord on SQLite with
    /// FUTAMURA_VERIFY=1 (every specialized result is compared with Rails'
    /// own chain, and a mismatch raises). The probe covers a default scope
    /// that reads state and branches, one that is not a plain condition, a
    /// `default_scope` method, nil, `unscoped`, and `scoping`/`unscoped`
    /// blocks. Skipped when ActiveRecord and sqlite3 are not installed.
    #[test]
    fn default_scopes_are_bound_at_run_time_and_agree_with_rails_under_verify() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let runtime = root.join("src/emit/futamura/futamura.rb");
        let probe = root.join("tests/futamura_default_scope_runtime.rb");
        let have = Command::new("ruby").args(["-e", "require 'active_record'; require 'sqlite3'; require 'concurrent'"]).output();
        if !have.is_ok_and(|o| o.status.success()) {
            eprintln!("skipped: ruby with activerecord and sqlite3 is not available");
            return;
        }
        let output = |verify: &str| {
            let run = Command::new("ruby").arg(&probe).arg(&runtime).env("FUTAMURA_VERIFY", verify).output().unwrap();
            assert!(run.status.success(), "verify={verify}\n{}\n{}", String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&run.stderr));
            String::from_utf8_lossy(&run.stdout).to_string()
        };
        let verified = output("1");
        let normal = output("0");
        let results = |s: &str| s.lines().filter(|l| !l.starts_with("fallbacks=") && !l.starts_with("calls_") && !l.starts_with('[')).collect::<Vec<_>>().join("\n");
        // Specialized and Rails' answers agree (verify raises otherwise), and
        // the verified run changes nothing.
        assert_eq!(results(&verified), results(&normal), "{verified}\n{normal}");
        assert!(normal.contains("item/1=[1, 1]\nitem/2=[2]\nitem/3=[nil]\nitem/nil=[1]\narchived=nil\nunscoped=1\n"), "{normal}");
        // The scope ran once for a call the specialized statement served,
        // and a call it cannot serve ran Rails' chain (nil, a join/order).
        assert!(normal.contains("calls_item=[1, 0]"), "{normal}");
        // `extending` in a default scope (query_methods.rb extending!): a named
        // module that leaves the chain's links and the loaders alone is served by
        // the statement; one overriding a link the chain calls (`where`) or a
        // loader (`load`), or made by a block, runs Rails' chain.
        assert!(normal.contains("calls_ext_isle=0 built=true"), "{normal}");
        for name in ["guarded", "anon", "loader"] {
            assert!(normal.contains(&format!("calls_ext_{name}=1 built=false")), "{name}\n{normal}");
        }
        // Five refusals do not disable a model for good: refused, then
        // evaluated again within OPAQUE_RETRY calls, and simple again.
        assert!(normal.contains("flip=[true, true, true]"), "{normal}");
        assert!(normal.contains("find_by_id=[\"owned\"]"), "{normal}");
        // The columns a default scope compares are read at every call (one branch binds `shop_id`, the other
        // `code`), and a default scope added or removed between calls is seen by the next: the statement
        // serves all of them and the answers are Rails'.
        assert!(normal.contains("branch=[1, 1, 1, 1]\nbranch_shop2=[2, 1]\ngrown=[1, 2, 1]"), "{normal}");
        assert!(!normal.contains("[:branch, :fallback]") && !normal.contains("[:grown, :fallback]"), "{normal}");
        // A class method the class defines itself (a double) is not answered beside: Rails runs it.
        assert!(normal.contains("via=[1, :double, 1, :prepended]") && normal.contains("calls_via=2"), "{normal}");
        // A `scoping`/`unscoped` block is the chain's starting relation (Scoping::Named::ClassMethods#all clones it,
        // adding no default scope), so equality conditions in it are bound like a default scope's: the item calls inside
        // them are served by the statement, and only the ranked/legacy/extension/flip ones ran Rails' chain.
        assert!(normal.contains("[:ranked, :fallback] [:ranked, :fallback] [:legacy, :fallback]") && !normal.contains("[:item, :fallback]"), "{normal}");
    }

    /// The FUTAMURA_STATS block names why a call ran Rails' chain. A nil bind
    /// (a statement of its own up to MAX_NILS, else `bind_nil_many`) and an Array
    /// are named, not a statement whose model never loaded; a default scope that is not a plain condition says
    /// which check it failed; and the block's `compile` hook takes the
    /// arguments compile takes. Skipped when ActiveRecord and sqlite3 are not
    /// installed.
    #[test]
    fn stats_name_the_guard_that_sent_a_call_to_rails() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let runtime = root.join("src/emit/futamura/futamura.rb");
        let probe = root.join("tests/futamura_stats_runtime.rb");
        let have = Command::new("ruby").args(["-e", "require 'active_record'; require 'sqlite3'; require 'concurrent'"]).output();
        if !have.is_ok_and(|o| o.status.success()) {
            eprintln!("skipped: ruby with activerecord and sqlite3 is not available");
            return;
        }
        let run = Command::new("ruby").arg(&probe).arg(&runtime).output().unwrap();
        let out = String::from_utf8_lossy(&run.stdout).to_string();
        assert!(run.status.success(), "{out}\n{}", String::from_utf8_lossy(&run.stderr));
        assert!(out.contains("thing specialized=3 fallback={\"bind_array[empty]\" => 1, \"bind_nil_many\" => 1}"), "{out}");
        assert!(out.contains("ranked specialized=0 fallback={\"default_scope_values[RankedThing]\" => 1}"), "{out}");
        assert!(out.contains("scoped specialized=1 fallback={}"), "{out}");
    }

    fn cpu_probe(verify: &str) -> Option<String> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let runtime = root.join("src/emit/futamura/futamura.rb");
        let probe = root.join("tests/futamura_stats_cpu_runtime.rb");
        let have = Command::new("ruby").args(["-e", "require 'active_record'; require 'sqlite3'; require 'concurrent'"]).output();
        if !have.is_ok_and(|o| o.status.success()) {
            eprintln!("skipped: ruby with activerecord and sqlite3 is not available");
            return None;
        }
        let run = Command::new("ruby").arg(&probe).arg(&runtime).env("FUTAMURA_VERIFY", verify).output().unwrap();
        let out = String::from_utf8_lossy(&run.stdout).to_string();
        assert!(run.status.success(), "verify={verify}\n{out}\n{}", String::from_utf8_lossy(&run.stderr));
        Some(out)
    }

    /// Per-site thread-CPU accounting: a call's guard phase (up to the statement), the specialized execute and
    /// instantiate, and Rails' chain are timed apart; `FUTAMURA_STATS_AB=1` times the chain beside every specialized
    /// call after a site's first; `FUTAMURA_STATS_HELPERS=1` times each guard helper.
    #[test]
    fn stats_time_the_guard_phase_the_specialized_path_and_the_chain_in_cpu() {
        let Some(out) = cpu_probe("0") else { return };
        assert!(out.contains("calls=9 specialized=8 fallback=1\nab_calls=7 chain_runs=10\n"), "{out}");
        assert!(out.contains("guard_cpu_positive=true exec_cpu_positive=true fallback_cpu_positive=true\n"), "{out}");
        assert!(out.contains("ab_cpu_positive=true compile_cpu_positive=true\nphases_within_wall=true\nclock_read_ns=true\n"), "{out}");
        assert!(out.contains("helpers=true\nhelper_self_le_inclusive=true\ncompiles=1 compile_cpu=true\n"), "{out}");
    }

    /// The verify shadow is Rails' chain run beside a specialized call to compare: not a fallback, and not CPU of
    /// the call's phases.
    #[test]
    fn the_verify_shadow_is_not_counted_as_a_fallback() {
        let Some(out) = cpu_probe("1") else { return };
        assert!(out.contains("calls=9 specialized=8 fallback=1\nab_calls=7 "), "{out}");
        assert!(out.contains("phases_within_wall=true\n"), "{out}");
    }
}

/// A statement is built once, from the chain as written, so it may stand for the chain only
/// while every method the chain calls is ActiveRecord's own. The probe
/// (`tests/futamura_guard_runtime.rb`) runs real ActiveRecord on SQLite.
mod guards_runtime {
    use std::process::Command;

    fn probe(verify: &str) -> Option<String> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let runtime = root.join("src/emit/futamura/futamura.rb");
        let probe = root.join("tests/futamura_guard_runtime.rb");
        let have = Command::new("ruby").args(["-e", "require 'active_record'; require 'sqlite3'; require 'concurrent'"]).output();
        if !have.is_ok_and(|o| o.status.success()) {
            eprintln!("skipped: ruby with activerecord and sqlite3 is not available");
            return None;
        }
        let run = Command::new("ruby").arg(&probe).arg(&runtime).env("FUTAMURA_VERIFY", verify).output().unwrap();
        assert!(run.status.success(), "verify={verify}\n{}\n{}", String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&run.stderr));
        Some(String::from_utf8_lossy(&run.stdout).to_string())
    }

    /// Each line is `name=[[record id, calls that ran Rails' chain], ...]`.
    #[test]
    fn a_method_the_chain_calls_that_is_not_activerecords_own_runs_rails_chain() {
        let Some(normal) = probe("0") else { return };
        // `unscoped` and `where` redefined by a module the class extends read request state on
        // every call (a different row for each zone); the statement would keep the first call's.
        assert!(normal.contains("unscoped=[[1, 1], [2, 1], [1, 1]]\nwhere=[[1, 1], [2, 1]]\n"), "{normal}");
        // A scope Rails defined is answered by the statement (0 fallbacks); the same scope
        // stubbed by an anonymous prepended module (Mocha), or one the scope calls, or one the
        // class body redefined after `scope :recent`, is Rails'.
        assert!(normal.contains("scope=[[1, 0], [2, 1], [2, 1]]\nscope_redefined=[[2, 1]]\n"), "{normal}");
        // An association reader: Rails' own, then stubbed on the owner, then redefined by its class.
        assert!(normal.contains("reader=[[1, 0], [nil, 1], [1, 0]]\nreader_overridden=[[nil, 1]]\n"), "{normal}");
        // Rails reads an association from `klass.unscoped`: a redefinition that adds `where(column: value)`
        // on request state is bound at run time (the answer follows the value, no fallback), one that
        // adds an order is Rails' to run, and its condition replaces a default scope's on the same column.
        assert!(normal.contains("reader_unscoped_redefined=[[3, 0], [4, 0], [3, 0]]\nreader_unscoped_order=[[1, 1]]\nreader_unscoped_default_column=[[2, 0], [1, 0]]\n"), "{normal}");
        // A default scope's `extending` module that defines a name a scope has; a Relation builder
        // patched in by a module prepended to ActiveRecord::Relation.
        assert!(normal.contains("extension_scope=[[1, 1]]"), "{normal}");
        assert!(normal.contains("patched=[[1, 1]]\n"), "{normal}");
        // Rails' own method reached through a symlinked path to the same files is still Rails'; one defined from outside Rails is not.
        assert!(normal.contains("aliased_rails_path=[[1, 0]]\noutside_rails=[[1, 1]]\n"), "{normal}");
    }

    /// A `sig` leaves a sorbet-runtime wrapper in place of the method. The probe
    /// (`tests/futamura_sorbet_runtime.rb`) uses the gem when it loads, else a reproduction of its wrappers.
    fn sorbet_probe(verify: &str) -> Option<String> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let runtime = root.join("src/emit/futamura/futamura.rb");
        let probe = root.join("tests/futamura_sorbet_runtime.rb");
        let have = Command::new("ruby").args(["-e", "require 'active_record'; require 'sqlite3'; require 'concurrent'"]).output();
        if !have.is_ok_and(|o| o.status.success()) {
            eprintln!("skipped: ruby with activerecord and sqlite3 is not available");
            return None;
        }
        let run = Command::new("ruby").arg(&probe).arg(&runtime).env("FUTAMURA_VERIFY", verify).output().unwrap();
        assert!(run.status.success(), "verify={verify}\n{}\n{}", String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&run.stderr));
        Some(String::from_utf8_lossy(&run.stdout).to_string())
    }

    /// Each line is `name=[[record id, calls that ran Rails' chain], ...]`, each reader read twice.
    #[test]
    fn a_sorbet_wrapper_that_can_only_forward_to_rails_method_is_looked_through() {
        let Some(normal) = sorbet_probe("0") else { return };
        // An abstract declaration over Rails' reader (`defined?(super) ? super : raise`) and over a class method
        // is the method behind it: answered by the statement. A checked sig whose arguments and return are all
        // `T.untyped` around Rails' reader is too.
        assert!(normal.contains("abstract_room=[[1, 0], [1, 0]]\n"), "{normal}");
        assert!(normal.contains("unscoped=[[1, 0], [1, 0]]\n"), "{normal}");
        assert!(normal.contains("checked_room=[[1, 0], [1, 0]]\n"), "{normal}");
        // An abstract wrapper whose `super` is a method the app defines, a sig that can raise on the value
        // (a typed return) and one that replaces it (`void`) are Rails' to run.
        assert!(normal.contains("overridden_room=[[nil, 1], [nil, 1]]\n"), "{normal}");
        assert!(normal.contains("typed_room=[[1, 1], [1, 1]]\n"), "{normal}");
        assert!(normal.contains("void_reader_rails=false\n"), "{normal}");
    }

    #[test]
    fn the_sorbet_calls_agree_with_rails_under_verify() {
        let (Some(verified), Some(normal)) = (sorbet_probe("1"), sorbet_probe("0")) else { return };
        let records = |s: &str| {
            s.lines()
                .filter(|l| !l.starts_with("sorbet=") && !l.starts_with("void_reader_rails="))
                .map(|l| {
                    let (name, rest) = l.split_once('=').unwrap();
                    let ids: Vec<&str> = rest.split("], [").map(|pair| pair.trim_matches(|c| c == '[' || c == ']').split(", ").next().unwrap()).collect();
                    format!("{name}:{}", ids.join(","))
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(records(&verified), records(&normal), "{verified}\n{normal}");
    }

    #[test]
    fn the_same_calls_agree_with_rails_under_verify() {
        let (Some(verified), Some(normal)) = (probe("1"), probe("0")) else { return };
        // Verify runs Rails' chain beside every specialized call (and raises on a difference), so
        // only the records agree, not the number of chains run.
        let records = |s: &str| {
            s.lines()
                .map(|l| {
                    let (name, rest) = l.split_once('=').unwrap();
                    let ids: Vec<&str> = rest.split("], [").map(|pair| pair.trim_matches(|c| c == '[' || c == ']').split(", ").next().unwrap()).collect();
                    format!("{name}:{}", ids.join(","))
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(records(&verified), records(&normal), "{verified}\n{normal}");
    }
}

/// A StatementCache placeholder stands for one scalar. A value Rails' predicate builder expands
/// (Array, Set, Range, Relation, record, composite association key) or a test double that stands
/// for an owner never reaches one. The probe (`tests/futamura_bind_runtime.rb`) runs real
/// ActiveRecord on SQLite; each field is `<record id>/<calls that ran Rails' chain>`.
mod bind_runtime {
    use std::process::Command;

    fn probe(verify: &str) -> Option<String> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let runtime = root.join("src/emit/futamura/futamura.rb");
        let probe = root.join("tests/futamura_bind_runtime.rb");
        let have = Command::new("ruby").args(["-e", "require 'active_record'; require 'sqlite3'; require 'concurrent'"]).output();
        if !have.is_ok_and(|o| o.status.success()) {
            eprintln!("skipped: ruby with activerecord and sqlite3 is not available");
            return None;
        }
        let run = Command::new("ruby").arg(&probe).arg(&runtime).env("FUTAMURA_VERIFY", verify).output().unwrap();
        assert!(run.status.success(), "verify={verify}\n{}\n{}", String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&run.stderr));
        Some(String::from_utf8_lossy(&run.stdout).to_string())
    }

    #[test]
    fn only_a_scalar_is_bound_and_every_value_rails_expands_runs_rails_chain() {
        let Some(normal) = probe("0") else { return };
        // Scalars are answered by the statement (0 fallbacks).
        assert!(normal.contains(
            "scalar=integer:1/0 string:2/0 symbol:3/0 true:1/0 false:2/0 date:1/0 time:1/0 zoned:1/0 decimal:1/0\n"
        ), "{normal}");
        // A one-element Array is an equality like a scalar (ArrayHandler builds `build(attribute, values.first)`; longer lists are
        // statements of their own, see `list_binds`). A Set (which has no entry in StatementCache.unsupported_value?), a Range, a
        // Relation and a record are Rails': it expands them into IN, BETWEEN or a subquery, and
        // here finds the same record it would without a statement. nil is its own statement
        // (`IS NULL`), see `a_nil_value_has_a_statement_for_its_pattern`.
        assert!(normal.contains(
            "collection=array:1/0 set:1/1 range:2/1 endless:2/1 relation:3/1 empty_set:nil/1 record:2/1 nil:nil/0\n"
        ), "{normal}");
    }

    #[test]
    fn an_association_key_that_is_not_one_column_runs_rails_chain() {
        let Some(normal) = probe("0") else { return };
        // A composite target has no single key to bind. With query constraints (Rails 8.2) the
        // join keys are one column and the query keys are not: the key is read like the
        // predicate builder reads it (n/a where the Rails in use has no such option).
        assert!(normal.contains("association=single:1/0 composite_key:{} composite:1/1 constrained_key:"), "{normal}");
        let line = normal.lines().find(|l| l.starts_with("association=")).unwrap();
        assert!(line.ends_with("constrained_key:n/a constrained:n/a") || line.ends_with("constrained_key:{} constrained:1/1"), "{line}");
        // A chain that cannot be built over placeholders is Rails' too, from the first call on.
        assert!(normal.contains("unbuildable=[[2, 1], [3, 1]]\n"), "{normal}");
    }

    #[test]
    fn a_value_object_is_bound_when_the_predicate_builder_treats_it_as_a_scalar() {
        let Some(normal) = probe("0") else { return };
        // A class that responds to `to_i` and nothing the predicate builder looks at is one
        // placeholder (PredicateBuilder#build: no `id`, only the BasicObject handler); a value
        // with an `id` (read as the key), one with a handler registered for it, and a Hash
        // (nested conditions) are Rails'.
        // (A handler can be registered only where the Rails in use leaves a predicate builder unfrozen.)
        assert!(
            normal.contains("typed=value_object:3/0 record_key:2/1 handler:1/1 hash:nil/1\n")
                || normal.contains("typed=value_object:3/0 record_key:2/1 handler:n/a hash:nil/1\n"),
            "{normal}"
        );
    }

    #[test]
    fn a_nil_value_has_a_statement_for_its_pattern() {
        let Some(normal) = probe("0") else { return };
        // `where(c: nil)` is `c IS NULL`: each pattern of nil positions is a statement (no
        // fallback, and the same record Rails' chain finds); more than MAX_NILS nils is Rails'.
        assert!(normal.contains(
            "nil=none:2/0 first:5/0 second:4/0 both:6/0 none_again:3/0 first_again:5/0 missing:nil/0 many:6/1\n"
        ), "{normal}");
    }

    #[test]
    fn a_test_double_owner_is_asked_nothing() {
        let Some(normal) = probe("0") else { return };
        // The double raises for anything it was not told to expect: the binds lambda (which reads
        // the owner's key) is not called for it, and is for a record, whose chain is answered.
        assert!(normal.contains("owner=double:1/1 asked:[] record:1/0 asked_after:[:binds]\n"), "{normal}");
    }

    #[test]
    fn the_same_calls_agree_with_rails_under_verify() {
        let (Some(verified), Some(normal)) = (probe("1"), probe("0")) else { return };
        // Verify runs Rails' chain beside every specialized call and raises on a difference, so
        // only the records agree, not the number of chains run.
        let records = |s: &str| {
            s.lines()
                .filter(|l| ["scalar=", "collection=", "typed=", "nil="].iter().any(|p| l.starts_with(p)))
                .map(|l| l.split(' ').map(|t| t.rsplit_once('/').map_or(t, |(id, _)| id).to_string()).collect::<Vec<_>>().join(" "))
                .collect::<Vec<_>>()
        };
        assert_eq!(records(&verified), records(&normal), "{verified}\n{normal}");
    }
}

/// A default scope's conditions are bound at run time. Besides `where(c: v)`, the static shapes
/// `where.not(c: v)` (NotEqual) and `where(c: [..])` / `where.not(c: [..])` (HomogeneousIn,
/// `IN` / `NOT IN`) over 2..=8 values are bound too (one statement for each list length);
/// a longer list is Rails'. The probe (`tests/futamura_default_condition_runtime.rb`) runs real
/// ActiveRecord on SQLite.
mod default_condition_runtime {
    use std::process::Command;

    fn probe(verify: &str) -> Option<String> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let runtime = root.join("src/emit/futamura/futamura.rb");
        let probe = root.join("tests/futamura_default_condition_runtime.rb");
        let have = Command::new("ruby").args(["-e", "require 'active_record'; require 'sqlite3'; require 'concurrent'"]).output();
        if !have.is_ok_and(|o| o.status.success()) {
            eprintln!("skipped: ruby with activerecord and sqlite3 is not available");
            return None;
        }
        let run = Command::new("ruby").arg(&probe).arg(&runtime).env("FUTAMURA_VERIFY", verify).output().unwrap();
        assert!(run.status.success(), "verify={verify}\n{}\n{}", String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&run.stderr));
        Some(String::from_utf8_lossy(&run.stdout).to_string())
    }

    /// Each line is `name=[[record id, calls that ran Rails' chain], ...]`.
    #[test]
    fn inequality_and_short_in_list_default_scopes_are_answered_by_the_statement() {
        let Some(normal) = probe("0") else { return };
        for (model, id) in [("hidden", 3), ("open", 3), ("closed", 2), ("mixed", 3)] {
            assert!(normal.contains(&format!("{model}=[[{id}, 0], [{id}, 0], [nil, 0]]\n")), "{model}\n{normal}");
        }
        // Nine values is more than a statement is built for: Rails' chain runs.
        assert!(normal.contains("long=[[4, 1], [4, 1], [nil, 1]]\n"), "{normal}");
    }

    #[test]
    fn the_same_default_scopes_agree_with_rails_under_verify() {
        let (Some(verified), Some(normal)) = (probe("1"), probe("0")) else { return };
        let records = |s: &str| s.replace("[", "").replace("]", "").lines().map(|l| l.split(", ").step_by(2).collect::<Vec<_>>().join(",")).collect::<Vec<_>>();
        assert_eq!(records(&verified), records(&normal), "{verified}\n{normal}");
    }
}

/// A statement is built over placeholders and run with the values in order, so it must keep one
/// slot for each placeholder (`StatementCache::BindMap`). An integer IN list serializes a
/// placeholder to nil and `HomogeneousIn#casted_values` compacts it away, shifting every later
/// value by one; such a statement, one that froze the default scope of a joined model, and one whose build raises an Exception that is not a
/// StandardError, is Rails' chain to run. The probe (`tests/futamura_bind_count_runtime.rb`) runs
/// real ActiveRecord on SQLite.
mod bind_count_runtime {
    use std::process::Command;

    fn probe(verify: &str) -> Option<String> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let runtime = root.join("src/emit/futamura/futamura.rb");
        let probe = root.join("tests/futamura_bind_count_runtime.rb");
        let have = Command::new("ruby").args(["-e", "require 'active_record'; require 'sqlite3'; require 'concurrent'"]).output();
        if !have.is_ok_and(|o| o.status.success()) {
            eprintln!("skipped: ruby with activerecord and sqlite3 is not available");
            return None;
        }
        let run = Command::new("ruby").arg(&probe).arg(&runtime).env("FUTAMURA_VERIFY", verify).output().unwrap();
        assert!(run.status.success(), "verify={verify}\n{}\n{}", String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&run.stderr));
        Some(String::from_utf8_lossy(&run.stdout).to_string())
    }

    /// Each line is `name=[[record ids, calls that ran Rails' chain], ...]`.
    #[test]
    fn an_integer_in_list_keeps_its_placeholders() {
        let Some(normal) = probe("0") else { return };
        // Arel's HomogeneousIn serializes a placeholder to nil for an integer column and drops it; the statement
        // keeps the slots (`ListSlot`) and serializes each value itself, so the list over an integer column is
        // answered by the statement, as the one over a string column is.
        assert!(normal.contains("rank=[[[2], 0], [[1, 2], 0]]\n"), "{normal}");
        assert!(normal.contains("name=[[[2], 0], [[1, 2], 0]]\n"), "{normal}");
    }

    #[test]
    fn a_joined_default_scope_is_bound_at_every_call_not_frozen_into_the_statement() {
        let Some(normal) = probe("0") else { return };
        // The joined model's default scope reads the state at each call (Reflection#join_scope runs it
        // at query time): its value is a placeholder of the statement, so the statement answers all
        // three calls, each with the records Rails' chain has for that state.
        assert!(normal.contains("joined=[[[1], 0], [[2], 0], [[1], 0]]\n"), "{normal}");
    }

    #[test]
    fn joined_default_scopes_with_lists_aliases_and_a_default_scope_of_the_chain_agree_with_rails() {
        let Some(normal) = probe("0") else { return };
        // The joins' placeholders precede the chain's own in the SQL, a list is one placeholder per
        // element, and one model joined twice binds both ON clauses: every call has Rails' records
        // for its own state, from the statement alone.
        assert!(normal.contains("pets=agree true fallbacks 0 distinct 3\n"), "{normal}");
        assert!(normal.contains("pets_left=agree true fallbacks 0 distinct 2\n"), "{normal}");
        assert!(normal.contains("pets.ids=[[1, 3, 4, 5], [5], [5], [], [], [1, 3, 4, 5]]\n"), "{normal}");
    }

    #[test]
    fn a_joined_default_scope_that_is_other_conditions_than_the_statements_runs_rails_chain() {
        let Some(normal) = probe("0") else { return };
        // Inside `Vet.unscoped { }` the join has no default scope (Scoping::Named `scope_for_association`):
        // the statement was built for another set of conditions, so Rails' chain answers (and finds the unscoped rows).
        assert!(normal.contains("pets_unscoped=[[1, 2, 3, 4, 5], 1]\n"), "{normal}");
    }

    #[test]
    fn a_build_that_raises_an_exception_is_a_refusal_not_a_crash() {
        let Some(normal) = probe("0") else { return };
        assert!(normal.contains("raising=[[1, 2, 3], 1]\n"), "{normal}");
    }

    #[test]
    fn the_same_chains_agree_with_rails_under_verify() {
        let (Some(verified), Some(normal)) = (probe("1"), probe("0")) else { return };
        // Verify runs the chain as written next to the statement, so only the records are compared.
        let ids = |s: &str| s.lines().filter(|l| l.contains(".ids=")).map(String::from).collect::<Vec<_>>();
        assert_eq!(ids(&verified), ids(&normal), "{verified}\n{normal}");    }
}

/// `where(author: record_or_id)`: PredicateBuilder#expand_from_hash expands
/// a `belongs_to` name to its foreign key (and, for a polymorphic one, the
/// type column too). Only the non-polymorphic case is statically one
/// condition, so only that one specializes.
mod association_keys {
    use std::path::PathBuf;

    use roundhouse::analyze::Analyzer;
    use roundhouse::emit::futamura::{INITIALIZER, specialize};
    use roundhouse::ingest::ingest_app_from_tree;

    const SCHEMA: &str = "ActiveRecord::Schema.define do\n  create_table \"authors\", force: :cascade do |t|\n    t.string \"name\"\n  end\n  create_table \"notes\", force: :cascade do |t|\n    t.integer \"author_id\"\n    t.integer \"subject_id\"\n    t.string \"subject_type\"\n    t.string \"slug\"\n  end\nend\n";

    fn run(finder: &str) -> (roundhouse::emit::futamura::Report, Vec<(String, String)>) {
        let files: Vec<(&str, &str)> = vec![
            ("db/schema.rb", SCHEMA),
            ("app/models/author.rb", "class Author < ApplicationRecord\nend\n"),
            ("app/models/note.rb", "class Note < ApplicationRecord\n  belongs_to :author\n  belongs_to :subject, polymorphic: true\nend\n"),
            ("app/services/finder.rb", finder),
        ];
        let tree: roundhouse::det::HashMap<PathBuf, Vec<u8>> =
            files.iter().map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec())).collect();
        let mut app = ingest_app_from_tree(tree).expect("ingest");
        Analyzer::new(&app).analyze(&mut app);
        let mut tree_files: Vec<(String, String)> = files.iter().map(|(p, c)| (p.to_string(), c.to_string())).collect();
        let report = specialize(&app, &mut tree_files).unwrap();
        (report, tree_files)
    }

    #[test]
    fn a_belongs_to_key_binds_the_key_rails_would_take_from_the_value() {
        let (report, files) = run("class Finder\n  def self.of(author, slug)\n    Note.find_by(author: author, slug: slug)\n  end\nend\n");
        assert_eq!(report.specialized.len(), 1, "{:?}", report.residue);
        let init = &files.iter().find(|(p, _)| p == INITIALIZER).unwrap().1;
        // Rails expands the name itself when it builds the statement.
        assert!(init.contains("Note.all.where(author: p.bind, slug: p.bind).limit(1)"), "{init}");
        let finder = &files.iter().find(|(p, _)| p == "app/services/finder.rb").unwrap().1;
        assert!(finder.contains("[Futamura.association_key(::Note, :author, author), Futamura.find_by_value(slug)]"), "{finder}");
    }

    #[test]
    fn a_polymorphic_belongs_to_key_is_left_to_rails() {
        let (report, _) = run("class Finder\n  def self.of(subject)\n    Note.where(subject: subject).to_a\n  end\nend\n");
        assert!(report.specialized.is_empty(), "{:?}", report.specialized);
        assert!(report.residue.iter().any(|r| r.contains("polymorphic belongs_to `subject`")), "{:?}", report.residue);
    }
}

/// Links and values a chain may carry beyond `where`/`order`/`limit`.
mod more_links_and_values {
    use std::path::PathBuf;

    use roundhouse::analyze::Analyzer;
    use roundhouse::emit::futamura::{INITIALIZER, specialize};
    use roundhouse::ingest::ingest_app_from_tree;

    const SCHEMA: &str = "ActiveRecord::Schema.define do\n  create_table \"clients\", force: :cascade do |t|\n    t.string \"key\"\n    t.integer \"kind\"\n  end\n  create_table \"tenants\", force: :cascade do |t|\n    t.string \"name\"\n  end\nend\n";

    fn run(finder: &str) -> (roundhouse::emit::futamura::Report, Vec<(String, String)>) {
        let files: Vec<(&str, &str)> = vec![
            ("db/schema.rb", SCHEMA),
            ("app/models/client.rb", "class Client < ApplicationRecord\n  KIND = 3\n  default_scope { where(kind: Current.kind) }\nend\n"),
            ("app/models/tenant.rb", "class Tenant < ApplicationRecord\n  NAME = \"main\"\nend\n"),
            ("app/services/finder.rb", finder),
        ];
        let tree: roundhouse::det::HashMap<PathBuf, Vec<u8>> =
            files.iter().map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec())).collect();
        let mut app = ingest_app_from_tree(tree).expect("ingest");
        Analyzer::new(&app).analyze(&mut app);
        let mut tree_files: Vec<(String, String)> = files.iter().map(|(p, c)| (p.to_string(), c.to_string())).collect();
        let report = specialize(&app, &mut tree_files).unwrap();
        (report, tree_files)
    }

    fn init(files: &[(String, String)]) -> &str {
        &files.iter().find(|(p, _)| p == INITIALIZER).expect("initializer").1
    }

    /// Scoping::Default::ClassMethods#unscoped is `relation` without the
    /// default scopes, so the statement is rooted there and says so.
    #[test]
    fn unscoped_directly_on_a_model_is_rooted_without_default_scopes() {
        let (report, files) = run("class Finder\n  def self.by_key(key)\n    Client.unscoped.where(key: key).first\n  end\n  def self.scoped(key)\n    Client.where(key: key).first\n  end\nend\n");
        assert_eq!(report.specialized.len(), 2, "{:?}", report.residue);
        assert!(init(&files).contains("\"Client\", one: true, unscoped: true, via: :unscoped) { |p| Client.unscoped.where(key: p.bind)"), "{}", init(&files));
        assert!(init(&files).contains("\"Client\", one: true, via: :where) { |p| Client.all.where(key: p.bind)"), "{}", init(&files));
    }

    #[test]
    fn unscoped_after_other_links_is_left_to_rails() {
        let (report, _) = run("class Finder\n  def self.by_key(key)\n    Tenant.where(name: key).unscoped.first\n  end\nend\n");
        assert!(report.specialized.is_empty(), "{:?}", report.specialized);
        assert!(report.residue.iter().any(|r| r.contains("unscoped other than directly on a model")), "{:?}", report.residue);
    }

    #[test]
    fn a_constant_value_is_read_at_the_call_site_and_a_call_on_one_is_evaluated_once() {
        let (report, files) = run("class Finder\n  def self.named\n    Tenant.find_by(name: Tenant::NAME)\n  end\n  def self.again(x)\n    Tenant.find_by(name: Tenant.fetch(x))\n  end\nend\n");
        assert_eq!(report.specialized.len(), 2, "{:?}", report.residue);
        let finder = &files.iter().find(|(p, _)| p == "app/services/finder.rb").unwrap().1;
        assert!(finder.contains("Futamura.records(") && finder.contains("[Futamura.find_by_value(Tenant::NAME)]"), "{finder}");
        // A call on a constant runs class code, so it is evaluated once, ahead of the call, and the fallback reads it back.
        assert!(finder.contains("Futamura.records(:services_finder_6_5, [Futamura.find_by_value((__fut_services_finder_6_5_0 = (Tenant.fetch(x))))]) { Tenant.find_by(name: __fut_services_finder_6_5_0) }"), "{finder}");
    }

    /// core.rb `find_by` (the class method) reads a value that responds to `id`
    /// as that id before it builds the statement; Relation#find_by, which is
    /// `where(hash).take`, does not.
    #[test]
    fn find_by_on_the_class_reads_ids_as_core_find_by_does_but_on_a_relation_does_not() {
        let (report, files) = run("class Finder\n  def self.on_class(x)\n    Tenant.find_by(name: x)\n  end\n  def self.on_relation(x)\n    Tenant.where(name: \"a\").find_by(name: x)\n  end\nend\n");
        assert_eq!(report.specialized.len(), 2, "{:?}", report.residue);
        let finder = &files.iter().find(|(p, _)| p == "app/services/finder.rb").unwrap().1;
        assert!(finder.contains("[Futamura.find_by_value(x)]"), "{finder}");
        assert_eq!(finder.matches("Futamura.find_by_value").count(), 1, "{finder}");
    }

    #[test]
    fn joins_select_and_lock_with_literal_arguments_are_links() {
        let (report, files) = run("class Finder\n  def self.locked(n)\n    Tenant.joins(:clients).select(:id, :name).where(name: n).lock.to_a\n  end\n  def self.dynamic(n)\n    Tenant.select(n).to_a\n  end\nend\n");
        assert_eq!(report.specialized.len(), 1, "{:?}", report.residue);
        assert!(init(&files).contains("Tenant.all.joins(:clients).select(:id, :name).where(name: p.bind).lock("), "{}", init(&files));
        assert!(report.residue.iter().any(|r| r.contains("select with a non-literal argument")), "{:?}", report.residue);
    }
}

/// A receiver-less `where` in a class method is a call on the model itself
/// (the method's self), so its chain roots at that model.
mod implicit_class_self {
    use std::path::PathBuf;

    use roundhouse::analyze::Analyzer;
    use roundhouse::emit::futamura::{INITIALIZER, specialize};
    use roundhouse::ingest::ingest_app_from_tree;

    #[test]
    fn a_class_method_chain_roots_at_its_model_and_passes_self_to_guard_subclasses() {
        let files: Vec<(&str, &str)> = vec![
            ("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table \"pages\", force: :cascade do |t|\n    t.string \"slug\"\n    t.boolean \"live\"\n  end\nend\n"),
            (
                "app/models/page.rb",
                "class Page < ApplicationRecord\n  def self.live_by(slug)\n    where(slug: slug, live: true).order(:id).first\n  end\n  def current(slug)\n    where(slug: slug).first\n  end\nend\n",
            ),
        ];
        let tree: roundhouse::det::HashMap<PathBuf, Vec<u8>> =
            files.iter().map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec())).collect();
        let mut app = ingest_app_from_tree(tree).expect("ingest");
        Analyzer::new(&app).analyze(&mut app);
        let mut tree_files: Vec<(String, String)> = files.iter().map(|(p, c)| (p.to_string(), c.to_string())).collect();
        let report = specialize(&app, &mut tree_files).unwrap();
        assert_eq!(report.specialized.len(), 1, "{:?}", report.residue);
        let page = &tree_files.iter().find(|(p, _)| p == "app/models/page.rb").unwrap().1;
        assert!(page.contains("[slug], root: self) { where(slug: slug, live: true).order(:id).first }"), "{page}");
        let init = &tree_files.iter().find(|(p, _)| p == INITIALIZER).unwrap().1;
        assert!(init.contains("\"Page\", one: true, via: :where) { |p| Page.all.where(slug: p.bind, live: true).order(:id)"), "{init}");
        // The instance method's `where` is not a class call: it stays Rails'.
        assert!(page.contains("    where(slug: slug).first\n"), "{page}");
    }
}

mod runtime_stats {
    use std::fs;
    use std::process::Command;

    use roundhouse::emit::futamura::INITIALIZER;

    // The emitted initializer carries an opt-in measurement block. It must
    // stay inert unless FUTAMURA_STATS names a directory, and the file must
    // stay valid Ruby with it.
    #[test]
    fn emitted_runtime_has_an_opt_in_stats_block_and_stays_valid_ruby() {
        let dir = crate::unique_dir("stats");
        let _ = fs::remove_dir_all(&dir);
        let src = dir.join("src");
        let out = dir.join("out");
        fs::create_dir_all(src.join("app/models")).unwrap();
        fs::create_dir_all(src.join("config")).unwrap();
        fs::create_dir_all(src.join("db")).unwrap();
        fs::write(src.join("Gemfile"), "source \"https://rubygems.org\"\ngem \"rails\"\n").unwrap();
        fs::write(src.join("config/routes.rb"), "Rails.application.routes.draw do\nend\n").unwrap();
        fs::write(
            src.join("db/schema.rb"),
            "ActiveRecord::Schema.define do\n  create_table \"pages\", force: :cascade do |t|\n    t.string \"slug\"\n  end\nend\n",
        )
        .unwrap();
        fs::write(src.join("app/models/application_record.rb"), "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n").unwrap();
        fs::write(
            src.join("app/models/page.rb"),
            "class Page < ApplicationRecord\n  def self.live_by(slug)\n    where(slug: slug).first\n  end\nend\n",
        )
        .unwrap();
        let run = Command::new(env!("CARGO_BIN_EXE_roundhouse"))
            .args(["--target", "futamura", "-o"])
            .arg(&out)
            .arg(&src)
            .output()
            .unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        let init = fs::read_to_string(out.join(INITIALIZER)).unwrap();
        // One guarded block, after the runtime module, that counts calls per
        // site and why each fell back; FUTAMURA_STATS_AB times the chain too.
        assert!(init.contains("if ENV[\"FUTAMURA_STATS\"].to_s != \"\""), "{init}");
        assert!(init.contains("singleton_class.prepend(Stats::Instrument)"), "{init}");
        assert!(init.contains("FUTAMURA_STATS_AB"), "{init}");
        for reason in ["default_scopes", "root_not_model", "relation_needs_rails"] {
            assert!(init.contains(reason), "{reason}: {init}");
        }
        // The whole file, block included, parses.
        if let Ok(ruby) = Command::new("ruby").arg("-c").arg(out.join(INITIALIZER)).output() {
            assert!(ruby.status.success(), "{}", String::from_utf8_lossy(&ruby.stderr));
        }
    }
}

/// A concern's method is analyzed once for each class that includes it, but
/// its source is one text: copies that specialize alike are spliced once,
/// not reported as overlapping.
mod shared_concern_sites {
    use std::path::PathBuf;

    use roundhouse::analyze::Analyzer;
    use roundhouse::emit::futamura::specialize;
    use roundhouse::ingest::ingest_app_from_tree;

    #[test]
    fn one_site_in_a_concern_included_twice_is_specialized_once() {
        let files: Vec<(&str, &str)> = vec![
            ("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table \"things\", force: :cascade do |t|\n    t.string \"name\"\n  end\n  create_table \"others\", force: :cascade do |t|\n    t.string \"name\"\n  end\nend\n"),
            ("app/models/concerns/findable.rb", "module Findable\n  extend ActiveSupport::Concern\n  def same_named\n    Thing.where(name: name).to_a\n  end\nend\n"),
            ("app/models/thing.rb", "class Thing < ApplicationRecord\n  include Findable\nend\n"),
            ("app/models/other.rb", "class Other < ApplicationRecord\n  include Findable\nend\n"),
        ];
        let tree: roundhouse::det::HashMap<PathBuf, Vec<u8>> =
            files.iter().map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec())).collect();
        let mut app = ingest_app_from_tree(tree).expect("ingest");
        Analyzer::new(&app).analyze(&mut app);
        let mut tree_files: Vec<(String, String)> = files.iter().map(|(p, c)| (p.to_string(), c.to_string())).collect();
        let report = specialize(&app, &mut tree_files).unwrap();
        assert_eq!(report.specialized.len(), 1, "{:?}", report.residue);
        assert!(report.residue.is_empty(), "{:?}", report.residue);
    }
}

/// `Module#private_constant`: `Namespace::Model` raises NameError outside the
/// namespace, and the generated initializer is outside every namespace.
mod private_model_constants {
    use std::path::PathBuf;

    use roundhouse::analyze::Analyzer;
    use roundhouse::emit::futamura::specialize;
    use roundhouse::ingest::ingest_app_from_tree;

    #[test]
    fn a_model_that_makes_its_own_constant_private_is_left_to_rails() {
        let files: Vec<(&str, &str)> = vec![
            ("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table \"things\", force: :cascade do |t|\n    t.string \"name\"\n  end\n  create_table \"others\", force: :cascade do |t|\n    t.string \"name\"\n  end\nend\n"),
            ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n"),
            ("app/models/hidden/thing.rb", "module Hidden\n  class Thing < ApplicationRecord\n  end\n  private_constant(:Thing)\nend\n"),
            ("app/models/other.rb", "class Other < ApplicationRecord\nend\n"),
            ("app/services/hidden/finder.rb", "module Hidden\n  class Finder\n    def self.of(x)\n      Thing.where(name: x).to_a\n    end\n  end\nend\n"),
            ("app/services/finder.rb", "class Finder\n  def self.of(x)\n    Other.where(name: x).to_a\n  end\nend\n"),
        ];
        let tree: roundhouse::det::HashMap<PathBuf, Vec<u8>> =
            files.iter().map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec())).collect();
        let mut app = ingest_app_from_tree(tree).expect("ingest");
        Analyzer::new(&app).analyze(&mut app);
        let mut tree_files: Vec<(String, String)> = files.iter().map(|(p, c)| (p.to_string(), c.to_string())).collect();
        let report = specialize(&app, &mut tree_files).unwrap();
        assert_eq!(report.specialized.len(), 1, "{:?}", report.residue);
        assert!(report.residue.iter().any(|r| r.contains("Hidden::Thing is a private_constant")), "{:?}", report.residue);
    }
}

/// `a&.b` is Ruby's csend: it skips only its own call (`nil&.b.c` still sends `c` to nil). The
/// lowering `a && a.b` leaves the chain walkable; a nil owner is sent to the source as written
/// (`Futamura.records` runs its fallback block), and a `&.` after a relation or after the
/// terminal is not a guard at all.
mod safe_navigation {
    use std::path::PathBuf;
    use std::process::Command;

    use roundhouse::analyze::Analyzer;
    use roundhouse::emit::futamura::specialize;
    use roundhouse::ingest::ingest_app_from_tree;

    const FINDER: &str = "class Finder\n  #: (Room?) -> Array[Message]?\n  def self.to_a(room)\n    room&.messages&.to_a\n  end\n\n  #: (Room?) -> Message?\n  def self.link_then_first(room)\n    room&.messages&.where(body: [\"a\", \"b\"])&.first\n  end\n\n  #: (Room?) -> Message?\n  def self.mid_chain(room)\n    room&.messages.where(body: [\"b\", \"c\"]).first\n  end\n\n  #: (Room) -> Message?\n  def self.terminal(room)\n    room.messages.where(body: \"c\")&.first\n  end\n\n  #: (Room?) -> Message?\n  def self.by_value(room)\n    Message.find_by(room_id: room&.id)\n  end\n\n  #: (Room) -> String?\n  def self.after_load(room)\n    Message.where(room_id: room.id).order(:id).first&.body\n  end\nend\n";

    fn emit(finder: &str) -> (roundhouse::emit::futamura::Report, Vec<(String, String)>) {
        let files: Vec<(&str, &str)> = vec![
            ("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table \"rooms\", force: :cascade do |t|\n    t.string \"name\"\n  end\n  create_table \"messages\", force: :cascade do |t|\n    t.integer \"room_id\"\n    t.string \"body\"\n  end\nend\n"),
            ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n"),
            ("app/models/room.rb", "class Room < ApplicationRecord\n  has_many :messages\nend\n"),
            ("app/models/message.rb", "class Message < ApplicationRecord\n  belongs_to :room\nend\n"),
            ("app/services/finder.rb", finder),
        ];
        let tree: roundhouse::det::HashMap<PathBuf, Vec<u8>> =
            files.iter().map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec())).collect();
        let mut app = ingest_app_from_tree(tree).expect("ingest");
        Analyzer::new(&app).analyze(&mut app);
        let mut tree_files: Vec<(String, String)> = files.iter().map(|(p, c)| (p.to_string(), c.to_string())).collect();
        let report = specialize(&app, &mut tree_files).unwrap();
        (report, tree_files)
    }

    #[test]
    fn a_guard_on_the_owner_specializes_and_one_after_a_relation_or_the_terminal_is_no_guard() {
        let (report, tree_files) = emit(FINDER);
        let finder = &tree_files.iter().find(|(p, _)| p == "app/services/finder.rb").unwrap().1;
        assert_eq!(report.specialized.len(), 6, "{:?}\n{finder}", report.residue);
        assert!(!report.residue.iter().any(|r| r.contains("safe navigation")), "{:?}", report.residue);
        // The call site keeps the source (guards included) as the fallback block, and names the owner it reads.
        for source in ["room&.messages&.to_a", "room&.messages&.where(body: [\"a\", \"b\"])&.first", "room&.messages.where(body: [\"b\", \"c\"]).first", "room.messages.where(body: \"c\")&.first"] {
            let line = finder.lines().find(|l| l.contains(source)).unwrap_or_else(|| panic!("{source}\n{finder}"));
            assert!(line.contains("Futamura.records(") && line.contains("owner: room, association: :messages"), "{line}");
        }
        // The `&.` follows the terminal / sits in a value: neither is part of the chain.
        let after = finder.lines().find(|l| l.contains(".order(:id).first")).unwrap();
        assert!(after.trim_end().ends_with("&.body"), "{after}");
        assert!(finder.lines().any(|l| l.contains("Futamura.records") && l.contains("room&.id")), "{finder}");
    }

    #[test]
    fn a_guard_on_a_receiver_that_is_not_a_plain_read_is_left_to_rails() {
        let finder = "class Finder\n  #: (Array[Room]) -> Array[Message]?\n  def self.of(rooms)\n    rooms.pop&.messages&.to_a\n  end\nend\n";
        let (report, tree_files) = emit(finder);
        let source = &tree_files.iter().find(|(p, _)| p == "app/services/finder.rb").unwrap().1;
        assert_eq!(report.specialized.len(), 0, "{source}");
        assert!(!source.contains("Futamura.records"), "{source}");
        assert!(report.residue.iter().any(|r| r.contains("value is")), "{:?}", report.residue);
    }

    /// The run-time half, against real ActiveRecord on SQLite: a nil owner returns nil or raises
    /// as the source does, a record's chain is served by its statement, and under
    /// FUTAMURA_VERIFY=1 each result is compared with Rails'. Skipped when ActiveRecord and sqlite3
    /// are not installed.
    #[test]
    fn a_nil_owner_behaves_as_written_and_a_record_agrees_with_rails_under_verify() {
        let have = Command::new("ruby").args(["-e", "require 'active_record'; require 'sqlite3'; require 'concurrent'"]).output();
        if !have.is_ok_and(|o| o.status.success()) {
            eprintln!("skipped: ruby with activerecord and sqlite3 is not available");
            return;
        }
        let (_, tree_files) = emit(FINDER);
        let dir = crate::unique_dir("safe-navigation-runtime");
        let _ = std::fs::remove_dir_all(&dir);
        for (path, content) in &tree_files {
            let target = dir.join(path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, content).unwrap();
        }
        let probe = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/futamura_safe_navigation_runtime.rb");
        for verify in ["1", "0"] {
            let run = Command::new("ruby").arg(&probe).arg(&dir).env("FUTAMURA_VERIFY", verify).output().unwrap();
            let out = String::from_utf8_lossy(&run.stdout).to_string();
            assert!(run.status.success(), "verify={verify}\n{out}\n{}", String::from_utf8_lossy(&run.stderr));
            assert!(!out.contains("MISMATCH"), "verify={verify}\n{out}");
            assert_eq!(out.lines().filter(|l| l.contains(" ok ")).count(), 14, "{out}");
            assert!(out.contains("to_a nil ok :nil") && out.contains("after_load ok"), "{out}");
            assert!(out.contains("mid_chain nil ok [:no_method_error, :where]"), "{out}");
            // Unverified, only the nil owners fell back (the `nil` call of each of the four readers); every record's chain, and the `nil` value bind, was a statement.
            if verify == "0" {
                assert!(out.contains("fallbacks=4\n"), "{out}");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A local the method assigns on several paths (`scope = scope.where(..) if c`)
/// is no longer left to Rails because it is "not assigned exactly once": each
/// path gets its own statement and the call site picks it with the condition
/// Ruby tested. Only straight-line code and `if` are followed.
mod locals_assigned_on_several_paths {
    use std::path::PathBuf;

    use roundhouse::analyze::Analyzer;
    use roundhouse::emit::futamura::{INITIALIZER, specialize};
    use roundhouse::ingest::ingest_app_from_tree;

    const SCHEMA: &str = "ActiveRecord::Schema.define do\n  create_table \"items\", force: :cascade do |t|\n    t.integer \"shop_id\"\n    t.string \"status\"\n    t.string \"kind\"\n  end\nend\n";

    fn run(body: &str) -> (roundhouse::emit::futamura::Report, String, String) {
        let finder = format!("class Item < ApplicationRecord\n  def self.find_all(shop, status, kind)\n{body}\n  end\nend\n");
        let files: Vec<(&str, &str)> = vec![
            ("db/schema.rb", SCHEMA),
            ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n"),
            ("app/models/item.rb", &finder),
        ];
        let tree: roundhouse::det::HashMap<PathBuf, Vec<u8>> =
            files.iter().map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec())).collect();
        let mut app = ingest_app_from_tree(tree).expect("ingest");
        Analyzer::new(&app).analyze(&mut app);
        let mut tree_files: Vec<(String, String)> = files.iter().map(|(p, c)| (p.to_string(), c.to_string())).collect();
        let report = specialize(&app, &mut tree_files).unwrap();
        let get = |path: &str| tree_files.iter().find(|(p, _)| p == path).map(|(_, c)| c.clone()).unwrap_or_default();
        (report, get("app/models/item.rb"), get(INITIALIZER))
    }

    #[test]
    fn a_relation_reassigned_under_modifier_ifs_gets_a_statement_per_path() {
        let (report, item, init) = run(
            "    scope = Item.where(shop_id: shop)\n    scope = scope.where(status: status) if status.present?\n    scope = scope.where(kind: kind) if kind.present?\n    scope.order(:id).to_a",
        );
        assert_eq!(report.specialized.len(), 1, "{:?}", report.residue);
        for chain in [
            "Item.all.where(shop_id: p.bind).where(status: p.bind).where(kind: p.bind).order(:id) }",
            "Item.all.where(shop_id: p.bind).where(status: p.bind).order(:id) }",
            "Item.all.where(shop_id: p.bind).where(kind: p.bind).order(:id) }",
            "Item.all.where(shop_id: p.bind).order(:id) }",
        ] {
            assert!(init.contains(chain), "{chain}\n{init}");
        }
        // The call site picks a path with the conditions the `if`s tested, and
        // each path binds only the values its chain has.
        assert!(item.contains("((kind.present?) ? ((status.present?) ? Futamura.records(:models_item_6_5_tt, [shop, status, kind])"), "{item}");
        assert!(item.contains("Futamura.records(:models_item_6_5_ff, [shop])"), "{item}");
        // Every path keeps the original chain as its fallback.
        assert_eq!(item.matches("{ scope.order(:id).to_a }").count(), 4, "{item}");
    }

    #[test]
    fn an_if_else_that_assigns_a_relation_on_both_sides_is_one_statement_per_branch() {
        let (report, _, init) = run(
            "    if kind.nil?\n      scope = Item.where(shop_id: shop)\n    else\n      scope = Item.where(kind: kind)\n    end\n    scope.to_a",
        );
        assert_eq!(report.specialized.len(), 1, "{:?}", report.residue);
        assert!(init.contains("Item.all.where(shop_id: p.bind) }"), "{init}");
        assert!(init.contains("Item.all.where(kind: p.bind) }"), "{init}");
    }

    #[test]
    fn an_if_expression_as_the_receiver_of_a_chain_follows_each_branch() {
        let (report, item, init) = run(
            "    scope = if status == \"open\" then Item.where(status: \"open\") else Item.where(shop_id: shop) end\n    scope.order(:id).to_a",
        );
        assert_eq!(report.specialized.len(), 1, "{:?}", report.residue);
        assert!(item.contains("((status == \"open\") ?"), "{item}");
        assert!(init.contains("Item.all.where(status: \"open\").order(:id) }"), "{init}");
    }

    #[test]
    fn a_branch_condition_that_is_not_a_plain_read_is_left_to_rails_with_a_reason() {
        let (report, item, _) = run(
            "    scope = Item.where(shop_id: shop)\n    scope = scope.where(status: status) if Item.count > 3\n    scope.to_a",
        );
        assert_eq!(report.specialized.len(), 0);
        assert!(item.contains("    scope.to_a\n"), "{item}");
        assert!(report.residue.iter().any(|r| r.contains("branch condition is not a plain read")), "{:?}", report.residue);
    }

    #[test]
    fn a_condition_over_a_local_reassigned_before_the_call_is_left_to_rails() {
        // `status` changes after the `if` tested it: the call site would read
        // the new value.
        let (report, _, _) = run(
            "    scope = Item.where(shop_id: shop)\n    scope = scope.where(kind: kind) if status.present?\n    status = nil\n    scope.to_a",
        );
        assert_eq!(report.specialized.len(), 0);
        assert!(report.residue.iter().any(|r| r.contains("branch condition")), "{:?}", report.residue);
    }

    #[test]
    fn assignments_in_loops_and_blocks_over_the_local_are_left_to_rails() {
        let (report, _, _) = run(
            "    scope = Item.where(shop_id: shop)\n    [status, kind].each { |v| scope = scope.where(kind: v) }\n    scope.to_a",
        );
        assert_eq!(report.specialized.len(), 0);
        assert!(report.residue.iter().any(|r| r.contains("reassigned")), "{:?}", report.residue);
        let (report, _, _) = run("    scope = Item.where(shop_id: shop)\n    scope ||= Item.where(kind: kind)\n    scope.to_a");
        assert_eq!(report.specialized.len(), 0);
        let (report, _, _) = run(
            "    scope = Item.where(shop_id: shop)\n    case kind\n    when \"a\" then scope = scope.where(kind: \"a\")\n    end\n    scope.to_a",
        );
        assert_eq!(report.specialized.len(), 0, "{:?}", report.residue);
    }

    #[test]
    fn more_than_three_branches_are_left_to_rails() {
        let (report, _, _) = run(
            "    scope = Item.where(shop_id: shop)\n    scope = scope.where(kind: \"a\") if kind == \"a\"\n    scope = scope.where(kind: \"b\") if kind == \"b\"\n    scope = scope.where(kind: \"c\") if kind == \"c\"\n    scope = scope.where(kind: \"d\") if kind == \"d\"\n    scope.to_a",
        );
        assert_eq!(report.specialized.len(), 0);
        assert!(report.residue.iter().any(|r| r.contains("more than 3 branches")), "{:?}", report.residue);
    }
}

/// A request-time value is read at the call site, where the terminal is. When the chain goes
/// through a local (`scope = Item.where(shop_id: shop)` ... `scope.to_a`) that is later than
/// the `where` ran, so a value that can change in between would be read late and bound
/// differently from what the original chain used. The assignment keeps what it evaluated in a
/// local (`__fut_cap_*`, spliced over the value) and the site reads that, as long as the relation
/// local is assigned once; a local assigned on several paths keeps such a site Rails'.
mod bind_timing {
    use std::path::PathBuf;

    use roundhouse::analyze::Analyzer;
    use roundhouse::emit::futamura::specialize;
    use roundhouse::ingest::ingest_app_from_tree;

    const SCHEMA: &str = "ActiveRecord::Schema.define do\n  create_table \"items\", force: :cascade do |t|\n    t.integer \"shop_id\"\n    t.string \"kind\"\n  end\nend\n";

    fn run_files(body: &str) -> (roundhouse::emit::futamura::Report, String) {
        let finder = format!("class Item < ApplicationRecord\n  def self.find_all(shop, kind)\n{body}\n  end\nend\n");
        let files: Vec<(&str, &str)> = vec![
            ("db/schema.rb", SCHEMA),
            ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n"),
            ("app/models/item.rb", &finder),
        ];
        let tree: roundhouse::det::HashMap<PathBuf, Vec<u8>> =
            files.iter().map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec())).collect();
        let mut app = ingest_app_from_tree(tree).expect("ingest");
        Analyzer::new(&app).analyze(&mut app);
        let mut tree_files: Vec<(String, String)> = files.iter().map(|(p, c)| (p.to_string(), c.to_string())).collect();
        let report = specialize(&app, &mut tree_files).unwrap();
        let item = tree_files.into_iter().find(|(p, _)| p == "app/models/item.rb").unwrap().1;
        (report, item)
    }

    fn run(body: &str) -> roundhouse::emit::futamura::Report {
        run_files(body).0
    }

    fn timing_residue(report: &roundhouse::emit::futamura::Report) -> bool {
        report.residue.iter().any(|r| r.contains("may change between"))
    }

    #[test]
    fn a_value_nothing_reassigns_is_read_late_harmlessly() {
        // A parameter, or a local written once before the chain, has the same value at the call site.
        let (report, item) = run_files("    scope = Item.where(shop_id: shop)\n    scope.to_a");
        assert_eq!(report.specialized.len(), 1, "{:?}", report.residue);
        assert!(!item.contains("__fut_cap"), "{item}");
        let report = run("    id = shop\n    scope = Item.where(shop_id: id)\n    scope.to_a");
        assert_eq!(report.specialized.len(), 1, "{:?}", report.residue);
        // A chain written as one expression reads its values when the call does.
        let report = run("    shop = 7\n    Item.where(shop_id: shop).to_a");
        assert_eq!(report.specialized.len(), 1, "{:?}", report.residue);
    }

    #[test]
    fn a_local_reassigned_after_the_relation_was_built_is_captured_at_the_assignment() {
        // Rails' chain bound the old `shop`: the assignment keeps it, the site binds the kept value.
        let (report, item) = run_files("    scope = Item.where(shop_id: shop)\n    shop = 7\n    scope.to_a");
        assert_eq!(report.specialized.len(), 1, "{:?}", report.residue);
        assert!(item.contains("scope = Item.where(shop_id: (__fut_cap_models_item_3_33 = (shop)))"), "{item}");
        assert!(item.contains("Futamura.records(:models_item_5_5, [__fut_cap_models_item_3_33]) { scope.to_a }"), "{item}");
        let (report, item) = run_files("    id = 1\n    scope = Item.where(shop_id: id)\n    id = 2\n    scope.to_a");
        assert_eq!(report.specialized.len(), 1, "{:?}", report.residue);
        assert!(item.contains("[__fut_cap_models_item_4_33]"), "{item}");
    }

    #[test]
    fn an_instance_variable_written_in_the_method_is_captured_at_the_assignment() {
        let (report, item) = run_files("    scope = Item.where(shop_id: @shop)\n    scope.to_a");
        assert_eq!(report.specialized.len(), 1, "{:?}", report.residue);
        assert!(!item.contains("__fut_cap"), "{item}");
        let (report, item) = run_files("    scope = Item.where(shop_id: @shop)\n    @shop = 7\n    scope.to_a");
        assert_eq!(report.specialized.len(), 1, "{:?}", report.residue);
        assert!(item.contains("scope = Item.where(shop_id: (__fut_cap_models_item_3_33 = (@shop)))"), "{item}");
    }

    #[test]
    fn a_value_a_call_may_change_is_captured_at_the_assignment() {
        // Nothing runs between the `where` and the terminal but more links on the relation.
        let (report, item) = run_files("    scope = Item.where(shop_id: shop.id)\n    scope = scope.where(kind: kind)\n    scope.to_a");
        // The local is assigned twice, so nothing is kept: the value is read again at the terminal
        // (nothing but links ran in between, which `Flow::same_value_later` checks).
        assert_eq!(report.specialized.len(), 1, "{:?}", report.residue);
        assert!(!item.contains("__fut_cap"), "{item}");
        // Another call may change `shop.id`; the terminal may run later, in a block, as many times as it is called.
        for body in [
            "    scope = Item.where(shop_id: shop.id)\n    shop.reload\n    scope.to_a",
            "    scope = Item.where(shop_id: shop.id)\n    [1].each { scope.to_a }",
        ] {
            let (report, item) = run_files(body);
            assert_eq!(report.specialized.len(), 1, "{:?}", report.residue);
            assert!(item.contains("scope = Item.where(shop_id: (__fut_cap_models_item_3_33 = (shop.id)))"), "{item}");
            assert!(item.contains("[__fut_cap_models_item_3_33]) {"), "{item}");
        }
    }

    #[test]
    fn a_relation_local_assigned_on_several_paths_is_left_to_rails_when_a_value_may_change() {
        let report = run("    scope = Item.where(shop_id: shop)\n    scope = scope.where(kind: kind) if kind\n    shop = 7\n    scope.to_a");
        assert_eq!(report.specialized.len(), 0, "{:?}", report.residue);
        assert!(timing_residue(&report), "{:?}", report.residue);
    }

    #[test]
    fn a_value_omitted_hash_entry_through_a_local_keeps_its_key() {
        let (report, item) = run_files("    scope = Item.where(shop_id:)\n    shop = 7\n    scope.to_a");
        assert_eq!(report.specialized.len(), 1, "{:?}", report.residue);
        assert!(item.contains("scope = Item.where(shop_id: (__fut_cap_models_item_3_24 = (shop_id)))"), "{item}");
    }
}

mod flow_paths_runtime {
    use std::path::PathBuf;
    use std::process::Command;

    use roundhouse::analyze::Analyzer;
    use roundhouse::emit::futamura::specialize;
    use roundhouse::ingest::ingest_app_from_tree;

    /// The run-time half, against real ActiveRecord on SQLite: every path of
    /// a relation built under `if`s is served by its own statement, and under
    /// FUTAMURA_VERIFY=1 each is compared with Rails' own chain (a mismatch
    /// raises). Skipped when ActiveRecord and sqlite3 are not installed.
    #[test]
    fn each_path_of_a_conditionally_built_relation_agrees_with_rails_under_verify() {
        let have = Command::new("ruby").args(["-e", "require 'active_record'; require 'sqlite3'; require 'concurrent'"]).output();
        if !have.is_ok_and(|o| o.status.success()) {
            eprintln!("skipped: ruby with activerecord and sqlite3 is not available");
            return;
        }
        let schema = "ActiveRecord::Schema.define do\n  create_table \"items\", force: :cascade do |t|\n    t.integer \"shop_id\"\n    t.string \"status\"\n    t.string \"kind\"\n  end\n  create_table \"notes\", force: :cascade do |t|\n    t.integer \"item_id\"\n    t.string \"body\"\n  end\nend\n";
        let model = "class Item < ApplicationRecord\n  has_many :notes\n  def recent_notes\n    notes.order(id: :desc).first(2)\n  end\n  def top_notes\n    notes.first(2)\n  end\n  def self.find_all(shop, status, kind)\n    scope = Item.where(shop_id: shop)\n    scope = scope.where(status: status) if status.present?\n    scope = scope.where(kind: kind) if kind.present?\n    scope.order(:id).to_a\n  end\nend\n";
        let files: Vec<(&str, &str)> = vec![
            ("db/schema.rb", schema),
            ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n"),
            ("app/models/item.rb", model),
            ("app/models/note.rb", "class Note < ApplicationRecord\n  belongs_to :item\nend\n"),
        ];
        let tree: roundhouse::det::HashMap<PathBuf, Vec<u8>> =
            files.iter().map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec())).collect();
        let mut app = ingest_app_from_tree(tree).expect("ingest");
        Analyzer::new(&app).analyze(&mut app);
        let mut tree_files: Vec<(String, String)> = files.iter().map(|(p, c)| (p.to_string(), c.to_string())).collect();
        let report = specialize(&app, &mut tree_files).unwrap();
        assert_eq!(report.specialized.len(), 3, "{:?}", report.residue);
        let dir = crate::unique_dir("flow-runtime");
        let _ = std::fs::remove_dir_all(&dir);
        for (path, content) in &tree_files {
            let target = dir.join(path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, content).unwrap();
        }
        let probe = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/futamura_flow_runtime.rb");
        for verify in ["1", "0"] {
            let run = Command::new("ruby").arg(&probe).arg(&dir).env("FUTAMURA_VERIFY", verify).output().unwrap();
            let out = String::from_utf8_lossy(&run.stdout).to_string();
            assert!(run.status.success(), "verify={verify}\n{out}\n{}", String::from_utf8_lossy(&run.stderr));
            assert!(!out.contains("MISMATCH"), "verify={verify}\n{out}");
            assert_eq!(out.lines().filter(|l| l.contains(" ok ")).count(), 9, "{out}");
            // A terminal on the reader itself reads the filled target: Rails'.
            assert!(out.contains("target_terminal_fallbacks=1"), "{out}");
            // Unverified, only that call fell back to Rails' chain: every
            // other path was served by its statement (verifying runs the
            // fallback to compare).
            if verify == "0" {
                assert!(out.contains("fallbacks=1\n"), "{out}");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// `lines.order(:id).first(2)` in an instance method of a model that declares
/// `has_many :lines` is the association of `self`.
mod implicit_self_association {
    use std::path::PathBuf;

    use roundhouse::analyze::Analyzer;
    use roundhouse::emit::futamura::{INITIALIZER, specialize};
    use roundhouse::ingest::ingest_app_from_tree;

    const SCHEMA: &str = "ActiveRecord::Schema.define do\n  create_table \"orders\", force: :cascade do |t|\n    t.string \"name\"\n  end\n  create_table \"lines\", force: :cascade do |t|\n    t.integer \"order_id\"\n    t.string \"sku\"\n  end\nend\n";

    fn run(order: &str) -> (roundhouse::emit::futamura::Report, String, String) {
        let files: Vec<(&str, &str)> = vec![
            ("db/schema.rb", SCHEMA),
            ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n"),
            ("app/models/line.rb", "class Line < ApplicationRecord\n  belongs_to :order\nend\n"),
            ("app/models/order.rb", order),
        ];
        let tree: roundhouse::det::HashMap<PathBuf, Vec<u8>> =
            files.iter().map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec())).collect();
        let mut app = ingest_app_from_tree(tree).expect("ingest");
        Analyzer::new(&app).analyze(&mut app);
        let mut tree_files: Vec<(String, String)> = files.iter().map(|(p, c)| (p.to_string(), c.to_string())).collect();
        let report = specialize(&app, &mut tree_files).unwrap();
        let get = |path: &str| tree_files.iter().find(|(p, _)| p == path).map(|(_, c)| c.clone()).unwrap_or_default();
        (report, get("app/models/order.rb"), get(INITIALIZER))
    }

    #[test]
    fn a_receiverless_has_many_reader_roots_the_chain_at_self() {
        let (report, order, init) = run("class Order < ApplicationRecord\n  has_many :lines\n  def first_lines\n    lines.order(:id).first(2)\n  end\nend\n");
        assert_eq!(report.specialized.len(), 1, "{:?}", report.residue);
        assert!(order.contains("Futamura.records(:models_order_4_5, -> { [self.id] }, owner: self, association: :lines) { lines.order(:id).first(2) }"), "{order}");
        assert!(init.contains("\"Line\", owner_keys: [\"order_id\"], owner_columns: [\"order_id\"], relation: true) { |p| Line.all.where(order_id: p.bind).order(:id).send(:ordered_relation).limit(2) }"), "{init}");
    }

    #[test]
    fn a_reader_the_model_redefines_or_gives_options_is_left_to_rails() {
        let (report, _, _) = run("class Order < ApplicationRecord\n  has_many :lines\n  def lines = super.where(sku: \"x\")\n  def first_lines\n    lines.order(:id).first(2)\n  end\nend\n");
        assert_eq!(report.specialized.len(), 0);
        assert!(report.residue.iter().any(|r| r.contains("also defined as a method")), "{:?}", report.residue);
        // A literal scope lambda is modeled (association_scopes_and_conditions);
        // `through:` is not.
        let (report, _, _) = run("class Order < ApplicationRecord\n  has_many :lines, through: :shipments\n  def first_lines\n    lines.order(:id).first(2)\n  end\nend\n");
        assert_eq!(report.specialized.len(), 0);
        assert!(report.residue.iter().any(|r| r.contains("options the specializer does not model")), "{:?}", report.residue);
    }
}

/// has_many readers with a scope lambda or `as:`, `where.not`, SQL fragments
/// with literal arguments, and literal IN lists
/// (tests/fixtures/futamura_assoc_app).
mod association_scopes_and_conditions {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use roundhouse::analyze::Analyzer;
    use roundhouse::emit::futamura::{INITIALIZER, specialize};
    use roundhouse::ingest::ingest_app_from_tree;

    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/futamura_assoc_app")
    }

    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.push(path);
            }
        }
    }

    fn run() -> (roundhouse::emit::futamura::Report, Vec<(String, String)>) {
        let root = fixture();
        let mut paths = Vec::new();
        walk(&root, &mut paths);
        let files: Vec<(String, String)> =
            paths.iter().map(|p| (p.strip_prefix(&root).unwrap().to_string_lossy().to_string(), fs::read_to_string(p).unwrap())).collect();
        let tree: roundhouse::det::HashMap<PathBuf, Vec<u8>> = files.iter().map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec())).collect();
        let mut app = ingest_app_from_tree(tree).expect("ingest");
        Analyzer::new(&app).analyze(&mut app);
        let mut tree_files = files;
        let report = specialize(&app, &mut tree_files).unwrap();
        (report, tree_files)
    }

    fn init(files: &[(String, String)]) -> &str {
        &files.iter().find(|(p, _)| p == INITIALIZER).expect("initializer").1
    }

    /// association_scope.rb `last_chain_scope` adds the key, then the type of
    /// a polymorphic `as:`; `add_constraints` adds the scope lambda's
    /// conditions and orders after the key. The runtime is told the key
    /// columns to check against the reflection, and every column the
    /// association constrains.
    #[test]
    fn a_has_many_reader_with_a_literal_scope_or_as_is_specialized() {
        let (report, files) = run();
        let init = init(&files);
        assert!(
            init.contains("owner_keys: [\"room_id\"], owner_columns: [\"room_id\", \"pinned\"], loads: true) { |p| Message.all.where(room_id: p.bind).where(pinned: true).order(:id) }"),
            "{init}"
        );
        assert!(init.contains("Message.all.where(room_id: p.bind).order(\"id DESC\").send(:ordered_relation).limit(2)"), "{init}");
        assert!(
            init.contains("\"Note\", owner_keys: [\"subject_id\", \"subject_type\"], owner_columns: [\"subject_id\", \"subject_type\"], loads: true) { |p| Note.all.where(subject_id: p.bind).where(subject_type: p.bind) }"),
            "{init}"
        );
        let finder = &files.iter().find(|(p, _)| p == "app/services/finder.rb").unwrap().1;
        assert!(finder.contains("-> { [room.id, room.class.polymorphic_name] }, owner: room, association: :notes"), "{finder}");
        // A scope that reads the owner is evaluated per owner: Rails'.
        assert!(report.residue.iter().any(|r| r.contains("has_many `scoped_messages` has options")), "{:?}", report.residue);
    }

    /// WhereChain#not negates the conditions it is given; a SQL fragment with
    /// literal arguments is sanitized into the statement when Rails builds
    /// it, so a request-time value cannot be one of them.
    #[test]
    fn where_not_fragments_and_literal_lists_are_specialized() {
        let (report, files) = run();
        let init = init(&files);
        assert!(init.contains("Message.all.where.not(kind: [\"visible\"]).order(:id)"), "{init}");
        assert!(init.contains("Message.all.where(kind: [\"visible\", \"hidden\"]).order(:id)"), "{init}");
        assert!(init.contains("Message.all.where.not(\"body IS NULL\").order(:id)"), "{init}");
        assert!(init.contains("Message.all.where(room_id: p.bind).where(\"body LIKE ?\", \"a%\").order(:id)"), "{init}");
        assert!(init.contains("Message.all.where(room_id: p.bind).where.not(body: nil).order(:id)"), "{init}");
        assert!(report.residue.iter().any(|r| r.contains("where with a SQL fragment and a request-time value")), "{:?}", report.residue);
    }

    /// The run-time half, against real ActiveRecord on SQLite with
    /// FUTAMURA_VERIFY=1 (every specialized result is compared with Rails'
    /// own chain, and a mismatch raises). Covers the guards: an association
    /// that is loaded or holds an unsaved record answers from memory, and a
    /// default scope's condition on a column the association constrains is
    /// replaced by the association's, so those calls run Rails' chain.
    /// Skipped when ActiveRecord and sqlite3 are not installed.
    #[test]
    fn association_statements_agree_with_rails_under_verify() {
        let have = Command::new("ruby").args(["-e", "require 'active_record'; require 'sqlite3'; require 'concurrent'"]).output();
        if !have.is_ok_and(|o| o.status.success()) {
            eprintln!("skipped: ruby with activerecord and sqlite3 is not available");
            return;
        }
        let out = crate::unique_dir("assoc");
        let _ = fs::remove_dir_all(&out);
        let emit = Command::new(env!("CARGO_BIN_EXE_roundhouse")).args(["--target", "futamura", "-o"]).arg(&out).arg(fixture()).output().unwrap();
        assert!(emit.status.success(), "{}", String::from_utf8_lossy(&emit.stderr));
        let probe = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/futamura_assoc_runtime.rb");
        let output = |verify: &str| {
            let run = Command::new("ruby").arg(&probe).arg(&out).env("FUTAMURA_VERIFY", verify).output().unwrap();
            assert!(run.status.success(), "verify={verify}\n{}\n{}", String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&run.stderr));
            String::from_utf8_lossy(&run.stdout).to_string()
        };
        // Verify mode runs Rails' chain as well, so it counts every call as a
        // fallback: only its results are compared.
        let results = |s: &str| s.lines().map(|l| l.split(" spec=").next().unwrap().to_string()).collect::<Vec<_>>().join("\n");
        let verified = output("1");
        let normal = output("0");
        assert_eq!(results(&verified), results(&normal), "{verified}\n{normal}");
        let specialized = |name: &str| normal.lines().any(|l| l.starts_with(&format!("{name}=")) && l.ends_with(" spec=1 fall=0"));
        let fell_back = |name: &str| normal.lines().any(|l| l.starts_with(&format!("{name}=")) && l.ends_with(" spec=0 fall=1"));
        for name in ["not_blank", "pinned", "pinned_ordered", "loaded_chain", "unsaved_chain", "first_pinned", "newest", "notes", "same_column", "fragment", "kinds", "kinds_not", "not_fragment", "remarks"] {
            assert!(specialized(name), "{name}\n{normal}");
        }
        for name in ["loaded", "unsaved", "first_pinned_loaded"] {
            assert!(fell_back(name), "{name}\n{normal}");
        }
        // The association's own condition on a column replaces the default scope's (WhereClause#merge).
        assert!(normal.contains("remarks=[1] "), "{normal}");
        // The reader's own target is not consulted once a link follows it.
        assert!(normal.contains("loaded_chain=[1, 3] ") && normal.contains("unsaved_chain=[1, 3] "), "{normal}");
        assert!(normal.contains("unsaved=[1, 3, nil] "), "{normal}");
        assert!(normal.contains("notes=[1] "), "{normal}");
        let _ = fs::remove_dir_all(&out);
    }
}

/// What a site prints for the values it binds, and for the constants its statement's block names.
/// The call site is inside the original lexical scope, so a bound value is the expression as the
/// source wrote it: a constant is not rooted at its namespace (`Namespace::Klass::NAME` is a
/// NameError for a `private_constant`, Module#private_constant), and an expression the Ruby
/// emitter has no lowering for is not printed as a `raise`. The statement's block runs in the
/// initializer, outside that scope, so a private constant there is bound from the call site too.
/// The probe (`tests/futamura_print_runtime.rb`) runs the emitted tree on real ActiveRecord.
mod printed_values {
    use super::{Command, fs, scratch, write};

    const FINDER: &str = "\
module Lookup
  class Finder
    KIND = \"k\"
    LIMIT = 2
    PAGE = 1 + 1
    private_constant :KIND, :LIMIT, :PAGE

    def self.by_kind(shop_id)
      Widget.where(shop_id: shop_id, kind: KIND).to_a
    end

    def self.omitted(shop_id)
      Widget.where(shop_id:, kind: KIND).to_a
    end

    def self.limited(shop_id)
      Widget.where(shop_id: shop_id).limit(LIMIT).to_a
    end

    def self.paged(shop_id)
      Widget.where(shop_id: shop_id).limit(PAGE).to_a
    end

    def self.hidden_cap_for(shop_id)
      Widget.where(shop_id: shop_id).hidden_cap.to_a
    end

    def self.capped_for(shop_id)
      Widget.where(shop_id: shop_id).capped.to_a
    end

    #: (singleton(ActiveRecord::Base) model) -> Widget?
    def self.by_model(model)
      Widget.find_by(owner_type: model.polymorphic_name, shop_id: 1)
    end
  end
end
";

    fn emit() -> (std::path::PathBuf, String) {
        let src = scratch("print-src");
        let out = scratch("print-out");
        write(&src, "db/schema.rb", b"ActiveRecord::Schema.define do\n  create_table \"owners\", force: :cascade do |t|\n    t.string \"name\"\n  end\n  create_table \"widgets\", force: :cascade do |t|\n    t.string \"name\"\n    t.string \"kind\"\n    t.string \"owner_type\"\n    t.integer \"owner_id\"\n    t.integer \"shop_id\"\n  end\nend\n");
        write(&src, "app/models/application_record.rb", b"class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n");
        write(&src, "app/models/widget.rb", b"class Widget < ApplicationRecord\n  CAP = 2\n  HIDDEN = 2\n  private_constant :HIDDEN\n  scope :capped, -> { order(:id).limit(CAP) }\n  scope :hidden_cap, -> { order(:id).limit(HIDDEN) }\nend\n");
        write(&src, "app/models/owner.rb", b"class Owner < ApplicationRecord\n  has_many :widgets, as: :owner\nend\n");
        write(&src, "app/services/lookup/finder.rb", FINDER.as_bytes());
        let run = Command::new(env!("CARGO_BIN_EXE_roundhouse")).args(["--target", "futamura", "-o"]).arg(&out).arg(&src).output().unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        let finder = fs::read_to_string(out.join("app/services/lookup/finder.rb")).unwrap();
        let _ = fs::remove_dir_all(&src);
        (out, finder)
    }

    #[test]
    fn bound_values_print_as_written_and_private_constants_are_not_rooted() {
        let (out, finder) = emit();
        // Read at the call site, inside the class body: the bare name.
        assert!(finder.contains("[shop_id, KIND]) {"), "{finder}");
        // A value-omitted hash entry (`shop_id:`, Ruby 3.1) binds the name, not the key text.
        assert!(!finder.contains("[shop_id:"), "{finder}");
        assert_eq!(finder.matches("[shop_id, KIND]) {").count(), 2, "{finder}");
        assert!(!finder.contains("Finder::KIND"), "{finder}");
        // A receiver the emitter has no lowering for is printed from the source.
        assert!(finder.contains("Futamura.find_by_value((__fut_services_lookup_finder_34_7_0 = (model.polymorphic_name)))"), "{finder}");
        assert!(!finder.contains("not supported"), "{finder}");
        // A constant in the block is public and rooted, a literal, or bound; never private and rooted.
        let init = fs::read_to_string(out.join("config/initializers/futamura.rb")).unwrap();
        assert!(!init.contains("Finder::PAGE") && !init.contains("Finder::LIMIT"), "{init}");
        // A constant limit is never part of the statement's text: the site reads it per call.
        assert!(finder.contains("[shop_id, Futamura.fixed(PAGE)]"), "{finder}");
        assert!(finder.contains("[shop_id, Futamura.fixed(LIMIT)]"), "{finder}");
        assert_eq!(init.matches(".limit(p.bind)").count(), 3, "{init}");
        // A private constant in a scope body cannot be named from the call site: that site stays Rails'.
        assert_eq!(finder.matches("Futamura.records(").count(), 6, "{finder}");
        let _ = fs::remove_dir_all(&out);
    }

    #[test]
    fn the_emitted_finder_runs_on_activerecord_with_and_without_verify() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let have = Command::new("ruby").args(["-e", "require 'active_record'; require 'sqlite3'; require 'concurrent'"]).output();
        if !have.is_ok_and(|o| o.status.success()) {
            eprintln!("skipped: ruby with activerecord and sqlite3 is not available");
            return;
        }
        let (out, _) = emit();
        let probe = root.join("tests/futamura_print_runtime.rb");
        let output = |verify: &str| {
            let run = Command::new("ruby").arg(&probe).arg(&out).env("FUTAMURA_VERIFY", verify).output().unwrap();
            assert!(run.status.success(), "verify={verify}\n{}\n{}", String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&run.stderr));
            String::from_utf8_lossy(&run.stdout).to_string()
        };
        let verified = output("1");
        let normal = output("0");
        // Verify counts every call as a fallback; only the results are compared.
        let results = |s: &str| s.lines().filter(|l| !l.starts_with("specialized=")).collect::<Vec<_>>().join("\n");
        assert_eq!(results(&verified), results(&normal));
        // Every call ran specialized: a constant limit is a statement per value, not a refusal.
        assert!(normal.ends_with("specialized=9\n"), "{normal}");
        assert!(normal.contains("paged=[\"a\", \"b\"]\n"), "{normal}");
        assert!(normal.contains("capped=[\"a\", \"b\"]\n"), "{normal}");
        // A stubbed constant changes the next call's limit; restoring it restores the first statement's result.
        assert!(normal.contains("limited_stubbed=[\"a\"]\n"), "{normal}");
        assert!(normal.contains("paged_stubbed=[\"a\", \"b\", \"c\"]\n"), "{normal}");
        assert!(normal.contains("capped_stubbed=[\"a\"]\n"), "{normal}");
        assert!(normal.contains("limited_restored=[\"a\", \"b\"]\n"), "{normal}");
        assert!(normal.contains("by_kind=[\"a\", \"b\", \"c\"]\n"), "{normal}");
        assert!(normal.contains("limited=[\"a\", \"b\"]\n"), "{normal}");
        assert!(normal.contains("by_model=[\"d\"]\n"), "{normal}");
        let _ = fs::remove_dir_all(&out);
    }
}

/// The run-time half for readers whose reflection compares more columns than the one the
/// statement names (a composite `primary_key:`/`foreign_key:` pair list, association_scope.rb
/// `last_chain_scope`: one equality per column pair, the value read from the owner), and for
/// chains called inside `Model.scoping { }` (Scoping::Named::ClassMethods#all clones the scope).
/// Real ActiveRecord on SQLite; FUTAMURA_VERIFY=1 compares every specialized result with Rails'
/// own chain (tests/fixtures/futamura_cpk_app, tests/futamura_cpk_runtime.rb).
mod composite_keys_and_scoping_blocks {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    #[test]
    fn composite_owner_keys_and_scoping_blocks_agree_with_rails_under_verify() {
        let have = Command::new("ruby").args(["-e", "require 'active_record'; require 'sqlite3'; require 'concurrent'"]).output();
        if !have.is_ok_and(|o| o.status.success()) {
            eprintln!("skipped: ruby with activerecord and sqlite3 is not available");
            return;
        }
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let out = crate::unique_dir("cpk");
        let _ = fs::remove_dir_all(&out);
        let emit = Command::new(env!("CARGO_BIN_EXE_roundhouse"))
            .args(["--target", "futamura", "-o"])
            .arg(&out)
            .arg(manifest.join("tests/fixtures/futamura_cpk_app"))
            .output()
            .unwrap();
        assert!(emit.status.success(), "{}", String::from_utf8_lossy(&emit.stderr));
        let probe = manifest.join("tests/futamura_cpk_runtime.rb");
        let output = |verify: &str| {
            let run = Command::new("ruby").arg(&probe).arg(&out).env("FUTAMURA_VERIFY", verify).output().unwrap();
            assert!(run.status.success(), "verify={verify}\n{}\n{}", String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&run.stderr));
            String::from_utf8_lossy(&run.stdout).to_string()
        };
        // Verify mode counts every call as a fallback: only its results are compared.
        let results = |s: &str| s.lines().map(|l| l.split(" spec=").next().unwrap().to_string()).collect::<Vec<_>>().join("\n");
        let verified = output("1");
        let normal = output("0");
        assert_eq!(results(&verified), results(&normal), "{verified}\n{normal}");
        let specialized = |name: &str| normal.lines().any(|l| l.starts_with(&format!("{name}=")) && l.ends_with(" spec=1 fall=0"));
        let fell_back = |name: &str| normal.lines().any(|l| l.starts_with(&format!("{name}=")) && l.ends_with(" spec=0 fall=1"));
        for name in ["lines", "lines_other", "gifts", "parcels", "by_sku", "of_order", "scoped_sku", "scoped_sku_other", "scoped_unscoped", "scoped_in", "scoped_assoc_reader_ids", "crates", "crates_other_shop", "lines_new_owner", "lines_keyed_new_owner", "parcels_new_owner"] {
            assert!(specialized(name), "{name}\n{normal}");
        }
        // A scope that orders is not equality conditions alone; an association reader does not
        // see the current scope (Association#scope) and is left to Rails; a nil key is IS NULL.
        for name in ["scoped_ordered", "scoped_reader", "lines_nil_region"] {
            assert!(fell_back(name), "{name}\n{normal}");
        }
        // The other key columns are conditions: the rows of another region or shop are not the order's.
        assert!(normal.contains("lines=[1, 2] ") && normal.contains("lines_other=[5] ") && normal.contains("gifts=[2] "), "{normal}");
        assert!(normal.contains("scoped_sku=[1] ") && normal.contains("scoped_unscoped=[4] "), "{normal}");
        // A default scope's equality on a column the composite key names too is replaced by the association's own
        // (WhereClause#merge): an order of shop 2 reads its shop-2 crates, not the default scope's shop 1.
        assert!(normal.contains("crates=[\"a\"] ") && normal.contains("crates_other_shop=[\"b\"] "), "{normal}");
        // A new owner without its key loads nothing (CollectionAssociation#null_scope?); with its key set it queries.
        assert!(normal.contains("lines_new_owner=[] ") && normal.contains("lines_keyed_new_owner=[10] ") && normal.contains("parcels_new_owner=[] "), "{normal}");
        let _ = fs::remove_dir_all(&out);
    }
}

/// The run-time half for Array binds (`where(column: ids)`): an IN list whose length is part of the
/// statement (PredicateBuilder::ArrayHandler, Arel HomogeneousIn), so one statement per length up to
/// MAX_ARRAY, its values serialized by the column's type as HomogeneousIn#casted_values does. Empty
/// lists, nil or Range elements, records, too long a list and values the type cannot serialize are
/// Rails' to run. Real ActiveRecord on SQLite; FUTAMURA_VERIFY=1 compares every specialized result
/// with Rails' own chain (tests/fixtures/futamura_arrays_app, tests/futamura_arrays_runtime.rb).
mod list_binds {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    #[test]
    fn array_binds_are_statements_per_length_and_agree_with_rails_under_verify() {
        let have = Command::new("ruby").args(["-e", "require 'active_record'; require 'sqlite3'; require 'concurrent'"]).output();
        if !have.is_ok_and(|o| o.status.success()) {
            eprintln!("skipped: ruby with activerecord and sqlite3 is not available");
            return;
        }
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let out = crate::unique_dir("arrays");
        let _ = fs::remove_dir_all(&out);
        let emit = Command::new(env!("CARGO_BIN_EXE_roundhouse"))
            .args(["--target", "futamura", "-o"])
            .arg(&out)
            .arg(manifest.join("tests/fixtures/futamura_arrays_app"))
            .output()
            .unwrap();
        assert!(emit.status.success(), "{}", String::from_utf8_lossy(&emit.stderr));
        let probe = manifest.join("tests/futamura_arrays_runtime.rb");
        let output = |verify: &str| {
            let run = Command::new("ruby").arg(&probe).arg(&out).env("FUTAMURA_VERIFY", verify).output().unwrap();
            assert!(run.status.success(), "verify={verify}\n{}\n{}", String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&run.stderr));
            String::from_utf8_lossy(&run.stdout).to_string()
        };
        // Verify mode counts every call as a fallback: only its results are compared.
        let results = |s: &str| s.lines().filter(|l| !l.starts_with("sprocket_reasons=")).map(|l| l.split(" spec=").next().unwrap().to_string()).collect::<Vec<_>>().join("\n");
        let verified = output("1");
        let normal = output("0");
        assert_eq!(results(&verified), results(&normal), "{verified}\n{normal}");
        let specialized = |name: &str| normal.lines().any(|l| l.starts_with(&format!("{name}=")) && l.ends_with(" spec=1 fall=0"));
        let fell_back = |name: &str| normal.lines().any(|l| l.starts_with(&format!("{name}=")) && l.ends_with(" spec=0 fall=1"));
        // A default scope that only preloads is names only: the statement preloads it before the chain's own preloads
        // (the association is loaded on every record), so those calls are served too.
        assert!(normal.contains("sprockets_calls spec=3 fall=0\nsprocket_reasons=[]"), "{normal}");
        assert!(normal.contains("sprockets=[2, 3] loaded=[[true, false], [true, false]]") && normal.contains("sprockets_nested=[2, 3] loaded=[[true, true], [true, true]]"), "{normal}");
        for name in [
            "ids1", "ids2", "ids3", "ids4", "ids_missing", "ids_dup", "ids_strings", "shop_ids", "codes", "codes_symbols", "ranks", "stamps", "not_ids",
            "not_ids1", "unscoped_ids", "first_of", "two_lists", "two_lists_mixed", "gadgets", "nil_and_list",
        ] {
            assert!(specialized(name), "{name}\n{normal}");
        }
        for name in [
            "empty", "nil_element", "nil_only", "range_element", "long", "records", "out_of_range", "out_of_range_only", "not_numeric", "set", "nested",
        ] {
            assert!(fell_back(name), "{name}\n{normal}");
        }
        assert!(normal.contains("ids3=[1, 4, 6] ") && normal.contains("ids_dup=[1, 2] ") && normal.contains("not_ids=[3, 4, 5, 6] "), "{normal}");
        // A value Rails drops from the list (out of the integer's range) is a list of another length: Rails' own answer.
        assert!(normal.contains("out_of_range=[1, 2] ") && normal.contains("not_numeric=[1] "), "{normal}");
        assert!(normal.contains("reasons=[\"bind_array[element]\", \"bind_array[empty]\", \"bind_array[long]\", \"bind_array[nil]\"]"), "{normal}");
        let _ = fs::remove_dir_all(&out);
    }
}

/// FUTAMURA_VERIFY=1 runs Rails' own chain beside the specialized statement and compares the two.
/// The comparison is not the application's: a `sql.active_record` subscriber (a test that counts
/// the statements of one kind) must see the same events as without verify, inside a query cache and
/// inside a transaction, and a preload through a polymorphic association must be compared the way
/// Preloader::Branch#grouped_records loads it (records of a class without the next association are
/// skipped). Real ActiveRecord on SQLite (`tests/futamura_verify_runtime.rb`).
mod verify_runtime {
    use std::process::Command;

    fn probe(verify: &str) -> Option<String> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let runtime = root.join("src/emit/futamura/futamura.rb");
        let probe = root.join("tests/futamura_verify_runtime.rb");
        let have = Command::new("ruby").args(["-e", "require 'active_record'; require 'sqlite3'; require 'concurrent'"]).output();
        if !have.is_ok_and(|o| o.status.success()) {
            eprintln!("skipped: ruby with activerecord and sqlite3 is not available");
            return None;
        }
        let run = Command::new("ruby").arg(&probe).arg(&runtime).env("FUTAMURA_VERIFY", verify).output().unwrap();
        assert!(run.status.success(), "verify={verify}\n{}\n{}", String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&run.stderr));
        Some(String::from_utf8_lossy(&run.stdout).to_string())
    }

    #[test]
    fn a_sql_subscriber_sees_the_same_statements_with_and_without_verify() {
        let (Some(verified), Some(normal)) = (probe("1"), probe("0")) else { return };
        // Everything but the fallback count (verify runs Rails' chain at every call).
        let events = |s: &str| s.lines().filter(|l| !l.starts_with("fallbacks=")).collect::<Vec<_>>().join("\n");
        assert_eq!(events(&verified), events(&normal), "{verified}\n{normal}");
        assert!(normal.contains("notes=[[1], {\"Note Load\" => 1}]\n"), "{normal}");
        assert!(normal.contains("notes_in_transaction=[[2], {\"Note Load\" => 1}]\n"), "{normal}");
        assert!(normal.contains("fallbacks=0\n"), "{normal}");
        // The shadow ran: it did not skip Rails' chain, and it left the instrumenter slot as it found it.
        assert!(!verified.contains("fallbacks=0\n"), "{verified}");
        assert!(verified.contains("slot_restored=true\n"), "{verified}");
        assert!(verified.contains("silent_missing=[]\n"), "{verified}");
    }

    #[test]
    fn a_preload_through_a_polymorphic_association_skips_records_without_the_reflection() {
        let Some(verified) = probe("1") else { return };
        assert!(verified.contains("regions=[[\"Country\", 1], [\"Province\", 1]]\n"), "{verified}");
    }
}

/// A preload entry whose target has a default scope is planned (the loader's scope is built once
/// per association otherwise), but the scope is read from the class at every call, as
/// Preloader::Association#build_scope does (`klass.scope_for_association`): a default scope that
/// reads state changing between calls (a tenant), returns nil, or adds a `NOT` and an order loads
/// what Rails loads. An entry Rails keeps (`through:`) is counted by reason under FUTAMURA_STATS.
/// Real ActiveRecord on SQLite (`tests/futamura_preload_runtime.rb`); under FUTAMURA_VERIFY=1 each
/// specialized preload is compared to Rails' own Preloader.
mod preload_runtime {
    use std::process::Command;

    fn probe(verify: &str, stats: Option<&std::path::Path>) -> Option<String> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let runtime = root.join("src/emit/futamura/futamura.rb");
        let probe = root.join("tests/futamura_preload_runtime.rb");
        let have = Command::new("ruby").args(["-e", "require 'active_record'; require 'sqlite3'; require 'concurrent'"]).output();
        if !have.is_ok_and(|o| o.status.success()) {
            eprintln!("skipped: ruby with activerecord and sqlite3 is not available");
            return None;
        }
        let mut run = Command::new("ruby");
        run.arg(&probe).arg(&runtime).env("FUTAMURA_VERIFY", verify);
        if let Some(dir) = stats {
            run.env("FUTAMURA_STATS", dir);
        }
        let run = run.output().unwrap();
        assert!(run.status.success(), "verify={verify}\n{}\n{}", String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&run.stderr));
        Some(String::from_utf8_lossy(&run.stdout).to_string())
    }

    #[test]
    fn a_default_scoped_target_is_planned_and_its_scope_is_read_at_every_call() {
        let Some(normal) = probe("0", None) else { return };
        assert!(normal.contains("books[tenant=1]=[\"b0\"] queries=1 tenant_in_sql=true\n"), "{normal}");
        assert!(normal.contains("books[tenant=2]=[\"b2\"] queries=1 tenant_in_sql=true\n"), "{normal}");
        assert!(normal.contains("books[tenant=nil]=[\"b0\", \"b1\", \"b2\", \"b3\"] queries=1 tenant_in_sql=false\n"), "{normal}");
        // A `scoping` block does not change what the Preloader builds from the class's default scope.
        assert!(normal.contains("scoping=[\"b2\"]\n"), "{normal}");
        assert!(normal.contains("nested=[[\"b0\", [\"x\"]]]\n"), "{normal}");
        // Planned (`true`: the node asks the class again at each call), not handed to Rails whole.
        assert!(normal.contains("[:books, \"Author\", [[:books, true, []]]]"), "{normal}");
        assert!(normal.contains("[{books: :tags}, \"Author\", [[:books, true, [:tags]]]]"), "{normal}");
    }

    #[test]
    fn the_planned_preloads_agree_with_rails_under_verify() {
        // Verify raises on a difference, so the probe passing is the check.
        let Some(verified) = probe("1", None) else { return };
        assert!(verified.contains("tags=[[1, \"b0\"], [2, nil], [3, nil], [4, nil]]\n"), "{verified}");
    }

    #[test]
    fn the_entries_rails_keeps_are_counted_by_reason_only_under_stats() {
        let dir = crate::unique_dir("preload-stats");
        std::fs::create_dir_all(&dir).unwrap();
        let counted = probe("0", Some(&dir));
        let off = probe("0", None);
        let _ = std::fs::remove_dir_all(&dir);
        let (Some(counted), Some(off)) = (counted, off) else { return };
        assert!(counted.contains("refusals={\"through\" => 1}\n"), "{counted}");
        assert!(!off.contains("refusals="), "{off}");
    }
}

/// Preloads whose reflections compare several columns (array `primary_key:`/`foreign_key:`):
/// Preloader::Association#derive_key reads one value per column and skips an owner with a nil in
/// any of them; LoaderQuery#load_records_for_keys builds `where(column => values)` per column
/// (an IN list each). The runtime plans them and runs the same queries; a list against a list of
/// another length is Rails' (counted as `composite_key`). Real ActiveRecord on SQLite
/// (`tests/futamura_cpk_preload_runtime.rb`, over tests/fixtures/futamura_cpk_app).
mod composite_key_preload {
    use std::process::Command;

    fn probe(verify: &str, stats: Option<&std::path::Path>) -> Option<String> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let runtime = root.join("src/emit/futamura/futamura.rb");
        let probe = root.join("tests/futamura_cpk_preload_runtime.rb");
        let have = Command::new("ruby").args(["-e", "require 'active_record'; require 'sqlite3'; require 'concurrent'"]).output();
        if !have.is_ok_and(|o| o.status.success()) {
            eprintln!("skipped: ruby with activerecord and sqlite3 is not available");
            return None;
        }
        let mut run = Command::new("ruby");
        run.arg(&probe).arg(&runtime).env("FUTAMURA_VERIFY", verify);
        if let Some(dir) = stats {
            run.env("FUTAMURA_STATS", dir);
        }
        let run = run.output().unwrap();
        assert!(run.status.success(), "verify={verify}\n{}\n{}", String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&run.stderr));
        Some(String::from_utf8_lossy(&run.stdout).to_string())
    }

    #[test]
    fn composite_preloads_load_what_rails_loads_with_the_same_queries() {
        let dir = std::env::temp_dir().join(format!("futamura-cpk-preload-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let normal = probe("0", Some(&dir));
        let verified = probe("1", None);
        let _ = std::fs::remove_dir_all(&dir);
        let (Some(normal), Some(verified)) = (normal, verified) else { return };
        for out in [&normal, &verified] {
            for name in ["lines", "gifts", "crates", "lines_order", "parcels"] {
                assert!(out.contains(&format!("{name}=same queries=1/1 sql=true\n")), "{name}\n{out}");
            }
            assert!(out.contains("nested=same queries=2/2 sql=true\n"), "{out}");
        }
        // A nil in one key column leaves the owner without a target; the other regions' rows are not its.
        assert!(normal.contains("lines_targets=[[1, [[1, 2]]], [2, [[5]]], [3, [[]]]]\n"), "{normal}");
        // One IN list per key column, not a tuple condition.
        assert!(normal.contains("\"lines\".\"region_id\" IN (?, ?) AND \"lines\".\"order_id\" IN (?, ?)"), "{normal}");
        // Planned, not handed to Rails: only the lists of different lengths are refused.
        assert!(normal.contains("[:lines, \"Order\", [[:lines, []]]]") && normal.contains("[:order, \"Line\", [[:order, []]]]"), "{normal}");
        assert!(normal.contains("[:parcels, \"Order\", \"composite_key\"]"), "{normal}");
        assert!(normal.contains("refusals={\"composite_key\" => 1}\n"), "{normal}");
    }
}

/// `where(column: Klass)`: a Class has no `id`, so the predicate builder builds `column = <bind>`
/// with it as the value (BasicObjectHandler) and the adapter inlines `Class#to_s` (no prepared
/// statements) or raises TypeError (prepared statements); a Class that answers `id` is read as the
/// key; a Relation is a subquery. The probe (`tests/futamura_class_bind_runtime.rb`) runs real
/// ActiveRecord on SQLite; each field is `<record id>/<calls that ran Rails' chain>`.
mod class_bind_runtime {
    use std::process::Command;

    fn probe(verify: &str, prepared: &str) -> Option<String> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let runtime = root.join("src/emit/futamura/futamura.rb");
        let probe = root.join("tests/futamura_class_bind_runtime.rb");
        let have = Command::new("ruby").args(["-e", "require 'active_record'; require 'sqlite3'; require 'concurrent'"]).output();
        if !have.is_ok_and(|o| o.status.success()) {
            eprintln!("skipped: ruby with activerecord and sqlite3 is not available");
            return None;
        }
        let run = Command::new("ruby").arg(&probe).arg(&runtime).env("FUTAMURA_VERIFY", verify).env("PREPARED", prepared).output().unwrap();
        assert!(run.status.success(), "verify={verify}\n{}\n{}", String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&run.stderr));
        Some(String::from_utf8_lossy(&run.stdout).to_string())
    }

    #[test]
    fn a_class_value_is_bound_as_the_class_name_rails_inlines() {
        let Some(normal) = probe("0", "0") else { return };
        // STI and polymorphic type columns alike: one placeholder, no fallback, the record Rails finds.
        assert!(normal.contains("sti=subclass:1/0 namespaced:3/0 base:nil/0 string:1/0\n"), "{normal}");
        assert!(normal.contains("polymorphic=class:2/0 base:1/0\n"), "{normal}");
    }

    #[test]
    fn a_class_that_answers_id_and_a_relation_run_rails_chain() {
        let Some(normal) = probe("0", "0") else { return };
        assert!(normal.contains("key=keyed_class:2/1\n"), "{normal}");
        assert!(normal.contains("relation=subquery:[1, 3]/1\n"), "{normal}");
    }

    #[test]
    fn with_prepared_statements_the_statement_raises_what_rails_raises() {
        let Some(normal) = probe("0", "1") else { return };
        assert!(normal.contains("sti=subclass:TypeError/0 namespaced:TypeError/0 base:TypeError/0 string:1/0\n"), "{normal}");
    }

    #[test]
    fn the_same_calls_agree_with_rails_under_verify() {
        // Verify raises on a difference, so the probe passing is the check.
        for prepared in ["0", "1"] {
            let Some(verified) = probe("1", prepared) else { return };
            assert!(verified.contains("key=keyed_class:2/1\n"), "{verified}");
        }
    }
}

/// `to_a` on a collection reader is CollectionProxy#records, which loads the association's target
/// (CollectionAssociation#load_target: `@target = merge_target_lists(find_target, target); loaded!`),
/// so a later read of the reader is served from memory. The specialized terminal makes the same
/// assignment (Association#target=, as Preloader::Association#associate_records_to_owner does);
/// `first`/`take`, or a link before `to_a`, query without loading it. The probe
/// (`tests/futamura_load_runtime.rb`) compares each scenario with Rails' own reader on real
/// ActiveRecord (SQLite): results, the loaded flag, the target, inverses, strict loading and the
/// number of queries.
mod loaded_targets {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/futamura_load_app")
    }

    fn emit(out: &Path) -> String {
        let _ = fs::remove_dir_all(out);
        let emit = Command::new(env!("CARGO_BIN_EXE_roundhouse")).args(["--target", "futamura", "-o"]).arg(out).arg(fixture()).output().unwrap();
        assert!(emit.status.success(), "{}", String::from_utf8_lossy(&emit.stderr));
        fs::read_to_string(out.join("config/initializers/futamura.rb")).unwrap()
    }

    #[test]
    fn only_to_a_on_the_reader_itself_is_marked_as_loading_the_target() {
        let out = crate::unique_dir("loads");
        let init = emit(&out);
        let define = |key: &str| init.lines().find(|l| l.contains(&format!("Futamura.define(:{key},"))).unwrap_or_else(|| panic!("{key}\n{init}")).to_string();
        // `room.messages.to_a`, and with the scope lambda the reader carries.
        assert!(define("services_finder_4_5").contains(", loads: true)"), "{init}");
        assert!(define("services_finder_9_5").contains(", loads: true)"), "{init}");
        // `first` answers from a query while the target is not loaded; a link makes a Relation.
        assert!(!define("services_finder_14_5").contains("loads:"), "{init}");
        assert!(!define("services_finder_19_5").contains("loads:"), "{init}");
        let _ = fs::remove_dir_all(&out);
    }

    #[test]
    fn a_specialized_to_a_loads_the_target_as_rails_does() {
        let have = Command::new("ruby").args(["-e", "require 'active_record'; require 'sqlite3'; require 'concurrent'"]).output();
        if !have.is_ok_and(|o| o.status.success()) {
            eprintln!("skipped: ruby with activerecord and sqlite3 is not available");
            return;
        }
        let out = crate::unique_dir("loads-run");
        emit(&out);
        let probe = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/futamura_load_runtime.rb");
        let output = |verify: &str| {
            let run = Command::new("ruby").arg(&probe).arg(&out).env("FUTAMURA_VERIFY", verify).output().unwrap();
            assert!(run.status.success(), "verify={verify}\n{}\n{}", String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&run.stderr));
            String::from_utf8_lossy(&run.stdout).to_string()
        };
        let verified = output("1");
        let normal = output("0");
        for run in [&verified, &normal] {
            assert!(!run.contains("same=false"), "{run}");
            assert_eq!(run.lines().count(), 10, "{run}");
        }
        // Verify mode runs Rails' chain at every call: the state and the queries are the same, the counts of
        // specialized calls are not.
        let state = |s: &str| s.lines().map(|l| l.split(" spec=").next().unwrap().to_string()).collect::<Vec<_>>().join("\n");
        assert_eq!(state(&verified), state(&normal), "{verified}\n{normal}");
        // The first read loads the target (one query), later reads are Rails' reads of it.
        assert!(normal.contains("twice same=true [[1, 2, 3], true, [1, 2, 3], true, true] extra=nil queries=1 spec=1 fall=1"), "{normal}");
        assert!(normal.contains("to_a_then_reads same=true [[3, true, 1, 3], true, [1, 2, 3], true, true] extra=nil queries=1 spec=1 fall=0"), "{normal}");
        assert!(normal.contains("first_does_not_load same=true [[1], false, nil, nil, nil] extra=nil queries=1"), "{normal}");
        assert!(normal.contains("relation_does_not_load same=true [[1, 2, 3], false, nil, nil, nil] extra=nil queries=1 spec=1"), "{normal}");
        assert!(normal.contains("scope_lambda_loads same=true [[3, 2, 1], true, [3, 2, 1], true, true]"), "{normal}");
        // Unsaved records, a new owner and a strict-loading owner are Rails' reads.
        assert!(normal.contains("new_record_before_load same=true [[1, 2, 3, nil], true, [1, 2, 3, nil], true, true] extra=nil queries=1 spec=0 fall=1"), "{normal}");
        assert!(normal.contains("strict_loading same=true [[\"ActiveRecord::StrictLoadingViolationError\"], false, nil, nil, nil]"), "{normal}");
        assert!(normal.contains("extra=[true, true, true] queries=1"), "{normal}");
        let _ = fs::remove_dir_all(&out);
    }
}

/// `first`/`last`/`take` on a collection reader `load_target` when `find_from_target?` holds
/// (collection_association.rb: a loaded or new-owner proxy, unsaved or changed target records, an
/// owner that is `strict_loading_all?`, a reflection with `strict_loading: true`): the read then
/// raises `StrictLoadingViolationError` (Association#find_target) or answers from the target it
/// sets. The specialized terminal falls back to Rails' read in each of those. The probe
/// (`tests/futamura_strict_runtime.rb`) runs every terminal the specializer models in every
/// strict-loading mode and target state on real ActiveRecord (SQLite), through the emitted Finder
/// and through Rails' own reader on a fresh owner: results or error, loaded flag, target, record
/// strictness and the number of queries must be the same.
mod strict_loading_terminals {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/futamura_load_app")
    }

    #[test]
    fn terminals_on_a_reader_read_as_rails_does_under_every_strict_loading_mode() {
        let have = Command::new("ruby").args(["-e", "require 'active_record'; require 'sqlite3'; require 'concurrent'"]).output();
        if !have.is_ok_and(|o| o.status.success()) {
            eprintln!("skipped: ruby with activerecord and sqlite3 is not available");
            return;
        }
        let out = crate::unique_dir("strict-run");
        let _ = fs::remove_dir_all(&out);
        let emit = Command::new(env!("CARGO_BIN_EXE_roundhouse")).args(["--target", "futamura", "-o"]).arg(&out).arg(fixture()).output().unwrap();
        assert!(emit.status.success(), "{}", String::from_utf8_lossy(&emit.stderr));
        let probe = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/futamura_strict_runtime.rb");
        let output = |verify: &str| {
            let run = Command::new("ruby").arg(&probe).arg(&out).env("FUTAMURA_VERIFY", verify).output().unwrap();
            assert!(run.status.success(), "verify={verify}\n{}\n{}", String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&run.stderr));
            String::from_utf8_lossy(&run.stdout).to_string()
        };
        let verified = output("1");
        let normal = output("0");
        for run in [&verified, &normal] {
            assert!(!run.contains("same=false"), "{run}");
            assert_eq!(run.lines().count(), 202, "{run}");
        }
        let state = |s: &str| s.lines().map(|l| l.split(" spec=").next().unwrap().to_string()).collect::<Vec<_>>().join("\n");
        assert_eq!(state(&verified), state(&normal), "{verified}\n{normal}");
        let raised = "[[\"ActiveRecord::StrictLoadingViolationError\"], false, nil, nil] queries=0 spec=0";
        // An unloaded target is a query, specialized, when nothing makes the proxy read its target.
        assert!(normal.contains("first/plain/unloaded same=true [[1], false, nil, [false]] queries=1 spec=1"), "{normal}");
        assert!(normal.contains("first/strict_n_plus_one/unloaded same=true [[1], false, nil, [true]] queries=1 spec=1"), "{normal}");
        // `strict_loading_all?` owners and `strict_loading: true` reflections load the target: Rails' read.
        for terminal in ["first", "last", "take", "first_n", "last_n"] {
            assert!(normal.contains(&format!("{terminal}/strict_all/unloaded same=true [[\"ActiveRecord::StrictLoadingViolationError\"], false, nil, nil] queries=0 spec=0")), "{terminal}\n{normal}");
        }
        for terminal in ["reflection_first", "reflection_last", "reflection_take"] {
            assert!(normal.contains(&format!("{terminal}/plain/unloaded same=true {raised}")), "{terminal}\n{normal}");
        }
        assert!(normal.contains(&format!("first/by_default same=true {raised}")), "{normal}");
        // A link after the reader is a Relation query, not a read of the target.
        assert!(normal.contains("find_by/strict_all/unloaded same=true [[2], false, nil, [false]] queries=1 spec=1"), "{normal}");
        assert!(normal.contains("linked_first/strict_all/unloaded same=true [[1], false, nil, [false]] queries=1 spec=1"), "{normal}");
        let _ = fs::remove_dir_all(&out);
    }
}

/// FUTAMURA_STATS counts the queries Futamura did not serve by the innermost app frame that issued them
/// (`path:line` under the app root), so the residue Rails runs can be ranked by how often it runs. Frames in Rails and
/// under vendor/ are skipped, a chain over several lines is counted at the line of its terminal, a call that fell
/// back is counted where it was written, and a statement the runtime served is only totalled. Real ActiveRecord on
/// SQLite (`tests/futamura_resweight_runtime.rb`).
mod resweight_runtime {
    use std::process::Command;

    #[test]
    fn queries_rails_ran_are_counted_by_app_call_site() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let runtime = root.join("src/emit/futamura/futamura.rb");
        let probe = root.join("tests/futamura_resweight_runtime.rb");
        let have = Command::new("ruby").args(["-e", "require 'active_record'; require 'sqlite3'; require 'concurrent'"]).output();
        if !have.is_ok_and(|o| o.status.success()) {
            eprintln!("skipped: ruby with activerecord and sqlite3 is not available");
            return;
        }
        let run = Command::new("ruby").arg(&probe).arg(&runtime).output().unwrap();
        let out = String::from_utf8_lossy(&run.stdout).to_string();
        assert!(run.status.success(), "{out}\n{}", String::from_utf8_lossy(&run.stderr));
        assert!(out.starts_with("served=2\n"), "{out}");
        // Written on one line, over several (the terminal's line), count, exists?, find_by.
        assert!(out.contains("app/finders.rb:5 calls=5 cached=1 names={\"Thing Load\" => 5}\n"), "{out}");
        assert!(out.contains("app/finders.rb:10 calls=2 cached=0 names={\"Thing Load\" => 2}\n"), "{out}");
        assert!(out.contains("app/finders.rb:12 calls=1 cached=0 names={\"Thing Count\" => 1}\n"), "{out}");
        assert!(out.contains("app/finders.rb:13 calls=1 cached=0 names={\"Thing Exists?\" => 1}\n"), "{out}");
        assert!(out.contains("app/finders.rb:14 calls=1 cached=0 names={\"Thing Load\" => 1}\n"), "{out}");
        // Through a vendored helper: the app line that called it. A fallback: where the call is written.
        assert!(out.contains("app/finders.rb:15 calls=1 cached=0"), "{out}");
        assert!(out.contains("app/finders.rb:4 calls=1 cached=0"), "{out}");
        assert!(!out.contains("vendor/") && !out.contains("(no app frame)"), "{out}");
        assert!(out.contains("dump_root=true dump_sites=true dump_served=2\n"), "{out}");
    }
}

/// A `where` value that is a call (not a plain read) is evaluated once, where Ruby evaluates the
/// hash's values (while it builds the argument list, left to right, before `where` runs), and the
/// fallback reads the result back. The probe (`tests/futamura_hoist_runtime.rb`) counts evaluations
/// on real ActiveRecord, with and without `FUTAMURA_VERIFY=1`.
mod hoisted_values {
    use super::{Command, fs, scratch, write};

    const LOOKUP: &str = "\
module Lookup
  def self.calls = (@calls ||= [])

  def self.tick(value)
    calls << value
    value.to_s.downcase
  end

  def self.by_kind(kind)
    Widget.where(shop_id: 1, kind: tick(kind)).order(:id).to_a
  end

  def self.ordered(name, kind)
    Widget.where(name: tick(name), kind: tick(kind)).to_a
  end

  def self.listed(names)
    Widget.where(shop_id: 1, name: names.map { |n| tick(n) }).to_a
  end

  def self.listed_with_block(pairs)
    Widget.where(shop_id: 1, name: pairs.map(&:last).each { |n| tick(n) }).to_a
  end

  def self.first_named(name)
    Widget.find_by(name: tick(name))
  end

  def self.stamp
    calls << :stamp
    1
  end

  def self.mixed
    Widget.where(shop_id: stamp, kind: tick(\"K\")).to_a
  end

  # A zero-argument call on the implicit self answers differently each time and records it.
  def self.bump
    calls << :bump
    @bump = (@bump || 0) + 1
  end

  def self.kind_name
    calls << :kind_name
    \"k\"
  end

  def self.shop_id
    calls << :shop_id
    1
  end

  def self.per_page
    calls << :per_page
    2
  end

  def self.by_bump
    Widget.where(shop_id: bump).order(:id).to_a
  end

  def self.by_shorthand
    Widget.where(shop_id:).order(:id).to_a
  end

  def self.paged
    Widget.where(shop_id: 1).order(:id).limit(per_page).to_a
  end

  def self.with_scope
    Widget.of_kind(kind_name).order(:id).to_a
  end

  def self.with_scope_twice
    Widget.not_kind_named(kind_name).order(:id).to_a
  end

  def self.with_scope_unused
    Widget.any_kind(kind_name).order(:id).to_a
  end

  # The relation is built here, and a value changes before it is loaded: Rails kept the first.
  def self.captured
    scope = Widget.where(shop_id: bump).order(:id)
    bump
    scope.to_a
  end

  def self.captured_shorthand
    scope = Widget.where(shop_id:).order(:id)
    scope.to_a
  end

  def self.captured_in_block
    scope = Widget.where(shop_id: bump).order(:id)
    bump
    [1, 2].map { scope.to_a.map(&:name) }
  end
end
";

    fn emit(lookup: &str) -> (std::path::PathBuf, String) {
        let src = scratch("hoist-src");
        let out = scratch("hoist-out");
        write(&src, "db/schema.rb", b"ActiveRecord::Schema.define do\n  create_table \"widgets\", force: :cascade do |t|\n    t.string \"name\"\n    t.string \"kind\"\n    t.integer \"shop_id\"\n  end\nend\n");
        write(&src, "app/models/application_record.rb", b"class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n");
        write(&src, "app/models/widget.rb", b"class Widget < ApplicationRecord\n  scope :of_kind, ->(kind) { where(kind: kind) }\n  scope :not_kind_named, ->(kind) { where(kind: kind).where.not(name: kind) }\n  scope :any_kind, ->(kind) { where(shop_id: 1) }\nend\n");
        write(&src, "app/services/lookup.rb", lookup.as_bytes());
        let run = Command::new(env!("CARGO_BIN_EXE_roundhouse")).args(["--target", "futamura", "-o"]).arg(&out).arg(&src).output().unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        let finder = fs::read_to_string(out.join("app/services/lookup.rb")).unwrap();
        let _ = fs::remove_dir_all(&src);
        (out, finder)
    }

    #[test]
    fn a_call_value_is_evaluated_once_in_place_among_the_binds_and_the_fallback_reads_it_back() {
        let (out, lookup) = emit(LOOKUP);
        // The scope with an unused parameter keeps its site Rails': Ruby evaluates the argument, the binds would not.
        assert_eq!(lookup.matches("Futamura.records(").count(), 14, "{lookup}");
        // In argument order, parenthesized so any expression is valid in the array.
        assert!(lookup.contains("Futamura.records(:services_lookup_14_5, [(__fut_services_lookup_14_5_0 = (tick(name))), (__fut_services_lookup_14_5_1 = (tick(kind)))]) { Widget.where(name: __fut_services_lookup_14_5_0, kind: __fut_services_lookup_14_5_1).to_a }"), "{lookup}");
        // A pure read beside a call keeps its own text; only the call is read back.
        assert!(lookup.contains("Futamura.records(:services_lookup_10_5, [(__fut_services_lookup_10_5_0 = (tick(kind)))]) { Widget.where(shop_id: 1, kind: __fut_services_lookup_10_5_0).order(:id).to_a }"), "{lookup}");
        assert!(!lookup.contains("{ Widget.where(shop_id: 1, kind: tick(kind))"), "{lookup}");
        // A zero-argument call on the implicit self is read back too, in its place among the binds.
        assert!(lookup.contains("Futamura.records(:services_lookup_35_5, [(__fut_services_lookup_35_5_0 = (stamp)), (__fut_services_lookup_35_5_1 = (tick(\"K\")))])"), "{lookup}");
        // A value-omitted entry reads back as `key: temp`; a limit is read back like a where value.
        assert!(lookup.contains("{ Widget.where(shop_id: __fut_services_lookup_64_5_0).order(:id).to_a }"), "{lookup}");
        assert!(lookup.contains("{ Widget.where(shop_id: 1).order(:id).limit(__fut_services_lookup_68_5_0).to_a }"), "{lookup}");
        // A scope parameter used twice is evaluated at its first bind and read at the second.
        assert!(lookup.contains("[(__fut_services_lookup_76_5_0 = (kind_name)), __fut_services_lookup_76_5_0]"), "{lookup}");
        // A value of a relation local is kept by the local's assignment, and the site binds the kept value.
        assert!(lookup.contains("scope = Widget.where(shop_id: (__fut_cap_services_lookup_85_35 = (bump))).order(:id)"), "{lookup}");
        assert!(lookup.contains("scope = Widget.where(shop_id: (__fut_cap_services_lookup_91_26 = (shop_id))).order(:id)"), "{lookup}");
        let _ = fs::remove_dir_all(&out);
    }

    #[test]
    fn a_call_value_through_a_local_is_kept_by_the_assignment() {
        let src = "module Lookup\n  def self.late(kind)\n    scope = Widget.where(kind: tick(kind))\n    scope.to_a\n  end\nend\n";
        let (out, lookup) = emit(src);
        assert_eq!(lookup.matches("Futamura.records(").count(), 1, "{lookup}");
        assert!(lookup.contains("scope = Widget.where(kind: (__fut_cap_services_lookup_3_32 = (tick(kind))))"), "{lookup}");
        assert!(lookup.contains("[__fut_cap_services_lookup_3_32]) { scope.to_a }"), "{lookup}");
        let _ = fs::remove_dir_all(&out);
    }

    #[test]
    fn a_kept_value_is_spliced_only_with_the_site_that_reads_it() {
        // The assignment of `scope` sits inside the first site's span (its hoisted value): the kept
        // value cannot be spliced, so the second site, which would read it, stays Rails' too.
        let src = "module Lookup\n  def self.inner\n    Widget.where(kind: (scope = Widget.where(shop_id: bump); bump)).to_a\n    scope.to_a\n  end\nend\n";
        let (out, lookup) = emit(src);
        assert_eq!(lookup.matches("Futamura.records(").count(), 1, "{lookup}");
        assert!(!lookup.contains("__fut_cap_"), "{lookup}");
        assert!(lookup.contains("\n    scope.to_a\n"), "{lookup}");
        // Two sites over one local share the one kept value.
        let src = "module Lookup\n  def self.two\n    scope = Widget.where(shop_id: bump)\n    bump\n    [scope.to_a, scope.first]\n  end\nend\n";
        let (out2, lookup) = emit(src);
        assert_eq!(lookup.matches("(__fut_cap_services_lookup_3_35 = (bump))").count(), 1, "{lookup}");
        assert_eq!(lookup.matches("[__fut_cap_services_lookup_3_35]) {").count(), 2, "{lookup}");
        let _ = fs::remove_dir_all(&out);
        let _ = fs::remove_dir_all(&out2);
    }

    #[test]
    fn the_emitted_lookup_evaluates_each_value_once_with_and_without_verify() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let have = Command::new("ruby").args(["-e", "require 'active_record'; require 'sqlite3'; require 'concurrent'"]).output();
        if !have.is_ok_and(|o| o.status.success()) {
            eprintln!("skipped: ruby with activerecord and sqlite3 is not available");
            return;
        }
        let (out, _) = emit(LOOKUP);
        let probe = root.join("tests/futamura_hoist_runtime.rb");
        let output = |verify: &str| {
            let run = Command::new("ruby").arg(&probe).arg(&out).env("FUTAMURA_VERIFY", verify).output().unwrap();
            assert!(run.status.success(), "verify={verify}\n{}\n{}", String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&run.stderr));
            String::from_utf8_lossy(&run.stdout).to_string()
        };
        let verified = output("1");
        let normal = output("0");
        let results = |s: &str| s.lines().filter(|l| !l.starts_with("specialized=")).collect::<Vec<_>>().join("\n");
        assert_eq!(results(&verified), results(&normal));
        // Each value once per call, in the order the hash lists them.
        assert!(normal.contains("by_kind=[\"a\", \"b\", \"c\"] calls=[\"K\"]\n"), "{normal}");
        assert!(normal.contains("ordered=[\"a\"] calls=[\"A\", \"K\"]\n"), "{normal}");
        assert!(normal.contains("listed=[\"a\", \"c\"] calls=[\"a\", \"c\"]\n"), "{normal}");
        assert!(normal.contains("first_named=[\"b\"] calls=[\"B\"]\n"), "{normal}");
        assert!(normal.contains("nil_value=[] calls=[nil]\n"), "{normal}");
        assert!(normal.contains("redefined_where=[\"a\", \"b\", \"c\"] calls=[\"K\"]\n"), "{normal}");
        // A zero-argument call with effects runs once per call, in a normal, a verify and a fallback run.
        assert!(normal.contains("by_bump=[\"a\", \"b\", \"c\", \"d\"] calls=[:bump]\n"), "{normal}");
        assert!(normal.contains("by_shorthand=[\"a\", \"b\", \"c\", \"d\"] calls=[:shop_id]\n"), "{normal}");
        assert!(normal.contains("paged=[\"a\", \"b\"] calls=[:per_page]\n"), "{normal}");
        assert!(normal.contains("with_scope=[\"a\", \"b\", \"c\", \"e\"] calls=[:kind_name]\n"), "{normal}");
        assert!(normal.contains("with_scope_twice=[\"a\", \"b\", \"c\", \"e\"] calls=[:kind_name]\n"), "{normal}");
        assert!(normal.contains("with_scope_unused=[\"a\", \"b\", \"c\", \"d\"] calls=[:kind_name]\n"), "{normal}");
        // The relation was built before the second `bump`: it holds the first value, and each call ran once.
        assert!(normal.contains("captured=[\"a\", \"b\", \"c\", \"d\"] calls=[:bump, :bump]\n"), "{normal}");
        assert!(normal.contains("captured_shorthand=[\"a\", \"b\", \"c\", \"d\"] calls=[:shop_id]\n"), "{normal}");
        assert!(normal.contains("captured_in_block=[[\"a\", \"b\", \"c\", \"d\"], [\"a\", \"b\", \"c\", \"d\"]] calls=[:bump, :bump]\n"), "{normal}");
        assert!(normal.contains("fallback_by_bump=[\"a\", \"b\", \"c\", \"d\"] calls=[:bump]\n"), "{normal}");
        assert!(normal.contains("fallback_captured=[\"a\", \"b\", \"c\", \"d\"] calls=[:bump, :bump]\n"), "{normal}");
        let _ = fs::remove_dir_all(&out);
    }
}
