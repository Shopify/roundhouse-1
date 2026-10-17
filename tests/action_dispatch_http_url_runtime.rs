// ActionDispatch::Http::URL's module functions (runtime/spinel/
// action_dispatch_http_url.rb) against the Rails native Core runs
// (Shopify/rails 5813559f4aed, 8.2.0.alpha; tests/support/rails_oracle.rb).
// tests/action_dispatch_http_url_runtime.rb runs the same calls through
// the pinned actionpack and through the port and requires every
// observation equal.
//
// The port rides with the Ruby-family CRuby trees where app code names the
// module (Core's WebUrlHelpersFactory; tests/generated_helpers_from_yaml.rs
// runs that factory unchanged); a tree without it refuses the read by name.

use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn url_functions_answer_like_actionpack() {
    let out = Command::new("ruby")
        .arg(root().join("tests/action_dispatch_http_url_runtime.rb"))
        .arg("compare")
        .current_dir(root())
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success()
            && stdout.contains("Native Rails and emitted ActionDispatch::Http::URL agree"),
        "URL diverged from actionpack\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    assert!(
        stdout.lines().count() > 65,
        "too few observations:\n{stdout}"
    );
    for expected in [
        r#"params then anchor: "\"/admin?search=search+value#anchor\" US-ASCII""#,
        r#"params nested: "\"/p?user%5Bage%5D=3&user%5Bname%5D=N&z=1\" US-ASCII""#,
        r#"anchor escaped: "\"/a#a%20b%23c/d?e:f@g!h%25i%5Bj%5D%C3%BC\" US-ASCII""#,
        r#"full url: "\"https://www.shop1.com:80/admin?search=search+value#anchor\" UTF-8""#,
        r#"full url https 443: "\"https://example.org/a\" UTF-8""#,
        r#"full url missing host: "ArgumentError: "#,
    ] {
        assert!(stdout.contains(expected), "missing {expected:?}:\n{stdout}");
    }
}

const MODEL: &str = r##"class Probe < ApplicationRecord
  def self.link(id)
    ActionDispatch::Http::URL.full_url_for(path: "/probes/#{id}", params: { q: "a b" }, anchor: "x", host: "shop.test", protocol: "https", port: nil)
  end
end
"##;

fn probe_app(postgres: bool) -> PathBuf {
    probe_app_in("", postgres)
}

fn probe_app_in(tag: &str, postgres: bool) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rh_http_url{tag}_{}_{postgres}", std::process::id()));
    let mut files = vec![
        (
            "app/controllers/application_controller.rb",
            "class ApplicationController < ActionController::Base\nend\n",
        ),
        (
            "app/models/application_record.rb",
            "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n",
        ),
        (
            "db/schema.rb",
            "ActiveRecord::Schema.define do\n  create_table \"probes\" do |t|\n    t.string \"title\"\n  end\nend\n",
        ),
        ("app/models/probe.rb", MODEL),
    ];
    if postgres {
        files.push((
            "config/database.yml",
            "test:\n  adapter: postgresql\n  database: roundhouse_unused\n",
        ));
        files.push(("roundhouse.yml", "database_adapter: postgresql\n"));
    }
    for (path, text) in files {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    dir
}

