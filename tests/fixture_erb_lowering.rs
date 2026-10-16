//! ERB-templated YAML fixtures (`ingest::fixture` + `lower::fixtures`).
//!
//! Rails renders every fixture file through ERB before YAML sees it,
//! and ingest used to drop any file containing a tag — "only knowable
//! to a running Ruby." That reasoning holds for the *value*, not for
//! the *program*: the emitted loader IS Ruby, so a tag can ride through
//! as an expression and evaluate where Rails would have evaluated it.
//!
//! campfire is the forcing function. Its `users.yml`, `messages.yml`
//! and `sessions.yml` are the only three fixture files it has with ERB
//! — and they are exactly the three every one of its 63 test files
//! needs (signing in needs a user, half the model tests reference
//! `messages(:third)`). Between them they cover the whole surface this
//! file pins: a statement tag binding a local, a value tag reading it
//! back, a relative time, and a call into the app's own model.

use roundhouse::app::App;
use roundhouse::dialect::FixtureValue;
use roundhouse::expr::ExprNode;
use roundhouse::Symbol;

/// A campfire-shaped app: one model whose columns cover each ERB value
/// kind, plus the fixture file under test.
fn app_with_fixture(fixture_yml: &str) -> App {
    let files: Vec<(&str, &str)> = vec![
        (
            "db/schema.rb",
            "ActiveRecord::Schema.define(version: 1) do\n  \
             create_table :users do |t|\n    \
             t.string :name\n    \
             t.string :password_digest\n    \
             t.string :bot_token\n    \
             t.datetime :created_at\n  \
             end\nend\n",
        ),
        (
            "app/models/user.rb",
            "class User < ApplicationRecord\n  \
             def self.generate_bot_token\n    \
             \"tok\"\n  end\nend\n",
        ),
        (
            "test/fixtures/users.yml",
            Box::leak(fixture_yml.to_string().into_boxed_str()),
        ),
    ];
    let tree = files
        .into_iter()
        .map(|(p, c)| (std::path::PathBuf::from(p), c.as_bytes().to_vec()))
        .collect();
    roundhouse::ingest::ingest_app_from_tree(tree).expect("ingest tree")
}

/// The emitted `test/fixtures/users.rb` — the loader a test run
/// actually executes.
fn emitted_fixture(app: &App) -> String {
    roundhouse::emit::ruby::emit_spinel(app)
        .into_iter()
        .find(|f| f.path.to_string_lossy() == "test/fixtures/users.rb")
        .map(|f| f.content)
        .expect("test/fixtures/users.rb")
}

/// campfire's `users.yml`, trimmed to two records. All three tag kinds.
const CAMPFIRE_USERS: &str = r#"<% password_digest = BCrypt::Password.create("secret123456") %>

david:
  name: David
  password_digest: <%= password_digest %>
  created_at: <%= 1.hour.ago %>

bender:
  name: Bender
  password_digest: <%= password_digest %>
  bot_token: <%= User.generate_bot_token %>
"#;

// ── ingest ──────────────────────────────────────────────────────────

#[test]
fn an_erb_fixture_is_ingested_rather_than_dropped() {
    // The regression this whole file exists for: before ERB lowering,
    // `app.fixtures` came back EMPTY for a file like this and the only
    // trace was a ledger line.
    let app = app_with_fixture(CAMPFIRE_USERS);
    let users = app
        .fixtures
        .iter()
        .find(|f| f.name.as_str() == "users")
        .expect("users fixture ingested");
    assert_eq!(users.records.len(), 2, "both labels survive the ERB split");
}

#[test]
fn a_statement_tag_becomes_the_fixtures_preamble() {
    // `<% password_digest = … %>` produces no output — it BINDS, and
    // four records downstream read the binding. It has to run once,
    // ahead of the inserts, which is what `preamble` is for.
    let app = app_with_fixture(CAMPFIRE_USERS);
    let users = &app.fixtures[0];
    assert_eq!(users.preamble.len(), 1, "one statement tag");
}

#[test]
fn a_value_tag_stays_an_expression_and_a_scalar_stays_a_scalar() {
    // The distinction that makes the rest work: `name: David` is data
    // the compiler knows, `password_digest: <%= … %>` is a program it
    // doesn't. Folding the second into a string at ingest would bake
    // one run's bcrypt digest (or one run's clock) into the emit.
    let app = app_with_fixture(CAMPFIRE_USERS);
    let david = app.fixtures[0]
        .records
        .get(&Symbol::from("david"))
        .expect("david");

    assert_eq!(
        david.get(&Symbol::from("name")).and_then(|v| v.as_scalar()),
        Some("David"),
    );
    assert!(matches!(
        david.get(&Symbol::from("password_digest")),
        Some(FixtureValue::Ruby(_)),
    ));
    assert!(matches!(
        david.get(&Symbol::from("created_at")),
        Some(FixtureValue::Ruby(_)),
    ));
}

#[test]
fn a_fixture_without_erb_is_untouched() {
    // The blog's shape. Every value is a scalar and there is no
    // preamble — this path must not change.
    let app = app_with_fixture("one:\n  name: Plain\n");
    let users = &app.fixtures[0];
    assert!(users.preamble.is_empty());
    assert_eq!(
        users.records[&Symbol::from("one")]
            .get(&Symbol::from("name"))
            .and_then(|v| v.as_scalar()),
        Some("Plain"),
    );
}

