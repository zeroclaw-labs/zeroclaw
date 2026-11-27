use filetime::{FileTime, set_file_mtime};
use std::{
    fs,
    path::Path,
    time::{Duration, SystemTime},
};
use zeroclaw_config::schema::MemoryConfig;
use zeroclaw_memory::hygiene::run_if_due;

fn write_dated(path: &Path, text: &str, age_days: u64) {
    fs::write(path, text).unwrap();
    let time = SystemTime::now() - Duration::from_secs(age_days * 86_400);
    set_file_mtime(path, FileTime::from_system_time(time)).unwrap();
}

#[test]
fn migrated_sessions_keep_their_suffix_and_expire_after_a_collision() {
    let workspace = tempfile::tempdir().unwrap();
    let sessions = workspace.path().join("sessions");
    let archive = sessions.join("archive");
    fs::create_dir_all(&archive).unwrap();
    let filename = "thread.jsonl.migrated";
    write_dated(&sessions.join(filename), "recently archived", 10);
    write_dated(&archive.join(filename), "expired copy", 40);
    write_dated(
        &archive.join("old.jsonl_2.migrated"),
        "expired collision",
        40,
    );
    write_dated(
        &archive.join("notes.jsonl_backup.migrated"),
        "unrelated file",
        40,
    );

    run_if_due(&MemoryConfig::default(), workspace.path()).unwrap();

    assert!(!sessions.join(filename).exists());
    let mut filenames: Vec<_> = fs::read_dir(&archive)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    filenames.sort();
    assert_eq!(
        filenames,
        ["notes.jsonl_backup.migrated", "thread_1.jsonl.migrated"]
    );
    assert_eq!(
        fs::read_to_string(archive.join("thread_1.jsonl.migrated")).unwrap(),
        "recently archived"
    );
}
