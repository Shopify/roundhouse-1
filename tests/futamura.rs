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
        // The call site binds the owner's id and keeps the chain as fallback.
        assert!(controller.contains(
            "@messages = Futamura.records(:controllers_messages_controller_4_17, [@room.id], owner: @room, \
             association: :messages) { @room.messages.with_creator.last_page }"
        ), "{controller}");
        // Static scopes stay calls for Rails to build; the terminal scope is
        // inlined, its constant qualified so the initializer can name it,
        // and `last(n)` follows Rails' FinderMethods: ordered_relation,
        // limit, reverse_order, then the rows reversed.
        assert!(init.contains(
            "Futamura.define(:controllers_messages_controller_4_17, Message, reverse: true) \
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
        assert!(init.contains("Story, one: true) { |p| Story.all.where(short_id: p.bind).send(:ordered_relation).limit(1) }"), "{init}");
        assert!(init.contains("Story, one: true) { |p| Story.all.where(short_id: p.bind).limit(1) }"), "{init}");
        assert!(out.contains("[params[:id].to_s]) { Story.where(short_id: params[:id].to_s).first }"), "{out}");
        assert!(out.contains("[params[:id] || params[:story_id]]) {"), "{out}");
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

/// Rails inherits default scopes down the superclass chain and through
/// included modules (activerecord/lib/active_record/scoping/default.rb), so
/// only a model that reaches a `default_scope` is left to Rails.
mod default_scope_reach {
    use std::path::PathBuf;

    use roundhouse::analyze::Analyzer;
    use roundhouse::emit::futamura::specialize;
    use roundhouse::ingest::ingest_app_from_tree;

    const SCHEMA: &str = "ActiveRecord::Schema.define do\n  create_table \"plain_things\", force: :cascade do |t|\n    t.string \"name\"\n  end\n  create_table \"scoped_things\", force: :cascade do |t|\n    t.string \"name\"\n  end\n  create_table \"nested_things\", force: :cascade do |t|\n    t.string \"name\"\n  end\n  create_table \"derived_things\", force: :cascade do |t|\n    t.string \"name\"\n  end\n  create_table \"dynamic_things\", force: :cascade do |t|\n    t.string \"name\"\n  end\n  create_table \"gem_things\", force: :cascade do |t|\n    t.string \"name\"\n  end\nend\n";

    fn run() -> roundhouse::emit::futamura::Report {
        let files: Vec<(&str, &str)> = vec![
            ("db/schema.rb", SCHEMA),
            ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n"),
            // A concern that scopes whoever includes it.
            ("app/models/concerns/tenanted.rb", "module Tenanted\n  extend ActiveSupport::Concern\n  included do\n    default_scope { where(name: Current.name) }\n  end\nend\n"),
            // A concern that reaches the scoping one only through another.
            ("app/models/concerns/wrapped.rb", "module Wrapped\n  extend ActiveSupport::Concern\n  included do\n    include Tenanted\n  end\nend\n"),
            ("app/models/concerns/harmless.rb", "module Harmless\n  extend ActiveSupport::Concern\n  def hello = 1\nend\n"),
            ("app/models/base_thing.rb", "class BaseThing < ApplicationRecord\n  self.abstract_class = true\n  default_scope { order(:id) }\nend\n"),
            ("app/models/plain_thing.rb", "class PlainThing < ApplicationRecord\n  include Harmless\nend\n"),
            ("app/models/scoped_thing.rb", "class ScopedThing < ApplicationRecord\n  include Tenanted\nend\n"),
            ("app/models/nested_thing.rb", "class NestedThing < ApplicationRecord\n  include Wrapped\nend\n"),
            ("app/models/derived_thing.rb", "class DerivedThing < BaseThing\nend\n"),
            ("app/models/dynamic_thing.rb", "class DynamicThing < ApplicationRecord\n  include Module.new\nend\n"),
            // A gem's module has no app source: Rails reports its scopes at run time.
            ("app/models/gem_thing.rb", "class GemThing < ApplicationRecord\n  include Discard::Model\nend\n"),
            (
                "app/services/finder.rb",
                "class Finder\n  def self.all_of(x)\n    [PlainThing.where(name: x).to_a, ScopedThing.where(name: x).to_a, NestedThing.where(name: x).to_a, DerivedThing.where(name: x).to_a, DynamicThing.where(name: x).to_a, GemThing.where(name: x).to_a]\n  end\nend\n",
            ),
        ];
        let tree: roundhouse::det::HashMap<PathBuf, Vec<u8>> =
            files.iter().map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec())).collect();
        let mut app = ingest_app_from_tree(tree).expect("ingest");
        Analyzer::new(&app).analyze(&mut app);
        let mut tree_files: Vec<(String, String)> = files.iter().map(|(p, c)| (p.to_string(), c.to_string())).collect();
        specialize(&app, &mut tree_files).unwrap()
    }

    #[test]
    fn only_models_that_reach_a_default_scope_stay_residue() {
        let report = run();
        let residue = report.residue.join("\n");
        for stays in ["ScopedThing", "NestedThing", "DerivedThing", "DynamicThing"] {
            assert!(residue.contains(&format!("{stays} may have a default_scope")), "{stays}: {residue}");
        }
        // The unrelated concern's default_scope no longer taints every model.
        assert!(!residue.contains("PlainThing may have"), "{residue}");
        assert!(!residue.contains("GemThing may have"), "{residue}");
        assert_eq!(report.specialized.len(), 2, "{:?}\n{residue}", report.specialized);
    }

    #[test]
    fn the_residue_names_where_the_default_scope_came_from() {
        let report = run();
        let residue = report.residue.join("\n");
        assert!(residue.contains("tenanted.rb declares a default_scope"), "{residue}");
        assert!(residue.contains("base_thing.rb declares a default_scope"), "{residue}");
        assert!(residue.contains("dynamic_thing.rb includes a module the specializer cannot resolve"), "{residue}");
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
        assert!(finder.contains("[Futamura.association_key(::Note, :author, author), slug]"), "{finder}");
    }

    #[test]
    fn a_polymorphic_belongs_to_key_is_left_to_rails() {
        let (report, _) = run("class Finder\n  def self.of(subject)\n    Note.where(subject: subject).to_a\n  end\nend\n");
        assert!(report.specialized.is_empty(), "{:?}", report.specialized);
        assert!(report.residue.iter().any(|r| r.contains("polymorphic belongs_to `subject`")), "{:?}", report.residue);
    }
}
