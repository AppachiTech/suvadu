use crate::repository::Repository;

pub fn test_repo() -> (tempfile::TempDir, Repository) {
    let dir = tempfile::TempDir::new().unwrap();
    let conn = crate::db::init_db(&dir.path().join("test.db")).unwrap();
    (dir, Repository::new(conn))
}
