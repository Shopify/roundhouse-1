//! Shopify Core's `ActiveSupport::Cache::MemoryMappedStore` runs on the
//! emitted PostgreSQL CRuby tree over the gem's real LMDB extension.
//!
//! memory_mapped_store 0.4.4 is a gem source (`gem_sources:`; its cache
//! store needs ActiveSupport). Its `MemoryMappedStore::DB` is Ruby around a
//! C extension, and its entry file keeps a load-time conditional (the
//! `Process._fork` hook) the ingest does not model, so the compiled entry
//! carried only `VERSION`. The store's `max_readers` default read
//! `::MemoryMappedStore::DEFAULT_MAX_READERS`, refused as an unsupported
//! constant (an inline `raise`), so `LocalCache.init` raised, left
//! `LocalCache.cache` nil and every
//! `DequeueDecoratorTestHelper#reset_background_queue_local_cache`
//! (`LocalCache.cache.clear`) failed with `undefined method 'clear' for
//! nil` (sweep-1 family 48938dcd, 12 ids). The tree now carries the entry
//! file's Ruby as a runtime port over the gem's own extension
//! (`project::EXTENSION_GEM_PORTS`), so keys, values, expiry, counters,
//! size limits and persistence are LMDB's.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::path::{Path, PathBuf};

use roundhouse::project::BuildTarget;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    memory_mapped_store (0.4.4)\n      activesupport (>= 7)\n\nDEPENDENCIES\n  memory_mapped_store (>= 0.4.4)\n";

