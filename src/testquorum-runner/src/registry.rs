use crate::TestManager;

pub(crate) struct ManagerRegistry {
    managers: Vec<Box<dyn TestManager>>,
}

impl ManagerRegistry {
    pub(crate) fn new() -> Self {
        Self {
            managers: Vec::new(),
        }
    }

    pub(crate) fn register(&mut self, manager: Box<dyn TestManager>) {
        self.managers.push(manager);
    }

    pub(crate) fn managers(&self) -> &[Box<dyn TestManager>] {
        &self.managers
    }
}

impl Default for ManagerRegistry {
    fn default() -> Self {
        Self::new()
    }
}
