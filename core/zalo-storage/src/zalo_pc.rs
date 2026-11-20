//! Reads the friend list Zalo PC keeps on this machine.
//!
//! Zalo PC (`FriendListManager._loadFriendLocal`) starts from its own cache
//! in `ZaloData/Database/_production/<uid>/Core/Index.db`, table `friend`,
//! and only asks the server again on a timer. A fresh session of another
//! client is answered with the recently active friends only, so this cache
//! is the only full copy on the machine.
//!
//! The database is open in the running Zalo PC, so it (with its `-wal` and
//! `-shm`) is copied to a private temporary folder and opened read-only.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::StorageError;

/// One cached friend, columns named as the server names the JSON fields.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PcFriend {
    /// Text columns (`userId`, `displayName`, `zaloName`, `avatar`, …).
    pub text: BTreeMap<String, String>,
    /// Integer columns (`gender`, `isBlocked`, `lastActionTime`, …).
    pub number: BTreeMap<String, i64>,
}

const TEXT_COLUMNS: [&str; 6] = ["userId", "displayName", "zaloName", "avatar", "username", "sdob"];
const NUMBER_COLUMNS: [&str; 7] = [
    "gender",
    "isBlocked",
    "lastActionTime",
    "isActive",
    "isActivePC",
    "isActiveWeb",
    "isFr",
];

/// `<config_home>/ZaloData/Database/_production/<uid>/Core/Index.db`, when it exists.
pub fn zalo_pc_index_db(config_home: &Path, uid: &str) -> Option<PathBuf> {
    if uid.is_empty() || !uid.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let path = config_home
        .join("ZaloData")
        .join("Database")
        .join("_production")
        .join(uid)
        .join("Core")
        .join("Index.db");
    path.is_file().then_some(path)
}

/// Friends as Zalo PC lists them in Contacts: `isFr = 1`, `isValid = 1`,
/// `type = 0`, without the account itself.
///
/// # Errors
///
/// Returns [`StorageError`] when the files cannot be copied or read.
pub fn read_zalo_pc_friends(index_db: &Path, self_uid: &str) -> Result<Vec<PcFriend>, StorageError> {
    with_copied_index(index_db, "zalo-pc-friends", |copy| read_typed(copy, self_uid, 0))
}

/// Official Accounts (`type = 7`) from the same `friend` table.
///
/// They stay in the chat list and out of Contacts / Strangers.
///
/// # Errors
///
/// Returns [`StorageError`] when the files cannot be copied or read.
pub fn read_zalo_pc_oa(index_db: &Path, self_uid: &str) -> Result<Vec<PcFriend>, StorageError> {
    with_copied_index(index_db, "zalo-pc-oa", |copy| read_typed(copy, self_uid, 7))
}

fn with_copied_index<T>(
    index_db: &Path,
    prefix: &str,
    read: impl FnOnce(&Path) -> Result<T, StorageError>,
) -> Result<T, StorageError> {
    let scratch = std::env::temp_dir().join(format!("{prefix}-{}", std::process::id()));
    std::fs::create_dir_all(&scratch)?;
    let result = (|| {
        let copy = scratch.join("Index.db");
        std::fs::copy(index_db, &copy)?;
        for suffix in ["-wal", "-shm"] {
            let side = PathBuf::from(format!("{}{suffix}", index_db.display()));
            if side.is_file() {
                std::fs::copy(&side, scratch.join(format!("Index.db{suffix}")))?;
            }
        }
        read(&copy)
    })();
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

fn read_typed(db: &Path, self_uid: &str, friend_type: i64) -> Result<Vec<PcFriend>, StorageError> {
    let con = rusqlite::Connection::open_with_flags(
        db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let columns: Vec<&str> = TEXT_COLUMNS.iter().chain(NUMBER_COLUMNS.iter()).copied().collect();
    let sql = format!(
        "SELECT {} FROM friend WHERE isFr = 1 AND COALESCE(isValid, 1) = 1 \
         AND COALESCE(type, 0) = ?2 AND userId <> ?1",
        columns.iter().map(|c| format!("\"{c}\"")).collect::<Vec<_>>().join(", ")
    );
    let mut stmt = con.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params![self_uid, friend_type], |row| {
        let mut friend = PcFriend::default();
        for (index, name) in TEXT_COLUMNS.iter().enumerate() {
            if let Some(value) = row.get::<_, Option<String>>(index)? {
                friend.text.insert((*name).to_owned(), value);
            }
        }
        for (offset, name) in NUMBER_COLUMNS.iter().enumerate() {
            if let Some(value) = row.get::<_, Option<i64>>(TEXT_COLUMNS.len() + offset)? {
                friend.number.insert((*name).to_owned(), value);
            }
        }
        Ok(friend)
    })?;
    let mut out = Vec::new();
    for row in rows {
        let friend = row?;
        if friend.text.get("userId").is_some_and(|id| !id.is_empty()) {
            out.push(friend);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_listed_friends_only() {
        let dir = std::env::temp_dir().join(format!("zalo-pc-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let db = dir.join("Index.db");
        let _ = std::fs::remove_file(&db);
        let con = rusqlite::Connection::open(&db).expect("open");
        con.execute_batch(
            "CREATE TABLE friend (userId TEXT, username TEXT, displayName TEXT, zaloName TEXT,
               avatar TEXT, sdob TEXT, gender INTEGER, isBlocked INTEGER, lastActionTime INTEGER,
               isActive INTEGER, isActivePC INTEGER, isActiveWeb INTEGER, isFr INTEGER,
               isValid INTEGER, type INTEGER);
             INSERT INTO friend (userId, displayName, isFr, isValid, type, gender) VALUES
               ('1', 'An', 1, 1, 0, 0),
               ('2', 'Bình', 1, 0, 0, 1),
               ('3', 'OA', 1, 1, 7, 0),
               ('9', 'Me', 1, 1, 0, 0),
               ('4', 'Cũ', 0, 1, 0, 0);",
        )
        .expect("seed");
        drop(con);
        let friends = read_zalo_pc_friends(&db, "9").expect("read");
        assert_eq!(friends.len(), 1);
        assert_eq!(friends[0].text["displayName"], "An");
        assert_eq!(friends[0].number["gender"], 0);
        let oa = read_zalo_pc_oa(&db, "9").expect("oa");
        assert_eq!(oa.len(), 1);
        assert_eq!(oa[0].text["displayName"], "OA");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
