//! Mac-wide ownership for background chat tabs. Pane selection stays with Workspace.
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, OnceLock},
};
#[derive(Default)]
struct Registry {
    epoch: u64,
    owners: HashMap<String, HashSet<String>>,
    pending: HashMap<String, usize>,
}
#[derive(Clone)]
pub struct Coordinator {
    registry: Arc<Mutex<Registry>>,
    owner: String,
}
pub struct Reservation {
    coordinator: Coordinator,
    project: String,
}
impl Coordinator {
    pub fn new() -> Self {
        static REGISTRY: OnceLock<Arc<Mutex<Registry>>> = OnceLock::new();
        Self {
            registry: REGISTRY.get_or_init(Default::default).clone(),
            owner: uuid::Uuid::new_v4().to_string(),
        }
    }
    pub fn available(&self, id: &str) -> bool {
        self.registry
            .lock()
            .unwrap()
            .owners
            .get(id)
            .is_none_or(|owners| owners.is_empty() || owners.contains(&self.owner))
    }
    pub fn claim(&self, id: &str, explicit: bool) -> bool {
        let mut registry = self.registry.lock().unwrap();
        let owners = registry.owners.entry(id.to_owned()).or_default();
        if !explicit && !owners.is_empty() && !owners.contains(&self.owner) {
            return false;
        }
        owners.insert(self.owner.clone());
        true
    }
    pub fn release(&self, id: &str) {
        let mut registry = self.registry.lock().unwrap();
        if let Some(owners) = registry.owners.get_mut(id) {
            owners.remove(&self.owner);
            if owners.is_empty() {
                registry.owners.remove(id);
            }
        }
    }
    pub fn release_all(&self) {
        self.registry.lock().unwrap().owners.retain(|_, owners| {
            owners.remove(&self.owner);
            !owners.is_empty()
        });
    }
    pub fn epoch(&self) -> u64 {
        self.registry.lock().unwrap().epoch
    }
    pub fn invalidate(&self) {
        self.registry.lock().unwrap().epoch += 1;
    }
    pub fn creation_pending(&self, project: &str) -> bool {
        self.registry
            .lock()
            .unwrap()
            .pending
            .get(project)
            .copied()
            .unwrap_or(0)
            > 0
    }
    pub fn reserve(&self, project: &str) -> Reservation {
        self.invalidate();
        *self
            .registry
            .lock()
            .unwrap()
            .pending
            .entry(project.to_owned())
            .or_default() += 1;
        Reservation {
            coordinator: self.clone(),
            project: project.to_owned(),
        }
    }
}
impl Reservation {
    pub fn finish(self, id: &str) {
        self.coordinator.claim(id, false);
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        let mut r = self.coordinator.registry.lock().unwrap();
        r.epoch += 1;
        if let Some(n) = r.pending.get_mut(&self.project) {
            *n -= 1;
            if *n == 0 {
                r.pending.remove(&self.project);
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ownership_pending_and_explicit_open() {
        let a = Coordinator {
            registry: Default::default(),
            owner: "a".into(),
        };
        let b = Coordinator {
            registry: a.registry.clone(),
            owner: "b".into(),
        };
        assert!(a.claim("chat", false));
        assert!(!b.claim("chat", false));
        let epoch = a.epoch();
        let pending = a.reserve("project");
        assert!(b.creation_pending("project"));
        assert!(b.epoch() > epoch);
        drop(pending);
        assert!(!b.creation_pending("project"));
        a.release_all();
        assert!(b.claim("chat", false));
        assert!(a.claim("chat", true));
    }
}
