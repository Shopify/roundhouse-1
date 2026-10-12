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

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("roundhouse-futamura-{name}-{}", std::process::id()));
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
    let run = Command::new(root.join("scripts/futamura-suite"))
        .arg(&fixture)
        .env("ROUNDHOUSE_BIN", env!("CARGO_BIN_EXE_roundhouse"))
        .env_remove("BUNDLE_GEMFILE")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        run.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    // The fixture's suite is green, so SAME must mean zero failures.
    assert!(stdout.contains(" 0 failures, 0 errors"), "{stdout}");
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
        assert!(controller.contains(
            "@messages = Futamura.records(:controllers_messages_controller_4_17, -> { [@room.id] }, owner: @room, \
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
             { |p| Message.all.where(room_id: p.bind).with_creator.ordered.send(:ordered_relation).limit(Message::PAGE_SIZE).reverse_order }"
        ), "{init}");
        assert!(report.specialized.iter().any(|s| s.ends_with("messages_controller.rb:4:17")), "{:?}", report.specialized);
    }

    #[test]
    fn constant_rooted_chain_binds_request_values_and_bakes_literals() {
        let (app, mut files) = app_and_files();
        specialize(&app, &mut files).unwrap();
        let controller = file(&files, "app/controllers/messages_controller.rb");
        let init = file(&files, INITIALIZER);
        assert!(controller.contains("@pinned = Futamura.records(:controllers_messages_controller_5_15, [@room.id]) {"), "{controller}");
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
        assert!(out.contains("[params[:id].to_s]) { Story.where(short_id: params[:id].to_s).first }"), "{out}");
        assert!(out.contains("[Futamura.find_by_value(params[:id] || params[:story_id])]) {"), "{out}");
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
        // and a call it cannot serve ran Rails' chain (nil, a join/order,
        // a scoping block).
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
        // A class method the class defines itself (a double) is not answered beside: Rails runs it.
        assert!(normal.contains("via=[1, :double, 1, :prepended]") && normal.contains("calls_via=2"), "{normal}");
        assert!(normal.contains("[:item, :fallback] [:item, :fallback] [:ranked, :fallback] [:ranked, :fallback] [:legacy, :fallback]"), "{normal}");
    }

    /// The FUTAMURA_STATS block names why a call ran Rails' chain. A nil bind
    /// (which a StatementCache refuses) is `bind_nil`, not a statement whose
    /// model never loaded; a default scope that is not a plain condition says
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
        assert!(out.contains("thing specialized=1 fallback={\"bind_array\" => 1, \"bind_nil\" => 1}"), "{out}");
        assert!(out.contains("ranked specialized=0 fallback={\"default_scope_values[RankedThing]\" => 1}"), "{out}");
        assert!(out.contains("scoped specialized=1 fallback={}"), "{out}");
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
        // An Array, a Set (which has no entry in StatementCache.unsupported_value?), a Range, a
        // Relation, a record and nil are Rails': it expands them into IN, BETWEEN, a subquery or
        // IS NULL, and here finds the same record it would without a statement.
        assert!(normal.contains(
            "collection=array:1/1 set:1/1 range:2/1 endless:2/1 relation:3/1 empty_set:nil/1 record:2/1 nil:nil/1\n"
        ), "{normal}");
    }

    #[test]
    fn an_association_key_that_is_not_one_column_runs_rails_chain() {
        let Some(normal) = probe("0") else { return };
        // A composite target has no single key to bind. With query constraints (Rails 8.2) the
        // join keys are one column and the query keys are not: the key is read like the
        // predicate builder reads it (n/a where the Rails in use has no such option).
        assert!(normal.contains("association=single:1/0 composite_key:nil composite:1/1 constrained_key:"), "{normal}");
        let line = normal.lines().find(|l| l.starts_with("association=")).unwrap();
        assert!(line.ends_with("constrained_key:n/a constrained:n/a") || line.ends_with("constrained_key:nil constrained:1/1"), "{line}");
        // A chain that cannot be built over placeholders is Rails' too, from the first call on.
        assert!(normal.contains("unbuildable=[[2, 1], [3, 1]]\n"), "{normal}");
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
                .filter(|l| l.starts_with("scalar=") || l.starts_with("collection="))
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
    fn a_constant_value_is_evaluated_at_the_call_site_but_a_call_on_one_is_not() {
        let (report, files) = run("class Finder\n  def self.named\n    Tenant.find_by(name: Tenant::NAME)\n  end\n  def self.again(x)\n    Tenant.find_by(name: Tenant.fetch(x))\n  end\nend\n");
        assert_eq!(report.specialized.len(), 1, "{:?}", report.residue);
        let finder = &files.iter().find(|(p, _)| p == "app/services/finder.rb").unwrap().1;
        assert!(finder.contains("Futamura.records(") && finder.contains("[Futamura.find_by_value(Tenant::NAME)]"), "{finder}");
        assert!(report.residue.iter().any(|r| r.contains("value is Send")), "{:?}", report.residue);
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
        let dir = std::env::temp_dir().join(format!("roundhouse-futamura-stats-{}", std::process::id()));
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

/// `a&.b` returns nil for a nil `a`; the specialized call reads the owner's
/// key first (`a.id`), and analysis binds a non-trivial owner to a temp local
/// (`__safe_nav1`) that exists only in its IR, so such a chain stays Rails'.
mod safe_navigation_owner {
    use std::path::PathBuf;

    use roundhouse::analyze::Analyzer;
    use roundhouse::emit::futamura::specialize;
    use roundhouse::ingest::ingest_app_from_tree;

    #[test]
    fn a_chain_through_safe_navigation_is_left_to_rails() {
        let files: Vec<(&str, &str)> = vec![
            ("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table \"rooms\", force: :cascade do |t|\n    t.string \"name\"\n  end\n  create_table \"messages\", force: :cascade do |t|\n    t.integer \"room_id\"\n    t.string \"body\"\n  end\nend\n"),
            ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n"),
            ("app/models/room.rb", "class Room < ApplicationRecord\n  has_many :messages\nend\n"),
            ("app/models/message.rb", "class Message < ApplicationRecord\n  belongs_to :room\nend\n"),
            ("app/services/finder.rb", "class Finder\n  #: (Room?) -> Array[Message]?\n  def self.of(room)\n    room&.messages&.to_a\n  end\n  #: (Room) -> Array[Message]\n  def self.sure(room)\n    room.messages.to_a\n  end\nend\n"),
        ];
        let tree: roundhouse::det::HashMap<PathBuf, Vec<u8>> =
            files.iter().map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec())).collect();
        let mut app = ingest_app_from_tree(tree).expect("ingest");
        Analyzer::new(&app).analyze(&mut app);
        let mut tree_files: Vec<(String, String)> = files.iter().map(|(p, c)| (p.to_string(), c.to_string())).collect();
        let report = specialize(&app, &mut tree_files).unwrap();
        let finder = &tree_files.iter().find(|(p, _)| p == "app/services/finder.rb").unwrap().1;
        assert!(finder.lines().filter(|l| l.contains("room&.messages")).all(|l| !l.contains("Futamura.records")), "{finder}");
        assert_eq!(report.specialized.len(), 1, "{:?}", report.residue);
        assert!(report.residue.iter().any(|r| r.contains("safe navigation in the chain")), "{:?}", report.residue);
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
/// differently from what the original chain used. Such a site stays Rails'.
mod bind_timing {
    use std::path::PathBuf;

    use roundhouse::analyze::Analyzer;
    use roundhouse::emit::futamura::specialize;
    use roundhouse::ingest::ingest_app_from_tree;

    const SCHEMA: &str = "ActiveRecord::Schema.define do\n  create_table \"items\", force: :cascade do |t|\n    t.integer \"shop_id\"\n    t.string \"kind\"\n  end\nend\n";

    fn run(body: &str) -> roundhouse::emit::futamura::Report {
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
        specialize(&app, &mut tree_files).unwrap()
    }

    fn timing_residue(report: &roundhouse::emit::futamura::Report) -> bool {
        report.residue.iter().any(|r| r.contains("may change between"))
    }

    #[test]
    fn a_value_nothing_reassigns_is_read_late_harmlessly() {
        // A parameter, or a local written once before the chain, has the same value at the call site.
        let report = run("    scope = Item.where(shop_id: shop)\n    scope.to_a");
        assert_eq!(report.specialized.len(), 1, "{:?}", report.residue);
        let report = run("    id = shop\n    scope = Item.where(shop_id: id)\n    scope.to_a");
        assert_eq!(report.specialized.len(), 1, "{:?}", report.residue);
        // A chain written as one expression reads its values when the call does.
        let report = run("    shop = 7\n    Item.where(shop_id: shop).to_a");
        assert_eq!(report.specialized.len(), 1, "{:?}", report.residue);
    }

    #[test]
    fn a_local_reassigned_after_the_relation_was_built_is_left_to_rails() {
        // Rails' chain bound the old `shop`; the call site would bind 7.
        let report = run("    scope = Item.where(shop_id: shop)\n    shop = 7\n    scope.to_a");
        assert_eq!(report.specialized.len(), 0);
        assert!(timing_residue(&report), "{:?}", report.residue);
        let report = run("    id = 1\n    scope = Item.where(shop_id: id)\n    id = 2\n    scope.to_a");
        assert_eq!(report.specialized.len(), 0);
        assert!(timing_residue(&report), "{:?}", report.residue);
    }

    #[test]
    fn an_instance_variable_written_in_the_method_is_left_to_rails() {
        let report = run("    scope = Item.where(shop_id: @shop)\n    scope.to_a");
        assert_eq!(report.specialized.len(), 1, "{:?}", report.residue);
        let report = run("    scope = Item.where(shop_id: @shop)\n    @shop = 7\n    scope.to_a");
        assert_eq!(report.specialized.len(), 0);
        assert!(timing_residue(&report), "{:?}", report.residue);
    }

    #[test]
    fn a_call_that_can_change_what_a_value_reads_is_left_to_rails() {
        // Nothing runs between the `where` and the terminal but more links on the relation.
        let report = run("    scope = Item.where(shop_id: shop.id)\n    scope = scope.where(kind: kind)\n    scope.to_a");
        assert_eq!(report.specialized.len(), 1, "{:?}", report.residue);
        // Another call may change `shop.id`.
        let report = run("    scope = Item.where(shop_id: shop.id)\n    shop.reload\n    scope.to_a");
        assert_eq!(report.specialized.len(), 0);
        assert!(timing_residue(&report), "{:?}", report.residue);
        // The terminal runs later, in a block, as many times as the block is called.
        let report = run("    scope = Item.where(shop_id: shop.id)\n    [1].each { scope.to_a }");
        assert_eq!(report.specialized.len(), 0);
        assert!(timing_residue(&report), "{:?}", report.residue);
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
        let dir = std::env::temp_dir().join(format!("roundhouse-futamura-flow-runtime-{}", std::process::id()));
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
            init.contains("owner_keys: [\"room_id\"], owner_columns: [\"room_id\", \"pinned\"]) { |p| Message.all.where(room_id: p.bind).where(pinned: true).order(:id) }"),
            "{init}"
        );
        assert!(init.contains("Message.all.where(room_id: p.bind).order(\"id DESC\").send(:ordered_relation).limit(2)"), "{init}");
        assert!(
            init.contains("\"Note\", owner_keys: [\"subject_id\", \"subject_type\"], owner_columns: [\"subject_id\", \"subject_type\"]) { |p| Note.all.where(subject_id: p.bind).where(subject_type: p.bind) }"),
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
        let out = std::env::temp_dir().join(format!("roundhouse-futamura-assoc-{}", std::process::id()));
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
        for name in ["not_blank", "pinned", "pinned_ordered", "loaded_chain", "unsaved_chain", "first_pinned", "newest", "notes", "same_column", "fragment", "kinds", "kinds_not", "not_fragment"] {
            assert!(specialized(name), "{name}\n{normal}");
        }
        for name in ["remarks", "loaded", "unsaved"] {
            assert!(fell_back(name), "{name}\n{normal}");
        }
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
        write(&src, "app/models/widget.rb", b"class Widget < ApplicationRecord\nend\n");
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
        assert!(finder.contains("Futamura.find_by_value(model.polymorphic_name)"), "{finder}");
        assert!(!finder.contains("not supported"), "{finder}");
        // A constant in the block is public and rooted, a literal, or bound; never private and rooted.
        let init = fs::read_to_string(out.join("config/initializers/futamura.rb")).unwrap();
        assert!(!init.contains("Finder::PAGE") && !init.contains("Finder::LIMIT"), "{init}");
        assert!(finder.contains("[shop_id, PAGE]"), "{finder}");
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
        assert!(normal.ends_with("specialized=3\n"), "{normal}");
        assert!(normal.contains("by_kind=[\"a\", \"b\", \"c\"]\n"), "{normal}");
        assert!(normal.contains("limited=[\"a\", \"b\"]\n"), "{normal}");
        assert!(normal.contains("by_model=[\"d\"]\n"), "{normal}");
        let _ = fs::remove_dir_all(&out);
    }
}
