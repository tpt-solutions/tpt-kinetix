/// A single named test vector in a [`Corpus`].
pub struct CorpusEntry {
    pub name: String,
    pub data: Vec<u8>,
}

/// A named collection of test byte vectors for fuzz regression.
pub struct Corpus {
    pub name: String,
    pub entries: Vec<CorpusEntry>,
}

impl Corpus {
    /// Create an empty corpus with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            entries: Vec::new(),
        }
    }

    /// Add a named entry to the corpus.
    pub fn add(&mut self, name: impl Into<String>, data: Vec<u8>) {
        self.entries.push(CorpusEntry {
            name: name.into(),
            data,
        });
    }

    /// Add well-known edge-case inputs: empty, single byte 0x00, all-zeros
    /// (64 bytes), and all-0xFF (64 bytes).
    pub fn add_edge_cases(&mut self) {
        self.add("empty", vec![]);
        self.add("single_zero", vec![0x00]);
        self.add("single_ff", vec![0xFF]);
        self.add("all_zeros_64", vec![0x00; 64]);
        self.add("all_ff_64", vec![0xFF; 64]);
        self.add("single_byte_0x01", vec![0x01]);
        // A repeating pattern that can stress many parsers.
        self.add(
            "alternating_00_ff",
            (0..64)
                .map(|i| if i % 2 == 0 { 0x00 } else { 0xFF })
                .collect(),
        );
    }

    /// Iterate over all entries in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = &CorpusEntry> {
        self.entries.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_corpus_has_no_entries() {
        let c = Corpus::new("test");
        assert_eq!(c.entries.len(), 0);
    }

    #[test]
    fn add_edge_cases_produces_entries() {
        let mut c = Corpus::new("fuzz");
        c.add_edge_cases();
        assert!(c.entries.len() >= 4);
        // First entry must be empty.
        assert!(c.entries[0].data.is_empty());
    }

    #[test]
    fn add_and_iter() {
        let mut c = Corpus::new("demo");
        c.add("hello", b"hello world".to_vec());
        let names: Vec<&str> = c.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["hello"]);
    }
}
