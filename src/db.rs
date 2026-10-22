//! 모든 커넥션이 같이 쓰는 저장소입니다.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

/// 이 간격마다 저장소 전체를 훑어 만료된 키를 지웁니다. 진짜 Redis 기본값(hz 10)과 같은 100ms입니다.
const ACTIVE_EXPIRE_INTERVAL: Duration = Duration::from_millis(100);

/// 값과 만료 시각을 같이 둡니다. 키를 지우면 만료 시각도 같이 사라져서, 둘이 어긋날 일이 없습니다.
pub struct Entry {
    /// 바이트 그대로 둡니다. Redis 값에는 UTF-8이 아닌 바이트도 들어옵니다.
    pub value: Vec<u8>,
    /// `None`이면 TTL이 없는 키입니다.
    pub expires_at: Option<Instant>,
}

impl Entry {
    fn is_expired(&self, now: Instant) -> bool {
        self.expires_at.is_some_and(|at| at <= now)
    }
}

/// 키 → 값 맵입니다. 맵을 밖에 그대로 내주지 않고 메서드로만 만지게 해서,
/// 키를 읽는 곳이 전부 만료 확인(`live`)을 거치게 합니다.
#[derive(Default)]
pub struct Entries {
    map: HashMap<Vec<u8>, Entry>,
}

impl Entries {
    /// 살아 있는 키만 돌려줍니다. 만료된 키는 여기서 지우고 없는 키로 봅니다 (lazy expiration).
    pub fn live(&mut self, key: &[u8], now: Instant) -> Option<&mut Entry> {
        if self.map.get(key).is_some_and(|entry| entry.is_expired(now)) {
            self.map.remove(key);
        }
        self.map.get_mut(key)
    }

    /// 있던 값과 TTL은 통째로 바뀝니다.
    pub fn insert(&mut self, key: Vec<u8>, entry: Entry) {
        self.map.insert(key, entry);
    }

    /// 살아 있던 키를 지웠으면 true입니다. 이미 만료된 키는 지우기만 하고 false입니다.
    pub fn remove(&mut self, key: &[u8], now: Instant) -> bool {
        self.map
            .remove(key)
            .is_some_and(|entry| !entry.is_expired(now))
    }

    /// DBSIZE용입니다. 진짜 Redis처럼 만료됐지만 아직 안 지워진 키도 셉니다.
    pub fn count(&self) -> usize {
        self.map.len()
    }

    /// 만료된 키를 전부 지웁니다 (active expiration). `retain`은 true를 돌려준 것만 남깁니다.
    pub fn remove_expired(&mut self, now: Instant) {
        self.map.retain(|_, entry| !entry.is_expired(now));
    }
}

/// 커넥션 스레드 여러 개가 같은 `Db` 하나를 봅니다. (`main`에서 `Arc`로 나눠 줍니다)
/// `Mutex`가 한 번에 한 스레드만 안을 만지게 막습니다.
#[derive(Default)]
pub struct Db {
    entries: Mutex<Entries>,
}

impl Db {
    /// 락을 잡고 저장소를 엽니다. 돌려받은 guard가 사라질 때 락이 저절로 풀립니다.
    pub fn lock(&self) -> MutexGuard<'_, Entries> {
        // 락을 잡은 스레드가 패닉하면 락이 poison 됩니다. 그래도 안의 데이터를 그대로 씁니다.
        // 락 안에서 하는 일이 insert/remove/retain 하나라 반쯤 바뀐 상태가 남지 않기 때문입니다.
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// 아무도 다시 읽지 않는 키도 결국 지워지게 합니다. 서버가 켜져 있는 동안 끝나지 않습니다.
pub fn expire_forever(db: &Db) {
    loop {
        thread::sleep(ACTIVE_EXPIRE_INTERVAL);
        // guard가 이 문장 끝에서 바로 사라져서, 락은 훑는 동안만 잡고 자는 동안에는 풉니다.
        // 키가 아주 많으면 훑는 동안 다른 커넥션이 기다립니다. 그때는 Redis처럼 일부만 뽑아 보는 방식으로 바꿉니다.
        db.lock().remove_expired(Instant::now());
    }
}