#[test]
fn a_tag_interpolated_into_a_larger_scalar_builds_the_string() {
    // `name: "hi <%= 1 %> there"` is a string BUILT at runtime, not the
    // tag's own result: ERB writes the tag's `to_s` into the text, which
    // is exactly a Ruby string interpolating the tag. Shopify core writes
    // this in 25 rows.
    let tree = vec![(
        std::path::PathBuf::from("test/fixtures/users.yml"),
        b"one:\n  name: \"hi <%= 1 %> there\"\n".to_vec(),
    )]
    .into_iter()
    .collect();
    let app = roundhouse::ingest::ingest_app_from_tree(tree).expect("ingest");
    let name = &app.fixtures[0].records[&Symbol::from("one")][&Symbol::from("name")];
    let FixtureValue::Ruby(expr) = name else {
        panic!("expected a Ruby value, got {name:?}");
    };
    let ExprNode::StringInterp { parts } = &*expr.node else {
        panic!("expected an interpolated string, got {:?}", expr.node);
    };
    assert_eq!(parts.len(), 3, "{parts:?}");
}

// ── emit (ruby) ─────────────────────────────────────────────────────

#[test]
fn the_preamble_runs_ahead_of_the_inserts_in_the_loader() {
    // Ordering is the correctness property: `password_digest` is a
    // local, so its binding must lexically precede every read of it
    // inside the same method body.
    let out = emitted_fixture(&app_with_fixture(CAMPFIRE_USERS));
    let bind = out
        .find("password_digest = BCrypt::Password.create(\"secret123456\")")
        .expect("preamble assignment emitted");
    let read = out
        .find("instance.password_digest = password_digest")
        .expect("value tag read emitted");
    assert!(bind < read, "binding must precede the read:\n{out}");
}

#[test]
fn a_relative_time_tag_grounds_to_a_duration() {
    // `1.hour.ago` reaches emit ungrounded — fixture classes are
    // synthesized after the shared post-analyze hooks have run over the
    // App — so `emit::ruby` re-applies the duration rewrite to them.
    // Without it this emits a bare `1.hour`, which no target's runtime
    // answers.
    let out = emitted_fixture(&app_with_fixture(CAMPFIRE_USERS));
    assert!(
        out.contains("ActiveSupport::Duration"),
        "expected a grounded Duration in:\n{out}",
    );
}

#[test]
fn a_tag_calling_the_apps_own_model_emits_as_that_call() {
    // `<%= User.generate_bot_token %>` is a call into emitted code —
    // the fixture loader and the model land in the same tree, so this
    // needs nothing beyond ordinary dispatch.
    let out = emitted_fixture(&app_with_fixture(CAMPFIRE_USERS));
    assert!(
        out.contains("User.generate_bot_token"),
        "expected the model call in:\n{out}",
    );
}

/// A `def` in a statement tag (`<% def helper(x) … end %>`) is Rails'
/// `ERB#result` of the file in the render context's binding, where the
/// `def` defines a method on the context class and the value tags call it
/// as `helper(…)`. The loader's body is a singleton method, so a `def`
/// written there defines an instance method nothing calls: the emitted
/// loader must define the helper on the module's singleton.
#[test]
fn a_def_in_a_statement_tag_is_callable_from_the_value_tags() {
    let app = app_with_fixture(
        "<%\ndef tagged(name, primary)\n  prefix = primary ? \"1\" : \"0\"\n  \"#{prefix}-#{name}\"\nend\n%>\n\n\
         david:\n  name: <%= tagged(:david, true) %>\n",
    );
    let emitted = emitted_fixture(&app);
    let load = emitted.split("def self._fixtures_load!").nth(1).expect("loader");
    assert!(
        load.contains("def self.tagged(name, primary)"),
        "a bare nested `def` defines an instance method of the module, which the singleton loader cannot call:\n{emitted}"
    );
    assert!(load.contains("instance.name = tagged(:david, true)"), "{emitted}");
}

/// A mapping field with a tag inside (`instruction:` of a jsonb column,
/// `payment_provider_id: <%= … %>` under it) is the Hash Psych reads from
/// the rendered text: a tag that is a whole scalar is read the way YAML
/// reads what it wrote (an Integer), text around a tag builds a string, and
/// the untouched scalars keep their types. It used to ride as JSON text
/// with the tag quoted, which the jsonb column then encoded a second time:
/// the reader answered a String, not a Hash.
#[test]
fn a_mapping_with_a_tag_inside_is_a_hash_not_json_text() {
    let app = app_with_fixture(
        "david:\n  name:\n    id: <%= 40 + 2 %>\n    label: \"x-<%= 1 %>\"\n    nested:\n      count: 3\n      list: [a, b]\n",
    );
    let value = &app.fixtures[0].records[&Symbol::from("david")][&Symbol::from("name")];
    let FixtureValue::Ruby(expr) = value else {
        panic!("expected a Ruby value, got {value:?}");
    };
    assert!(matches!(&*expr.node, ExprNode::Hash { .. }) || matches!(&*expr.node, ExprNode::Seq { .. }), "{:?}", expr.node);
    let emitted = emitted_fixture(&app);
    assert!(
        emitted.contains(
            r#"{ "id" => ActiveRecord::TestFixtures.erb_scalar(40 + 2), "label" => "x-#{1}", "nested" => { "count" => 3, "list" => ["a", "b"] } }"#
        ),
        "{emitted}"
    );
}
