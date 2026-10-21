pub struct Cart {
    items: Vec<u32>,
}

impl Cart {
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn total(&self) -> u32 {
        self.items.iter().sum()
    }

    pub fn savings(&self) -> u32 {
        self.total() / 5
    }
}

pub fn unrelated() -> u32 {
    42
}
