//! A constant read inside a multiple assignment's index or splat target
//! resolves from the lexical scope it is written in.
//!
//! Core's lib/development_support/local_datastores.rb writes `old,
//! ENV["VERBOSE"] = ENV["VERBOSE"], "false"`. Rubydex's indexer skips a
//! multi-write's index targets, `*rest` and later targets, so the `ENV`
//! there had no source answer and was "constant not supported".
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const STORE: &str = r#"module Kit
  STORE = {}

  class Store
    def self.swap(value)
      old, ENV["RH_MULTI_WRITE"] = ENV["RH_MULTI_WRITE"], value
      count, STORE[:last] = STORE.size, value
      first, *STORE[:rest] = 1, 2, 3
      [old, count, first, ENV["RH_MULTI_WRITE"], STORE[:last], STORE[:rest]]
    end
  end
end
"#;

const SCRIPT: &str = r#"p Kit::Store.swap("one")
p Kit::Store.swap("two")
p Kit::STORE
"#;

#[test]
fn constants_in_multi_write_index_and_splat_targets_resolve_lexically() {
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("{STORE}\n{SCRIPT}"))
        .env_remove("RH_MULTI_WRITE")
        .output()
        .expect("native Ruby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = "[nil, 0, 1, \"one\", \"one\", [2, 3]]\n[\"one\", 2, 1, \"two\", \"two\", [2, 3]]\n{last: \"two\", rest: [2, 3]}\n";
    assert_eq!(String::from_utf8_lossy(&native.stdout), expected);
    let run = emit_and_run::empty_app().write("app/models/kit/store.rb", STORE).run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout, expected);
}
