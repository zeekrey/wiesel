//! The full Lucide icon set (https://lucide.dev/icons), embedded so the packaged app never
//! loads icons from disk or the network. Regenerate with `scripts/update-lucide-icons.py`.
use std::sync::OnceLock;

const DATA: &str = include_str!("../resources/lucide/icons.tsv");

pub struct Icon {
    pub name: &'static str,
    pub tags: Vec<&'static str>,
    /// Inner SVG markup for a 24×24 stroked icon.
    pub body: &'static str,
}

/// Every icon, sorted by name.
pub fn icons() -> &'static [Icon] {
    static ICONS: OnceLock<Vec<Icon>> = OnceLock::new();
    ICONS.get_or_init(|| {
        let mut icons: Vec<Icon> = DATA
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .filter_map(|line| {
                let mut fields = line.splitn(3, '\t');
                let name = fields.next()?;
                let tags = fields.next()?;
                let body = fields.next()?;
                Some(Icon {
                    name,
                    tags: tags.split('|').filter(|tag| !tag.is_empty()).collect(),
                    body,
                })
            })
            .collect();
        icons.sort_unstable_by_key(|icon| icon.name);
        icons
    })
}

pub fn find(name: &str) -> Option<&'static Icon> {
    let icons = icons();
    icons
        .binary_search_by(|icon| icon.name.cmp(name))
        .ok()
        .map(|index| &icons[index])
}

/// Rank for a query that is already lowercase and space-normalized; lower is better.
fn rank(icon: &Icon, query: &str) -> Option<u8> {
    let name = icon.name.replace('-', " ");
    if name == query {
        return Some(0);
    }
    if name.starts_with(query) {
        return Some(1);
    }
    if name.contains(query) {
        return Some(2);
    }
    let tags = || icon.tags.iter().map(|tag| tag.to_lowercase());
    if tags().any(|tag| tag == query) {
        return Some(3);
    }
    if tags().any(|tag| tag.starts_with(query)) {
        return Some(4);
    }
    if tags().any(|tag| tag.contains(query)) {
        return Some(5);
    }
    // Every word appears somewhere, e.g. "arrow circle" or "chat bubble".
    let words: Vec<_> = query.split(' ').collect();
    (words.len() > 1
        && words
            .iter()
            .all(|word| name.contains(word) || tags().any(|tag| tag.contains(word))))
    .then_some(6)
}

/// Icon names matching a name or tag query, best matches first, then alphabetically.
/// An empty query returns every icon.
pub fn search(query: &str) -> Vec<&'static str> {
    let query = query
        .to_lowercase()
        .replace('-', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if query.is_empty() {
        return icons().iter().map(|icon| icon.name).collect();
    }
    let mut ranked: Vec<_> = icons()
        .iter()
        .filter_map(|icon| rank(icon, &query).map(|rank| (rank, icon.name)))
        .collect();
    ranked.sort_unstable();
    ranked.into_iter().map(|(_, name)| name).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embeds_the_full_sorted_icon_set_with_tags() {
        let icons = icons();
        assert!(icons.len() > 1500);
        assert!(icons.windows(2).all(|pair| pair[0].name < pair[1].name));
        assert!(icons.iter().all(|icon| icon.body.starts_with('<')));
        assert!(find("message-circle").is_some());
        assert!(!find("activity").unwrap().tags.is_empty());
        assert!(find("../wand").is_none());
    }

    #[test]
    fn searches_names_before_tags() {
        let results = search("  Heart ");
        assert_eq!(results[0], "heart");
        assert!(results.contains(&"activity"), "tag match for heart rate");
        let heart = results
            .iter()
            .position(|name| *name == "heart-pulse")
            .unwrap();
        let activity = results.iter().position(|name| *name == "activity").unwrap();
        assert!(heart < activity);
    }

    #[test]
    fn searches_multiword_names_and_tags() {
        assert_eq!(search("message circle")[0], "message-circle");
        assert_eq!(search("message-circle")[0], "message-circle");
        assert!(search("font size").contains(&"a-arrow-down"));
        assert!(search("zzzz-not-an-icon").is_empty());
        assert_eq!(search("").len(), icons().len());
    }
}
