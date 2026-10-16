//! Which directories Ruby resolves constant paths under, and which source
//! files that makes autoloadable: one answer for the emitted autoload
//! index (`project::apply_models_autoload`) and for `defined?(Const)`
//! folding (`lower::defined_const_fold`), so the two agree on whether the
//! emitted program has a constant without an explicit `require`.

use std::collections::BTreeSet;

use crate::app::App;

pub struct AutoloadRoots<'a> {
    app_root: &'a str,
    /// Each Zeitwerk loader's root, relative to the app root.
    pub loader_roots: BTreeSet<&'a str>,
    /// Directories an engine's `paths.add(…, eager_load: true)` roots
    /// (`App::engine_autoload_roots`).
    engine_roots: BTreeSet<&'a str>,
    /// Directories a loader collapses (they add no namespace).
    collapsed_dirs: BTreeSet<&'a str>,
}

impl<'a> AutoloadRoots<'a> {
    pub fn of(app: &'a App) -> Self {
        let rel = |path: &'a str| path.strip_prefix(app.root.as_str()).unwrap_or(path).trim_matches('/');
        AutoloadRoots {
            app_root: app.root.as_str(),
            loader_roots: app.zeitwerk_loaders.iter().map(|l| rel(l.root.as_str())).collect(),
            engine_roots: app.engine_autoload_roots.iter().map(|r| r.trim_matches('/')).collect(),
            collapsed_dirs: app
                .zeitwerk_loaders
                .iter()
                .flat_map(|l| l.collapsed.iter())
                .map(|d| rel(d.as_str()))
                .collect(),
        }
    }

    /// The directory a constant's path is read from must be one Ruby
    /// resolves constant paths under: a Rails autoload root (`app/<x>`,
    /// `app/<x>/concerns`, a Zeitwerk loader's root) or a `lib/` on the
    /// load path. `lib/podding/patches/active_record.rb` names
    /// Podding::Patches::ActiveRecord, not the ActiveRecord it reopens.
    pub fn is_root(&self, dir: &[&str]) -> bool {
        let n = dir.len();
        (n >= 1 && dir[n - 1] == "lib") || self.is_autoload_root(dir)
    }

    /// [`is_root`](Self::is_root) without a component's or engine's own
    /// `lib/` (`components/x/lib`) on the load path: a file there is found
    /// by a `require`, and only an autoload root makes its constant
    /// defined before anything requires it. The app's top-level `lib/`
    /// stays one (`config.autoload_lib`).
    pub fn is_autoload_root(&self, dir: &[&str]) -> bool {
        let n = dir.len();
        dir == ["lib"]
            || (n >= 2 && dir[n - 2] == "app")
            || (n >= 3 && dir[n - 1] == "concerns" && dir[n - 3] == "app")
            || self.loader_roots.contains(dir.join("/").as_str())
            || self.engine_roots.contains(dir.join("/").as_str())
    }

    /// A directory a Zeitwerk loader collapses adds no namespace
    /// (`ingest::zeitwerk`): a source's path is read for its constant
    /// without those directories.
    pub fn constant_path(&self, path: &str) -> String {
        let segments: Vec<&str> = path.split('/').collect();
        segments
            .iter()
            .enumerate()
            .filter(|(i, _)| i + 1 == segments.len() || !self.collapsed_dirs.contains(segments[..=*i].join("/").as_str()))
            .map(|(_, segment)| *segment)
            .collect::<Vec<_>>()
            .join("/")
    }

    /// `source` relative to the app root.
    pub fn rel<'s>(&self, source: &'s str) -> &'s str {
        source.strip_prefix(self.app_root).map(|p| p.trim_start_matches('/')).unwrap_or(source)
    }

    /// Does an autoload root map `source` to the constant `underscored`
    /// (`a/b/c` for `A::B::C`), compared as Zeitwerk's camelize would,
    /// without underscores and case (`organizations_api.rb` for
    /// OrganizationsAPI)? An engine's `lib/` on the load path is no autoload
    /// root: Core's essentials `lib/verdict/evaluation_tracker.rb` is
    /// defined only once the to_prepare hook requires it, which is why
    /// `CapturesEventReporterContext#initialize` asks `defined?` first.
    pub fn source_names(&self, source: &str, underscored: &str) -> bool {
        let fold = |text: &str| text.chars().filter(|c| *c != '_').collect::<String>().to_ascii_lowercase();
        let rel = self.rel(source);
        let path = self.constant_path(rel.strip_suffix(".rb").unwrap_or(rel));
        let depth = underscored.matches('/').count() + 1;
        let segments: Vec<&str> = path.split('/').collect();
        if segments.len() < depth {
            return false;
        }
        let (dir, named) = segments.split_at(segments.len() - depth);
        fold(&named.join("/")) == fold(underscored) && self.is_autoload_root(dir)
    }
}
