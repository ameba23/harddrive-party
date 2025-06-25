#[derive(Clone)]
pub struct KnownPeers {
    db: sled::Tree,
}

impl KnownPeers {
    pub fn new(db: sled::Tree) -> Self {
        Self { db }
    }

    pub fn add_peer(&self, name: &str) -> anyhow::Result<()> {
        self.db.insert(name.as_bytes(), &[])?;
        Ok(())
    }

    pub fn has(&self, name: &str) -> bool {
        self.db.contains_key(name.as_bytes()).unwrap_or_default()
    }
}
