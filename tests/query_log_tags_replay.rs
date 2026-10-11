//! activerecord's `active_record.query_log_tags_config` initializer
//! (railtie.rb:405-437) does its work only under
//! `config.active_record.query_log_tags_enabled`. The replay in
//! app/models.rb is emitted for an app that writes that key, and only
//! then: an app that does not has nothing for it to configure, and
//! without an Application class there is no `Rails.application.config`
//! to register its after-initialize step on, so a bare load of
//! app/models.rb raised there.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const REPLAY: &str = "ActiveRecord::QueryLogs.apply_railtie_config(";

fn models(application: Option<&str>) -> String {
    let mut app = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\ndatabase_adapter: postgresql\n")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_query_log_tags_replay\n");
    if let Some(body) = application {
        app = app.write(
            "config/application.rb",
            &format!("module Blog\n  class Application < Rails::Application\n{body}  end\nend\n"),
        );
    }
    let (dir, _) = app.emit(BuildTarget::Ruby);
    std::fs::read_to_string(dir.join("app/models.rb")).expect("app/models.rb")
}

#[test]
fn an_app_that_never_enables_query_log_tags_has_no_replay() {
    assert!(!models(None).contains(REPLAY));
    assert!(!models(Some("")).contains(REPLAY));
    let other = "    config.active_record.query_log_tags_format = :sqlcommenter\n";
    assert!(!models(Some(other)).contains(REPLAY));
}

#[test]
fn an_app_that_enables_query_log_tags_replays_its_config() {
    let body = "    config.active_record.query_log_tags_enabled = true\n    config.active_record.query_log_tags_format = :sqlcommenter\n";
    let text = models(Some(body));
    let at = text.find(REPLAY).unwrap_or_else(|| panic!("no replay in:\n{text}"));
    let call = &text[at..text[at..].find('\n').map_or(text.len(), |n| at + n)];
    assert!(call.contains("enabled: Rails.application."), "{call}");
    assert!(call.contains("format: Rails.application."), "{call}");
    assert!(call.contains("tags: nil"), "{call}");
}
