//! 모든 커넥션이 같이 쓰는 저장소입니다.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{SendError, Sender};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

/// 이 간격마다 저장소 전체를 훑어 만료된 키를 지웁니다. 진짜 Redis 기본값(hz 10)과 같은 100ms입니다.
const ACTIVE_EXPIRE_INTERVAL: Duration = Duration::from_millis(100);

/// 키 하나에 들어가는 값의 종류입니다. 경우마다 안에 들고 있는 데이터가 다릅니다.
/// 값을 꺼내 쓰는 곳은 `match`로 모든 경우에 답해야 컴파일됩니다. 종류가 다를 때의 답이 WRONGTYPE입니다.
pub enum Value {
    /// 바이트 그대로 둡니다. Redis 값에는 UTF-8이 아닌 바이트도 들어옵니다.
    Str(Vec<u8>),
    /// 양 끝에서 넣고 빼는 줄입니다. `VecDeque`는 앞뒤 어느 쪽에서 넣고 빼도 나머지를 옮기지 않습니다.
    /// 빈 리스트는 저장하지 않습니다. 마지막 원소를 빼는 쪽이 키도 같이 지웁니다.
    List(VecDeque<Vec<u8>>),
}

/// 리스트의 어느 쪽 끝인지입니다. `L`로 시작하는 커맨드는 왼쪽(앞), `R`은 오른쪽(뒤)입니다.
#[derive(Clone, Copy)]
pub enum End {
    Left,
    Right,
}

/// 대기자에게 넘겨주는 것입니다. (꺼낸 키, 꺼낸 값) 순서입니다.
pub type Delivery = (Vec<u8>, Vec<u8>);

/// `BLPOP`/`BRPOP`으로 빈 리스트 앞에서 자고 있는 커넥션 하나입니다.
pub struct Waiter {
    /// 기다리는 키 전부입니다. 한 곳에서 받으면 나머지 줄에서도 이름을 지워야 해서 들고 있습니다.
    keys: Vec<Vec<u8>>,
    end: End,
    /// 이 대기자만 받는 우편함의 보내는 쪽입니다. 받는 쪽(`Receiver`)은 자고 있는 커넥션 스레드가 들고 있습니다.
    sender: Sender<Delivery>,
}

impl Waiter {
    /// `BLPOP`이면 앞, `BRPOP`이면 뒤에서 꺼낸 값을 받습니다.
    pub fn end(&self) -> End {
        self.end
    }

    /// 값을 우편함에 넣습니다. 자고 있던 스레드는 이걸로 깨어납니다.
    /// 받는 쪽이 이미 사라졌으면 못 보낸 값을 그대로 돌려줍니다.
    pub fn deliver(self, key: Vec<u8>, value: Vec<u8>) -> Result<(), Vec<u8>> {
        // `send`는 실패하면 보내려던 것을 에러 안에 담아 돌려줍니다. 거기서 값만 꺼냅니다.
        self.sender
            .send((key, value))
            .map_err(|SendError((_, value))| value)
    }
}

/// 값과 만료 시각을 같이 둡니다. 키를 지우면 만료 시각도 같이 사라져서, 둘이 어긋날 일이 없습니다.
pub struct Entry {
    pub value: Value,
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
///
/// 대기자 명단도 여기 같이 둡니다. 저장소와 같은 락 안에 있어야 "넣기 → 명단 확인 → 넘겨주기"가
/// 락 한 번 안에서 끝나고, 그 사이에 다른 커넥션이 끼어들 수 없습니다.
#[derive(Default)]
pub struct Entries {
    map: HashMap<Vec<u8>, Entry>,
    /// 번호표 → 대기자입니다. 여기에 있으면 아직 아무 값도 못 받은 것입니다.
    waiters: HashMap<u64, Waiter>,
    /// 키마다 서 있는 사람 줄입니다. 번호표만 먼저 온 순서대로 들어 있습니다.
    queues: HashMap<Vec<u8>, VecDeque<u64>>,
    next_waiter_id: u64,
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

    /// 대기자를 기다리는 키마다 줄 맨 뒤에 세우고 번호표를 돌려줍니다.
    pub fn add_waiter(&mut self, keys: Vec<Vec<u8>>, end: End, sender: Sender<Delivery>) -> u64 {
        let id = self.next_waiter_id;
        self.next_waiter_id = id.wrapping_add(1);
        for key in &keys {
            // 줄(`queues`)과 대기자(`keys`)가 키를 각자 들고 있어야 해서 여기서는 복사합니다.
            let queue = self.queues.entry(key.clone()).or_default();
            // `BLPOP q q 0`처럼 같은 키를 두 번 적어도 줄에는 한 번만 섭니다.
            if !queue.contains(&id) {
                queue.push_back(id);
            }
        }
        self.waiters.insert(id, Waiter { keys, end, sender });
        id
    }

    /// `key`를 제일 먼저 기다린 대기자를 명단에서 꺼냅니다. 그 대기자가 서 있던 다른 줄에서도 이름을 지웁니다.
    /// 안 지우면 주인 없는 번호표가 그 줄 앞에 남아서, 뒤에 선 진짜 대기자가 값을 못 받습니다.
    pub fn take_waiter(&mut self, key: &[u8]) -> Option<Waiter> {
        let id = self.queues.get_mut(key)?.pop_front()?;
        let waiter = self.waiters.remove(&id)?;
        self.leave_queues(id, &waiter.keys);
        Some(waiter)
    }

    /// 시간이 다 된 대기자가 스스로 명단에서 빠질 때 부릅니다.
    /// true면 아직 명단에 있었던 것, 즉 아무 값도 못 받은 것입니다.
    /// false면 넣는 쪽이 먼저 꺼내 간 것이라, 값이 이미 우편함에 들어 있습니다.
    pub fn remove_waiter(&mut self, id: u64) -> bool {
        let Some(waiter) = self.waiters.remove(&id) else {
            return false;
        };
        self.leave_queues(id, &waiter.keys);
        true
    }

    fn leave_queues(&mut self, id: u64, keys: &[Vec<u8>]) {
        for key in keys {
            let Some(queue) = self.queues.get_mut(key) else {
                continue;
            };
            queue.retain(|&waiting| waiting != id);
            // 아무도 안 서 있는 줄은 남겨 두지 않습니다.
            if queue.is_empty() {
                self.queues.remove(key);
            }
        }
    }

    /// 테스트가 "대기자가 명단에 올라갔는지"를 sleep 없이 확인하는 데 씁니다.
    #[cfg(test)]
    pub fn waiter_count(&self) -> usize {
        self.waiters.len()
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
        // 락 안에서 패닉할 수 있는 코드(`unwrap`, 범위 밖 인덱스)를 쓰지 않아서 반쯤 바뀐 상태가 남지 않기 때문입니다.
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
