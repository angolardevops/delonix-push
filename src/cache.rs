//! Cache pequena com validade curta, para tirar da base o que se lê a cada mensagem e muda raramente
//! (a chave de servidor → projecto, e os limites do projecto). A validade é o atraso máximo com que uma
//! revogação de chave ou uma mudança de limites chega a OUTRAS instâncias; na própria instância limpa-se logo.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct Ttl<K, V> {
    ttl: Duration,
    map: Mutex<HashMap<K, (V, Instant)>>,
}

impl<K: Hash + Eq, V: Clone> Ttl<K, V> {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            map: Mutex::new(HashMap::new()),
        }
    }

    pub fn get(&self, k: &K) -> Option<V> {
        let m = self.map.lock().unwrap();
        m.get(k)
            .filter(|(_, at)| at.elapsed() < self.ttl)
            .map(|(v, _)| v.clone())
    }

    pub fn put(&self, k: K, v: V) {
        let mut m = self.map.lock().unwrap();
        if m.len() > 100_000 {
            m.retain(|_, (_, at)| at.elapsed() < self.ttl);
            if m.len() > 100_000 {
                m.clear();
            }
        }
        m.insert(k, (v, Instant::now()));
    }

    pub fn remove(&self, k: &K) {
        self.map.lock().unwrap().remove(k);
    }

    pub fn clear(&self) {
        self.map.lock().unwrap().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expires_and_clears() {
        let c: Ttl<u8, u8> = Ttl::new(Duration::from_millis(30));
        c.put(1, 10);
        assert_eq!(c.get(&1), Some(10));
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(c.get(&1), None, "expirou");
        c.put(2, 20);
        c.clear();
        assert_eq!(c.get(&2), None);
    }
}
