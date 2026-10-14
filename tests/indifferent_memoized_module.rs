//! A module memoizing an indifferent hash behind a `self.hash` reader,
//! read with a caller's Symbol: Core's `Secrets` (`Secrets[:mysql]` in
//! database.yml). Rails answers `v|w`.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SECRETS: &str = r##"module Secrets
  def self.[](key)
    hash.fetch(key)
  end

  def self.hash
    @hash ||= from_env.with_indifferent_access
  end

  def self.hash=(value)
    @hash = value
  end

  def self.from_env
    [{ "mysql" => { "host" => "v" } }, { "redis" => "w" }].each.with_object({}) do |part, secrets|
      secrets.deep_merge!(part)
    end
  end
  private_class_method :from_env
end
"##;

#[test]
fn memoized_indifferent_hash_reads_a_symbol_key() {
    emit_and_run::empty_app()
        .write("app/models/secrets.rb", SECRETS)
        .run_ruby(
            r##"
text = "#{Secrets[:mysql][:host]}|#{Secrets[:redis]}"
raise "got #{text.inspect}" unless text == "v|w"
puts "ALL OK"
"##,
        )
        .assert_passes();
}
