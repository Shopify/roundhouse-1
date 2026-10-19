//! `Rails.application.load_tasks` (e2e sweep-1, round 4). Core's
//! RakeTaskTestCase runs it in every setup; the runtime's
//! `Rails::Application` had no such method (NoMethodError, 11 ids that
//! pass natively). It now requires rake and loads the `.rake` files the
//! emitted tree carries under `lib/tasks` and under each component, as
//! Rails does for the application and its engines. A tree with none says
//! so on stderr: the compiler does not emit an app's `.rake` files yet,
//! and an answer that hid it would read as every task being defined.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const ROOT_TASKS: &str = r##"module ProbeTasks
  extend Rake::DSL
  extend self

  def greeting
    "hello from #{name}"
  end

  namespace :probe do
    desc "Say hello"
    task :hello do
      puts greeting
    end
  end
end
"##;

const COMPONENT_TASKS: &str = r##"namespace :probe do
  task :component do
    puts "component task"
  end
end
"##;

const SCRIPT: &str = r##"
p Rails.application.load_tasks.class
p Rake::Task.task_defined?("probe:hello")
p Rake::Task.task_defined?("probe:component")
Rake.application.tasks.each(&:reenable)
Rake::Task["probe:hello"].invoke
Rake::Task["probe:component"].invoke
"##;

fn run(dir: &std::path::Path, script: &str) -> std::process::Output {
    let script = format!(
        "# encoding: utf-8\nrequire File.expand_path(\"main\", Dir.pwd)\nMain.configure_default_adapter!\n{script}"
    );
    emit_and_run::ruby()
        .arg("-e")
        .arg(script)
        .current_dir(dir)
        .env("BLOG_DB", ":memory:")
        .output()
        .expect("spawn ruby")
}

#[test]
fn load_tasks_loads_the_rake_files_of_the_root_and_its_components() {
    let (dir, _) = emit_and_run::real_blog().emit(BuildTarget::Ruby);
    std::fs::create_dir_all(dir.join("lib/tasks")).unwrap();
    std::fs::write(dir.join("lib/tasks/probe.rake"), ROOT_TASKS).unwrap();
    std::fs::create_dir_all(dir.join("components/probe/lib/tasks")).unwrap();
    std::fs::write(dir.join("components/probe/lib/tasks/more.rake"), COMPONENT_TASKS).unwrap();
    let out = run(&dir, SCRIPT);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Rails::Application\ntrue\ntrue\nhello from ProbeTasks\ncomponent task\n"
    );
    assert!(!String::from_utf8_lossy(&out.stderr).contains("load_tasks found no .rake"));
}

#[test]
fn load_tasks_names_a_tree_that_carries_no_rake_file() {
    let (dir, _) = emit_and_run::real_blog().emit(BuildTarget::Ruby);
    let out = run(&dir, "p Rails.application.load_tasks.class\np Rake::Task.task_defined?(\"probe:hello\")\n");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "Rails::Application\nfalse\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Rails.application.load_tasks found no .rake file in the emitted tree"),
        "{stderr}"
    );
}
