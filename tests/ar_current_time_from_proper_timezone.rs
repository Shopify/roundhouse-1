//! `ActiveRecord::Timestamp#current_time_from_proper_timezone`: Core's
//! `SofterDelete` and `Platform::Reaper::Policy` call it on a record
//! (private, bare) and on a model (public), and it answers the clock in
//! `ActiveRecord.default_timezone` (UTC unless an app sets `:local`).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SCRIPT: &str = r#"
now = Article.current_time_from_proper_timezone
raise "class: #{now.inspect}" unless now.is_a?(Time) && now.utc?
raise "stale" unless (Time.now - now).abs < 5

article = Article.new
raise "instance method must be private" if article.respond_to?(:current_time_from_proper_timezone)
at = article.send(:current_time_from_proper_timezone)
raise "instance: #{at.inspect}" unless at.is_a?(Time) && at.utc?

module ActiveRecord
  def self.default_timezone = :local
end
local = Article.current_time_from_proper_timezone
raise "local: #{local.inspect}" if local.utc? && Time.now.utc_offset != 0
puts "current_time_from_proper_timezone passed"
"#;

#[test]
fn a_model_and_its_record_answer_the_clock_in_the_default_timezone() {
    emit_and_run::real_blog().run_ruby(SCRIPT).assert_passes();
}