/// The installed gem's directory, from the gem home the test runs under.
fn gem_dir() -> PathBuf {
    let out = emit_and_run::ruby()
        .args(["-e", "puts Gem::Specification.find_by_name('memory_mapped_store', '= 0.4.4').gem_dir"])
        .output()
        .expect("ruby");
    assert!(
        out.status.success(),
        "memory_mapped_store 0.4.4 must be installed on Gem.path (its built extension is what this test runs):\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    PathBuf::from(String::from_utf8(out.stdout).unwrap().trim())
}

/// The gem's `lib` Ruby files, as vendored sources (the extension itself is
/// found from the gem home).
fn vendored_lib(gem: &Path) -> Vec<(String, String)> {
    ["memory_mapped_store.rb", "memory_mapped_store/version.rb", "active_support/cache/memory_mapped_store.rb"]
        .iter()
        .map(|file| {
            let text = std::fs::read_to_string(gem.join("lib").join(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
            (format!("vendor/gems/memory_mapped_store-0.4.4/lib/{file}"), text)
        })
        .collect()
}

fn emit_app() -> (emit_and_run::Emitted, Vec<String>) {
    let gem = gem_dir();
    let mut app = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\ndatabase_adapter: postgresql\ngem_sources:\n  memory_mapped_store: vendor/gems/memory_mapped_store-0.4.4\n")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_memory_mapped_store\n")
        .write("Gemfile.lock", LOCK)
        .write(
            "lib/queue_cache.rb",
            "# Core's Podding::BackgroundQueue::LocalCache, in miniature.\n\
             module QueueCache\n  class << self\n    attr_reader :cache\n\n    \
             def init(path, size:, ttl:)\n      @cache = ActiveSupport::Cache::MemoryMappedStore.new(path, size: size)\n      @cache.ttl = ttl\n    end\n\n    \
             def write(key, value)\n      retries = 1\n      cache.write(key, value)\n    rescue ::MemoryMappedStore::Error, ::MemoryMappedStore::FinalError\n      raise unless retries > 0\n      retries -= 1\n      cache.clear\n      retry\n    end\n  end\nend\n",
        );
    for (path, text) in vendored_lib(&gem) {
        app = app.write(&path, &text);
    }
    let (dir, errors) = app.emit(BuildTarget::Ruby);
    // The entry file's `Process._fork` conditional is the ingest gap the
    // port covers (named once, for lib/memory_mapped_store.rb).
    let errors = errors
        .into_iter()
        .filter(|e| !e.contains("database_adapter not supported") && !e.contains("a load-time conditional definition whose branch holds a statement other than a `def`"))
        .collect();
    (dir, errors)
}

const BOOT: &str = "require_relative 'runtime/active_support_lazy_load_hooks'\n\
     $LOAD_PATH.unshift(File.expand_path('runtime/features'))\n\
     require_relative 'runtime/module_delegate'\n\
     require_relative 'runtime/active_support_inflector'\n\
     require_relative 'runtime/rails'\n\
     Rails.env_name = 'test'\n\
     require_relative 'runtime/rails_railtie'\n\
     require_relative 'runtime/rails_engine'\n\
     require_relative 'runtime/active_support_deprecation'\n";

#[test]
fn the_tree_carries_the_extension_gems_entry_file_ruby_over_its_extension() {
    let (dir, errors) = emit_app();
    assert!(errors.is_empty(), "{errors:?}");
    let read = |path: &str| std::fs::read_to_string(dir.join(path)).unwrap_or_else(|e| panic!("{path}: {e}"));
    let loader = read("runtime/extension_gems.rb");
    assert!(loader.contains("[\"memory_mapped_store\", \"0.4.4\"]"), "{loader}");
    assert!(loader.contains("require_relative \"memory_mapped_store\""), "{loader}");
    let port = read("runtime/memory_mapped_store.rb");
    assert!(port.contains("require \"memory_mapped_store/memory_mapped_store\""), "{port}");
    assert!(port.contains("class DB"), "{port}");
    assert!(port.contains("module ForkHooks"), "{port}");
    // The gem's own native file loads from the gem home. It was reported
    // "not loaded" because the gem is one the app compiles from source.
    let facades = read("runtime/gem_facades.rb");
    assert!(facades.contains("__rh_extension_features = [\"memory_mapped_store/memory_mapped_store\"]"), "{facades}");
    assert!(!facades.contains("gem_source_file_not_compiled]: \\\"memory_mapped_store/memory_mapped_store"), "{facades}");
    assert!(read("Gemfile").contains("gem \"memory_mapped_store\", \"0.4.4\", require: false"));
    // The store's own constants are the port's, not refused by name.
    let store = read("app/models/active_support/cache/memory_mapped_store.rb");
    assert!(!store.contains("constant not supported"), "{store}");
    assert!(store.contains("::MemoryMappedStore::DEFAULT_MAX_READERS"), "{store}");
    let cache = read("app/models/queue_cache.rb");
    assert!(!cache.contains("gem_requires_rails"), "{cache}");
    assert!(cache.contains("::MemoryMappedStore::Error"), "{cache}");
}

#[test]
fn the_memory_mapped_store_keeps_values_expires_and_counts_in_lmdb() {
    let (dir, errors) = emit_app();
    assert!(errors.is_empty(), "{errors:?}");
    let path = dir.join("tmp-cache");
    let small = dir.join("tmp-small");
    let small2 = dir.join("tmp-small2");
    let script = format!(
        "{BOOT}require_relative 'runtime/extension_gems'\n\
         require_relative 'app/models'\n\
         require 'active_support/cache/memory_mapped_store'\n\
         QueueCache.init({path:?}, size: 1 << 20, ttl: 60)\n\
         cache = QueueCache.cache\n\
         p cache.class\n\
         cache.write('a', {{ 'x' => [1, 2] }})\n\
         p cache.read('a')\n\
         p cache.read('missing')\n\
         p cache.size\n\
         p cache.increment('n', 3)\n\
         p cache.increment('n')\n\
         p cache.decrement('n', 2)\n\
         cache.write('short', 'v', expires_in: 0.05)\n\
         sleep 0.1\n\
         p cache.read('short')\n\
         p cache.delete('a')\n\
         p cache.read('a')\n\
         p cache.ttl\n\
         p cache.readers_count.class\n\
         p MemoryMappedStore::OOMError.ancestors.first(3)\n\
         p MemoryMappedStore::LMDB_VERSION\n\
         # counters are raw LMDB values, not cache entries: read is nil\n\
         # (the gem under native Ruby answers the same)\n\
         other = ActiveSupport::Cache::MemoryMappedStore.new({path:?}, size: 1 << 20)\n\
         p other.read('n')\n\
         # the cache lives on disk: a second handle on the same path sees it\n\
         cache.write('shared', 'seen')\n\
         p other.read('shared')\n\
         cache.clear\n\
         p [cache.size, other.read('shared')]\n\
         # a full database raises OOMError (MDB_MAP_FULL)\n\
         db = MemoryMappedStore::DB.new({small:?}, max_size: 1 << 16)\n\
         full = 0\n\
         200.times {{ |i| begin; db.put(\"key#{{i}}\", 'v' * 4096); rescue MemoryMappedStore::OOMError; full += 1; end }}\n\
         p [full > 0, db.size < 200]\n\
         # the store clears a full database and retries the write\n\
         QueueCache.init({small2:?}, size: 1 << 16, ttl: 60)\n\
         200.times {{ |i| QueueCache.write(\"key#{{i}}\", 'v' * 4096) }}\n\
         p QueueCache.cache.size < 200\n\
         p QueueCache.cache.read('key199').size\n"
    );
    let out = emit_and_run::ruby().current_dir(&*dir).args(["-e", &script]).output().expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stdout}\n{stderr}");
    assert_eq!(
        stdout,
        "ActiveSupport::Cache::MemoryMappedStore\n{\"x\" => [1, 2]}\nnil\n1\n3\n4\n2\nnil\ntrue\nnil\n60\nInteger\n[MemoryMappedStore::OOMError, MemoryMappedStore::FinalError, MemoryMappedStore::Error]\n\"0.9.31\"\nnil\n\"seen\"\n[0, nil]\n[true, true]\ntrue\n4096\n",
        "{stderr}"
    );
}
