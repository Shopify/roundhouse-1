//! Core's `Secrets` boot shape, against native ActiveSupport 8.1.4 as
//! the oracle: a module whose `class << self` reads
//! `config/vault.<env>.json` through ERB, deep-merges it into
//! `with_indifferent_access`, and `delegate`s `fetch`/`key?`/`dig` to
//! that hash on the class side; callers in another file pass arguments.
//! Also `HashWithIndifferentAccess` and `deep_merge` themselves.
//!
//! The JSON file is app data under `config/`, carried verbatim into the
//! emitted tree.
//!
//! Not covered: `hwia.reverse_merge(defaults)`. Ingest rewrites every
//! `reverse_merge` to `defaults.merge(recv)` before types are known,
//! which is a plain Hash with Symbol keys beside the String ones when
//! `recv` is indifferent (`[:a]` reads the default, Rails reads the
//! receiver's value). `with_defaults`, its alias, is not rewritten.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

const VAULT: &str = r##"module Vault
  ROOT = File.expand_path("../../..", __FILE__)

  class << self
    def [](key)
      hash.fetch(key)
    end

    delegate(:fetch, :key?, :dig, to: :hash)

    def hash
      @hash ||= from_env.with_indifferent_access
    end

    attr_writer(:hash)

    private

    def from_env
      [
        File.join(ROOT, "config/vault.json"),
        File.join(ROOT, "config/vault.test.json"),
      ].each.with_object({}) do |file, secrets|
        secrets.deep_merge!(from_json_file(file)) if File.exist?(file)
      end
    end

    def from_json_file(filename)
      JSON.parse(ERB.new(File.read(filename)).result(binding))
    end
  end
end
"##;

const VAULT_JSON: &str = r##"{
  "mysql": { "readonly": "r-<%= 40 + 2 %>", "shards": [{ "name": "s1" }] },
  "plain": 1
}
"##;

const VAULT_TEST_JSON: &str = r##"{
  "mysql": { "writer": "w" },
  "extra": true
}
"##;

const REPORT_SRC: &str = r##"module VaultReport
  def self.lines
    out = []
    out << "index=#{Vault[:mysql][:readonly]} #{Vault["mysql"]["readonly"]} #{Vault[:mysql][:writer]}"
    out << "fetch=#{Vault.fetch(:plain)} #{Vault.fetch(:missing, "dflt")} #{Vault.fetch("extra")}"
    out << "key=#{Vault.key?(:mysql)} #{Vault.key?("nope")}"
    out << "dig=#{Vault.dig(:mysql, :shards, 0, :name)} #{Vault.dig("mysql", "shards", 0, "name")}"
    out << "class=#{Vault.hash.class.name} #{Vault[:mysql].class.name} #{Vault[:mysql][:shards][0].class.name}"
    begin
      Vault[:missing]
    rescue KeyError => e
      out << "missing=#{e.class}"
    end
    out
  end

  def self.hash_lines
    h = { "a" => 1, b: { "c" => [{ d: 2 }] } }.with_indifferent_access
    h[:x] = { y: 1 }
    out = []
    out << "read=#{h[:a]} #{h["b"][:c][0]["d"]} #{h.fetch("a")} #{h.dig(:b, :c, 0, :d)} #{h[:x]["y"]} #{h.key?(:b)}"
    out << "keys=#{h.keys.inspect} #{h.except(:a).keys.inspect} #{h.slice(:a).inspect}"
    out << "classes=#{h.class.name} #{h.to_hash.class.name} #{h.to_hash["b"].class.name} #{h.merge(z: 1).class.name} #{h.select { |k, _| k == "a" }.class.name}"
    out << "merge=#{h.merge(z: 1)["z"]} #{h.with_defaults(a: 9, q: 2)[:a]} #{h.with_defaults(a: 9, q: 2)[:q]} #{h.values_at(:a, "b").size}"
    out << "sym=#{h.symbolize_keys.keys.inspect} #{h.deep_symbolize_keys[:b][:c][0].inspect}"
    out << "delete=#{h.delete(:x).inspect} #{h.key?("x")}"
    deep = { a: { b: 1, c: 2 }, l: [1] }.deep_merge({ a: { b: 3 }, l: [2] })
    summed = { a: { b: 1 }, n: 1 }.deep_merge({ a: { b: 3 }, n: 4 }) { |_key, left, right| left + right }
    target = { a: { b: 1 } }
    target.deep_merge!({ a: { c: 2 } })
    out << "deep=#{deep.inspect} #{summed.inspect} #{target.inspect}"
    out
  end
end
"##;

const REPORT: &str = "puts VaultReport.lines\nputs VaultReport.hash_lines\n";

const EXPECTED: &str = "\
index=r-42 r-42 w
fetch=1 dflt true
key=true false
dig=s1 s1
class=ActiveSupport::HashWithIndifferentAccess ActiveSupport::HashWithIndifferentAccess ActiveSupport::HashWithIndifferentAccess
missing=KeyError
read=1 2 1 2 1 true
keys=[\"a\", \"b\", \"x\"] [\"b\", \"x\"] {\"a\" => 1}
classes=ActiveSupport::HashWithIndifferentAccess Hash Hash ActiveSupport::HashWithIndifferentAccess ActiveSupport::HashWithIndifferentAccess
merge=1 1 2 2
sym=[:a, :b, :x] {d: 2}
delete={\"y\" => 1} false
deep={a: {b: 3, c: 2}, l: [2]} {a: {b: 4}, n: 5} {a: {b: 1, c: 2}}
";

#[test]
fn native_activesupport_8_1_4_answers() {
    let dir = std::env::temp_dir().join(format!("roundhouse-vault-native-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (path, text) in [
        ("app/lib/vault.rb", VAULT),
        ("app/lib/vault_report.rb", REPORT_SRC),
        ("config/vault.json", VAULT_JSON),
        ("config/vault.test.json", VAULT_TEST_JSON),
    ] {
        let full = dir.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
    let output = Command::new("ruby")
        .arg("-e")
        .arg(format!(
            "gem \"activesupport\", \"8.1.4\"\nrequire \"active_support\"\nrequire \"active_support/core_ext/hash\"\nrequire \"active_support/core_ext/module/delegation\"\nrequire \"json\"\nrequire \"erb\"\nrequire \"./app/lib/vault\"\nrequire \"./app/lib/vault_report\"\n{REPORT}"
        ))
        .current_dir(&dir)
        .output()
        .expect("native Ruby control");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8_lossy(&output.stdout), EXPECTED);
}

#[test]
fn emitted_app_matches_native_activesupport() {
    let run = emit_and_run::real_blog()
        .write("app/lib/vault.rb", VAULT)
        .write("app/lib/vault_report.rb", REPORT_SRC)
        .write("config/vault.json", VAULT_JSON)
        .write("config/vault.test.json", VAULT_TEST_JSON)
        .run_ruby(REPORT);
    assert!(run.emitted.join("config/vault.test.json").is_file(), "config JSON carried as data");
    run.assert_passes();
    assert_eq!(run.stdout, EXPECTED);
}
