/// 极简 BM25（k1=1.5, b=0.75），中文按字 fallback：非 ASCII 连续段切单字，ASCII 切词。
/// 零依赖、确定性，够 memory 召回用。
pub struct Bm25 {
    docs: Vec<Vec<String>>,
    avgdl: f64,
    df: std::collections::HashMap<String, usize>,
    k1: f64,
    b: f64,
}

fn tokenize(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut is_ascii_word = false;
    for ch in text.chars() {
        let ch_ascii = ch.is_ascii_alphanumeric();
        if ch.is_whitespace() || ch == ',' || ch == '.' || ch == ';' || ch == ':' {
            if !cur.is_empty() {
                out.push(cur.clone());
                cur.clear();
            }
            is_ascii_word = false;
        } else if ch_ascii {
            cur.push(ch.to_ascii_lowercase());
            is_ascii_word = true;
        } else {
            if is_ascii_word && !cur.is_empty() {
                out.push(cur.clone());
                cur.clear();
            }
            is_ascii_word = false;
            // 中文等非 ASCII：按字切
            out.push(ch.to_string());
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

impl Bm25 {
    pub fn new() -> Self {
        Self { docs: vec![], avgdl: 1.0, df: std::collections::HashMap::new(), k1: 1.5, b: 0.75 }
    }

    pub fn add(&mut self, text: &str) {
        let toks = tokenize(text);
        if toks.is_empty() {
            return;
        }
        let mut seen = std::collections::HashSet::new();
        for t in &toks {
            if seen.insert(t.clone()) {
                *self.df.entry(t.clone()).or_insert(0) += 1;
            }
        }
        self.avgdl = if self.docs.is_empty() {
            toks.len() as f64
        } else {
            (self.avgdl * self.docs.len() as f64 + toks.len() as f64) / (self.docs.len() + 1) as f64
        };
        self.docs.push(toks);
    }

    pub fn len(&self) -> usize {
        self.docs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    pub fn search(&self, query: &str, top_k: usize) -> Vec<usize> {
        let q = tokenize(query);
        let n = self.docs.len() as f64;
        let mut scored: Vec<(usize, f64)> = vec![];
        for (i, doc) in self.docs.iter().enumerate() {
            let dl = doc.len() as f64;
            let mut score = 0.0;
            for qt in &q {
                let tf = doc.iter().filter(|t| *t == qt).count() as f64;
                if tf == 0.0 {
                    continue;
                }
                let dfn = self.df.get(qt).copied().unwrap_or(0) as f64;
                let idf = ((n - dfn + 0.5) / (dfn + 0.5) + 1.0).ln();
                score += idf * (tf * (self.k1 + 1.0)) / (tf + self.k1 * (1.0 - self.b + self.b * dl / self.avgdl));
            }
            if score > 0.0 {
                scored.push((i, score));
            }
        }
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(top_k);
        scored.into_iter().map(|(i, _)| i).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_relevant_doc_first() {
        let mut b = Bm25::new();
        b.add("rust harness loops guard");
        b.add("memory store jsonl journal");
        b.add("rust memory bm25 search");
        let hits = b.search("rust memory", 2);
        assert_eq!(hits[0], 2);
    }

    #[test]
    fn chinese_tokenized_by_char() {
        let mut b = Bm25::new();
        b.add("防无限循环护栏");
        b.add("记忆库召回");
        let hits = b.search("循环", 1);
        assert_eq!(hits[0], 0);
    }
}
