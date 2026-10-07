//! `require "useragent"` on the PostgreSQL CRuby tree answers from the
//! runtime's port (runtime/user_agent.rb), never the gem home's gem.
//! Shopify Core's secure_headers gem source requires "useragent", and
//! gem_facades.rb's load of it loaded useragent 0.16.11 over the port:
//! "already initialized constant UserAgent::MATCHER" and the gem's methods
//! mixed into the port's classes.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

#[test]
fn a_native_require_of_useragent_loads_the_port() {
    let (dir, _) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\ndatabase_adapter: postgresql\n")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_user_agent_feature_port\n")
        .write("lib/oracle.rb", "class Oracle\nend\n")
        .emit(BuildTarget::Ruby);
    for feature in ["useragent", "user_agent", "platform_agent"] {
        let path = dir.join(format!("runtime/features/{feature}.rb"));
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert!(text.contains("require_relative \"../user_agent\""), "{text}");
    }

    let home = dir.join("tmp-gem-home");
    let lib = home.join("gems/useragent-0.16.11/lib");
    std::fs::create_dir_all(&lib).unwrap();
    std::fs::write(lib.join("useragent.rb"), "require 'user_agent'\n").unwrap();
    std::fs::write(lib.join("user_agent.rb"), "class UserAgent\n  NATIVE = true\n  MATCHER = /native/\nend\n").unwrap();
    std::fs::create_dir_all(home.join("specifications")).unwrap();
    std::fs::write(
        home.join("specifications/useragent-0.16.11.gemspec"),
        "Gem::Specification.new do |s|\n  s.name = \"useragent\"\n  s.version = \"0.16.11\"\n  s.summary = \"x\"\n  s.authors = [\"x\"]\n  s.files = [\"lib/useragent.rb\", \"lib/user_agent.rb\"]\n  s.require_paths = [\"lib\"]\nend\n",
    )
    .unwrap();
    let out = emit_and_run::ruby()
        .current_dir(&*dir)
        .env("RH_TEST_GEMS", &home)
        .args([
            "-e",
            // gem_facades.rb's order: the port, the features path, then
            // the gem source's dependency.
            "Gem.paths = { 'GEM_PATH' => [ENV['RH_TEST_GEMS'], *Gem.path].join(File::PATH_SEPARATOR) }\n\
             require_relative 'runtime/user_agent'\n\
             $LOAD_PATH.unshift(File.expand_path('runtime/features'))\n\
             require 'useragent'\n\
             require 'platform_agent'\n\
             puts defined?(UserAgent::NATIVE).inspect\n\
             puts UserAgent.parse('Mozilla/5.0 (X11; Linux x86_64; rv:126.0) Gecko/20100101 Firefox/126.0').browser\n\
             puts $LOADED_FEATURES.grep(%r{/useragent-}).inspect\n",
        ])
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stdout}\n{stderr}");
    assert_eq!(stdout, "nil\nFirefox\n[]\n", "{stderr}");
    assert!(!stderr.contains("already initialized constant"), "{stderr}");
}