fn run(dir: &Path, args: &[&str]) -> (bool, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_roundhouse"));
    command.args(args).arg(dir);
    if args[0] != "check" {
        command
            .arg("-o")
            .arg(dir.join(format!("emitted-{}", args[1])));
    }
    let output = command.output().unwrap();
    (
        output.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

#[test]
fn the_ruby_tree_carries_the_port_and_others_refuse_it_by_name() {
    let dir = probe_app(false);
    let (ok, text) = run(&dir, &["check"]);
    assert!(ok, "check refused ActionDispatch::Http::URL: {text}");
    assert!(
        !text.contains("constant not supported") && !text.contains("no known method"),
        "{text}"
    );
    let (ok, text) = run(&dir, &["--target", "ruby"]);
    assert!(
        ok,
        "the ruby tree refused ActionDispatch::Http::URL: {text}"
    );
    let emitted = dir.join("emitted-ruby");
    assert_eq!(
        std::fs::read_to_string(emitted.join("runtime/action_dispatch_http_url.rb")).unwrap(),
        std::fs::read_to_string(root().join("runtime/spinel/action_dispatch_http_url.rb")).unwrap()
    );
    let facades = std::fs::read_to_string(emitted.join("runtime/gem_facades.rb")).unwrap();
    assert!(
        facades.contains("require_relative \"action_dispatch_http_url\""),
        "{facades}"
    );
    let out = Command::new("ruby")
        .args(["-e", "require File.expand_path(\"main\", Dir.pwd)\nMain.configure_default_adapter!\np Probe.link(7)\n"])
        .current_dir(&emitted)
        .env("BLOG_DB", ":memory:")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "\"https://shop.test/probes/7?q=a+b#x\"\n"
    );
    for target in ["spinel", "rust"] {
        let (ok, text) = run(&dir, &["--target", target]);
        assert!(!ok, "{target} accepted ActionDispatch::Http::URL: {text}");
        assert!(
            text.contains("runtime_constant")
                && text.contains("runtime/action_dispatch_http_url.rb"),
            "{target} lost the named refusal: {text}"
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}

/// The PostgreSQL CRuby tree goes through the same Ruby-family assembly:
/// it carries the port and requires it before the app's code.
#[test]
fn the_postgresql_ruby_tree_carries_the_port() {
    use roundhouse::project::{BuildTarget, target_files};
    let dir = probe_app(true);
    let mut app = roundhouse::ingest::ingest_app(&dir).expect("ingest probe app");
    roundhouse::session::analyze_and_lower(&mut app);
    assert!(
        app.database_yml.is_some(),
        "not a PostgreSQL app: {:?}",
        app.database_refusals
    );
    let (files, diagnostics) =
        roundhouse::emit::diagnostics::scope(|| target_files(&app, &dir, BuildTarget::Ruby));
    let files = files.expect("package the PostgreSQL ruby tree");
    let refused: Vec<String> = diagnostics
        .iter()
        .map(ToString::to_string)
        .filter(|d| d.contains("Http::URL"))
        .collect();
    assert!(refused.is_empty(), "{refused:?}");
    let source = |path: &str| {
        files
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, text)| text.as_str())
            .unwrap_or_else(|| panic!("tree lacks {path}"))
    };
    assert_eq!(
        source("runtime/action_dispatch_http_url.rb"),
        std::fs::read_to_string(root().join("runtime/spinel/action_dispatch_http_url.rb")).unwrap()
    );
    assert!(
        source("runtime/gem_facades.rb").contains("require_relative \"action_dispatch_http_url\""),
        "{}",
        source("runtime/gem_facades.rb")
    );
    std::fs::remove_dir_all(dir).unwrap();
}

/// Core's StorefrontUrlHelpers parses a route pattern with Journey and
/// fills it from a params hash; the Journey port answers like actionpack.
#[test]
fn journey_route_patterns_parse_and_format() {
    const JOURNEY_MODEL: &str = r##"class Probe < ApplicationRecord
  def self.generate(pattern, params)
    ast = ActionDispatch::Journey::Ast.new(ActionDispatch::Journey::Parser.parse(pattern), false)
    formatter = ActionDispatch::Journey::Visitors::FormatBuilder.new.accept(ast.root)
    route_params = params.dup.extract!(*ast.path_params)
    [ast.path_params.map(&:to_s).join(","), formatter.evaluate(route_params), ast.root.to_s].join(" ")
  end
end
"##;
    let dir = probe_app_in("_journey", false);
    std::fs::write(dir.join("app/models/probe.rb"), JOURNEY_MODEL).unwrap();
    let (ok, text) = run(&dir, &["check"]);
    assert!(ok, "check refused Journey: {text}");
    assert!(
        !text.contains("constant not supported") && !text.contains("no known method"),
        "{text}"
    );
    let (ok, text) = run(&dir, &["--target", "ruby"]);
    assert!(ok, "the ruby tree refused Journey: {text}");
    let emitted = dir.join("emitted-ruby");
    let script = r#"require File.expand_path("main", Dir.pwd)
Main.configure_default_adapter!
puts Probe.generate("(/:shop_route_locale)/account/login(/:legacy_cart_param)(.:format)", { shop_route_locale: "en", format: "json", extra: 1 })
puts Probe.generate("(/:l)/gift_cards/:shop_id(.:format)/:token", { shop_id: 1, token: "a b" })
puts Probe.generate("/x/:id", {}).inspect
"#;
    let out = Command::new("ruby")
        .args(["-e", script])
        .current_dir(&emitted)
        .env("BLOG_DB", ":memory:")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "shop_route_locale,legacy_cart_param,format /en/account/login.json (/:shop_route_locale)/account/login(/:legacy_cart_param)(.:format)\n\
         l,shop_id,format,token /gift_cards/1/a%20b (/:l)/gift_cards/:shop_id(.:format)/:token\n\
         \"id  /x/:id\"\n"
    );
    std::fs::remove_dir_all(dir).unwrap();
}
