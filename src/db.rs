//! 모든 커넥션이 같이 쓰는 저장소입니다.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

/// 키와 값은 둘 다 바이트 그대로 둡니다. Redis 값에는 UTF-8이 아닌 바이트도 들어옵니다.
pub type Entries = HashMap<Vec<u8>, Vec<u8>>;

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
        // 락 안에서 하는 일이 insert/remove 한 번이라 반쯤 바뀐 상태가 남지 않기 때문입니다.
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
