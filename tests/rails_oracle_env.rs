// The native oracle's gem home selection (tests/support/rails_oracle.rb):
// the default is Core's current gem home, `CORE_GEM_HOME` selects another
// (the previous Core's stays selectable), and the gem path adds bundler's
// patched source gems and the Ruby's own gem dir, where a newer bundled
// minitest sits that Core's Gemfile.lock does not select.
use std::process::Command;

const CURRENT: &str = "/nix/store/d958gj4a8pmld3fxa7fhyqasz61cafch-ruby-gems-dev/lib/ruby/gems/4.0.0";
const PREVIOUS: &str = "/nix/store/kr4swvh5mw01fs3kixfa9afy57zsyihg-ruby-gems-dev/lib/ruby/gems/4.0.0";

fn oracle(home: Option<&str>, script: &str) -> String {
    let mut cmd = Command::new("ruby");
    cmd.current_dir(env!("CARGO_MANIFEST_DIR"))
        .env_remove("CORE_GEM_HOME")
        .env_remove("CORE_GEM_PATH")
        .env_remove("RH_RAILS_ORACLE")
        .args(["-e", &format!("require './tests/support/rails_oracle'\n{script}")]);
    if let Some(home) = home {
        cmd.env("CORE_GEM_HOME", home);
    }
    let out = cmd.output().expect("ruby");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn oracle_defaults_to_the_current_core_gem_home_and_its_rails_checkout() {
    if !std::path::Path::new(CURRENT).is_dir() {
        return;
    }
    let out = oracle(None, "puts RailsOracle::CORE_GEM_HOME, RailsOracle::CORE_GEM_PATH.split(':').size, File.basename(RailsOracle::PINNED)");
    assert_eq!(out, format!("{CURRENT}\n3\nrails-5eb8c4c45bee"));
}

#[test]
fn oracle_keeps_the_previous_gem_home_selectable() {
    if !std::path::Path::new(PREVIOUS).is_dir() {
        return;
    }
    let out = oracle(Some(PREVIOUS), "puts RailsOracle::CORE_GEM_PATH, File.basename(RailsOracle::PINNED)");
    assert_eq!(out, format!("{PREVIOUS}\nrails-5813559f4aed"));
}

#[test]
fn oracle_activates_the_gem_homes_minitest_not_the_rubys_bundled_one() {
    if !std::path::Path::new(CURRENT).is_dir() {
        return;
    }
    let script = "ENV['RH_RAILS_ORACLE_EXEC'] = '1'\nGem.paths = { 'GEM_HOME' => RailsOracle::CORE_GEM_HOME, 'GEM_PATH' => RailsOracle::CORE_GEM_PATH }\nRailsOracle.pin_gem_home_versions\nputs Gem.loaded_specs.fetch('minitest').version";
    assert_eq!(oracle(None, script), "5.27.0");
}
