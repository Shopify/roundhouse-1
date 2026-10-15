//! A class body loop that registers through a class macro the app's own
//! superclass defines. Core's `AssetCloud::ZippedThemeFiles < AssetCloud::Base`:
//!
//! ```ruby
//! (AssetCloud::ThemeFiles.asset_classes.keys - BLOCKED_THEME_BUCKETS).each do |bucket_name|
//!   bucket bucket_name, ZipfileBucket, asset_class: asset_class
//! end
//! ```
//!
//! `bucket` is a class method of the compiled asset_cloud gem's `Base`, but
//! the emit carried a computed-receiver body statement only when the class
//! itself defined (or any class answers) every macro it calls; the loop was
//! dropped with no diagnostic, no `sections`, `templates`, ... bucket
//! registered, and every read of a theme file of a zipped theme raised
//! `AssetCloud::InvalidBucketError: No such namespace: sections/main-article.liquid`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const BASE: &str = r#"class Registry
  def self.registered
    @registered ||= []
  end

  def self.register(name, klass = nil, options = {})
    registered << [name, klass, options]
  end
end
"#;

const SUB: &str = r#"class Sub < Registry
  NAMES = { a: "x", b: "y", c: "z" }.freeze
  BLOCKED = [:c].freeze

  register :root

  (NAMES.keys - BLOCKED).each do |name|
    value = NAMES[name]
    register name, String, asset_class: value
  end

  NAMES.each do |key, options|
    register key, nil, options
  end
end
"#;
#[test]
fn a_loop_calling_the_superclasss_macro_runs_in_the_emitted_body() {
    let app = || {
        emit_and_run::empty_app()
            .write("app/models/registry.rb", BASE)
            .write("app/models/sub.rb", SUB)
    };
    let (dir, _) = app().emit(BuildTarget::Ruby);
    let sub = std::fs::read_to_string(dir.join("app/models/sub.rb")).expect("emitted sub.rb");
    assert!(sub.contains("(NAMES.keys - BLOCKED).each do |name|"), "the loop is dropped:\n{sub}");
    let run = app().run_ruby("p Sub.registered\n");
    run.assert_passes();
    assert_eq!(
        run.stdout,
        "[[:root, nil, {}], [:a, String, {asset_class: \"x\"}], [:b, String, {asset_class: \"y\"}], [:a, nil, \"x\"], [:b, nil, \"y\"], [:c, nil, \"z\"]]\n"
    );
}
