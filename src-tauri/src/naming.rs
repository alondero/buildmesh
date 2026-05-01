use rand::seq::IndexedRandom;

static ADJECTIVES: &[&str] = &[
    "amber", "bold", "brave", "bright", "calm", "clean", "clear", "cool",
    "crisp", "dark", "deep", "dry", "eager", "early", "easy", "fair",
    "fast", "fine", "firm", "flat", "fond", "free", "fresh", "full",
    "glad", "gold", "good", "grand", "great", "green", "happy", "hard",
    "high", "holy", "hot", "huge", "keen", "kind", "lame", "last",
    "late", "lazy", "lean", "light", "live", "lone", "long", "loud",
    "lucky", "mad", "main", "mild", "neat", "new", "nice", "noble",
    "odd", "old", "open", "pale", "plain", "proud", "pure", "quick",
    "quiet", "rare", "raw", "real", "red", "rich", "ripe", "rough",
    "round", "safe", "sharp", "shy", "slim", "slow", "small", "smart",
    "soft", "solid", "sour", "spare", "steep", "still", "strong", "sure",
    "sweet", "tall", "tame", "thick", "thin", "tight", "tiny", "tough",
    "true", "vast", "warm", "weak", "wide", "wild", "wise", "young",
    "zany",
];

static NOUNS: &[&str] = &[
    "arch", "badge", "barn", "beam", "bell", "bird", "blade", "bloom",
    "boat", "bolt", "bone", "book", "bow", "box", "breeze", "brick",
    "brook", "brush", "cairn", "cape", "cave", "chain", "charm", "chest",
    "cliff", "clock", "cloud", "coast", "coin", "coral", "crane", "creek",
    "cross", "crown", "dawn", "deer", "dome", "dove", "drum", "dune",
    "elm", "ember", "fern", "field", "flame", "flint", "fog", "forge",
    "fork", "fox", "frost", "gate", "gem", "glen", "grove", "hawk",
    "hedge", "heron", "hill", "horn", "isle", "jade", "jay", "knot",
    "lake", "lamp", "lane", "lark", "leaf", "ledge", "marsh", "maze",
    "mist", "moon", "moss", "nest", "oak", "oar", "orbit", "otter",
    "owl", "palm", "path", "peak", "pine", "plume", "pond", "quill",
    "rain", "ranch", "reef", "ridge", "river", "robin", "rock", "rose",
    "sage", "seed", "shade", "shell", "shore", "slate", "slope", "spark",
    "spire", "star", "stone", "storm", "sun", "surf", "swan", "thorn",
    "tide", "tower", "trail", "tree", "vale", "vine", "wave", "well",
    "wind", "wing", "wolf", "wood", "wren",
];

pub fn generate_random_name() -> String {
    let mut rng = rand::rng();
    let adj1 = ADJECTIVES.choose(&mut rng).unwrap();
    let adj2 = ADJECTIVES.choose(&mut rng).unwrap();
    let noun = NOUNS.choose(&mut rng).unwrap();
    format!("{}-{}-{}", adj1, adj2, noun)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_three_word_hyphenated_name() {
        let name = generate_random_name();
        let parts: Vec<&str> = name.split('-').collect();
        assert_eq!(parts.len(), 3);
        assert!(parts.iter().all(|p| !p.is_empty()));
        assert!(name.chars().all(|c| c.is_ascii_lowercase() || c == '-'));
    }
}
