use super::*;
use crate::design::fonts::GEIST_REGULAR;

fn fixture() -> (tempfile::TempDir, FontCache, CjkFace) {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.ttc");
    fs::write(&source, b"source").unwrap();
    let face = CjkFace {
        family: "Test".into(),
        path: source,
        index: 2,
        weight: 400,
    };
    let cache = FontCache::new(dir.path().join("fonts"));
    (dir, cache, face)
}

fn store(cache: &FontCache, face: &CjkFace) {
    assert_eq!(
        cache
            .convert(face, || Some(GEIST_REGULAR.to_vec()))
            .unwrap(),
        GEIST_REGULAR
    );
}

#[test]
fn cache_hit_and_source_path_index_weight_size_and_mtime_invalidation() {
    let (_dir, cache, face) = fixture();
    assert!(cache.read(&face).is_none());
    store(&cache, &face);
    assert_eq!(cache.read(&face).unwrap(), GEIST_REGULAR);
    let mut changed = face.clone();
    changed.index += 1;
    assert!(cache.read(&changed).is_none());
    changed = face.clone();
    changed.weight = 500;
    assert!(cache.read(&changed).is_none());
    changed = face.clone();
    changed.path.set_file_name("other.ttc");
    fs::copy(&face.path, &changed.path).unwrap();
    assert!(cache.read(&changed).is_none());

    let original_time = fs::metadata(&face.path).unwrap().modified().unwrap();
    let file = File::options().write(true).open(&face.path).unwrap();
    file.set_times(fs::FileTimes::new().set_modified(original_time + Duration::from_secs(1)))
        .unwrap();
    assert!(cache.read(&face).is_none());
    file.set_times(fs::FileTimes::new().set_modified(original_time))
        .unwrap();
    assert!(cache.read(&face).is_some());
    fs::write(&face.path, b"larger source").unwrap();
    file.set_times(fs::FileTimes::new().set_modified(original_time))
        .unwrap();
    assert!(cache.read(&face).is_none(), "size alone invalidates");
}

#[test]
fn corrupt_and_oversized_cached_fonts_are_misses() {
    let (_dir, cache, face) = fixture();
    store(&cache, &face);
    let key = cache.key(&face).unwrap();
    fs::write(&key, &GEIST_REGULAR[..100]).unwrap();
    assert!(cache.read(&face).is_none());
    store(&cache, &face);
    File::options()
        .write(true)
        .open(&key)
        .unwrap()
        .set_len(MAX_FACE_BYTES + 1)
        .unwrap();
    assert!(cache.read(&face).is_none());
}

#[test]
fn cache_failure_and_source_change_during_conversion_do_not_block_rendering() {
    let (_dir, cache, face) = fixture();
    fs::write(&cache.directory, b"not a directory").unwrap();
    store(&cache, &face);
    assert!(cache.read(&face).is_none());
    fs::remove_file(&cache.directory).unwrap();
    let bytes = cache
        .convert(&face, || {
            fs::write(&face.path, b"updated source").unwrap();
            Some(GEIST_REGULAR.to_vec())
        })
        .unwrap();
    assert_eq!(bytes, GEIST_REGULAR);
    assert!(cache.read(&face).is_none());
    assert!(cache.convert(&face, || None).is_none());
}

#[test]
fn cache_evicts_old_weights_to_bound_total_size_and_removes_abandoned_writes() {
    let (_dir, mut cache, mut face) = fixture();
    cache.limit = (GEIST_REGULAR.len() * 2) as u64;
    store(&cache, &face);
    let old = cache.key(&face).unwrap();
    File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(1)))
        .unwrap();
    face.weight = 500;
    store(&cache, &face);
    face.weight = 600;
    store(&cache, &face);
    assert!(!old.exists());
    assert!(cache.read(&face).is_some());
    let total: u64 = fs::read_dir(&cache.directory)
        .unwrap()
        .map(|e| e.unwrap().metadata().unwrap().len())
        .sum();
    assert!(total <= cache.limit);
    let abandoned = cache.directory.join("ct-v0-abandoned.tmp");
    fs::write(&abandoned, b"incomplete").unwrap();
    File::options()
        .write(true)
        .open(&abandoned)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(UNIX_EPOCH))
        .unwrap();
    let active = cache.directory.join("ct-v1-active.tmp");
    fs::write(&active, b"still writing").unwrap();
    cache.prune();
    assert!(!abandoned.exists());
    assert!(active.exists());
}
