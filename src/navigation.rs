//! A rooted, owner-driven navigation stack, independent of GPUI and screen types.

/// Identity of one mounted entry. Re-pushing the same component creates a new identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntryId(u64);

/// A component and its owner-defined pop lifecycle data.
pub struct Entry<T, L> {
    pub id: EntryId,
    pub component: T,
    /// Absent for the permanent root; returned to the owner when a child is popped.
    pub on_pop: Option<L>,
}

/// Root cannot be removed or replaced. Covered entries stay owned by the stack.
///
/// `L` is the on-pop equivalent: the owner consumes it after `pop` (or each entry
/// returned by `pop_to_root`) to perform cleanup/focus restoration with its UI context.
/// The stack never invokes callbacks during destruction.
pub struct Navigation<T, L> {
    root: Entry<T, L>,
    children: Vec<Entry<T, L>>,
    next_id: u64,
}

impl<T, L> Navigation<T, L> {
    pub fn new(root: T) -> Self {
        Self {
            root: Entry {
                id: EntryId(0),
                component: root,
                on_pop: None,
            },
            children: Vec::new(),
            next_id: 1,
        }
    }

    pub fn root(&self) -> &Entry<T, L> {
        &self.root
    }

    pub fn current(&self) -> &Entry<T, L> {
        self.children.last().unwrap_or(&self.root)
    }

    /// Mounted entries in root-to-current order, for owner-specific singleton policies.
    pub fn entries(&self) -> impl DoubleEndedIterator<Item = &Entry<T, L>> {
        std::iter::once(&self.root).chain(self.children.iter())
    }

    /// Update owner lifecycle data without exposing mutable routes or entry identities.
    pub fn lifecycles_mut(&mut self) -> impl Iterator<Item = &mut L> {
        self.children
            .iter_mut()
            .filter_map(|entry| entry.on_pop.as_mut())
    }

    pub fn is_root(&self) -> bool {
        self.children.is_empty()
    }

    /// Number of mounted entries, including the permanent root.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn depth(&self) -> usize {
        self.children.len() + 1
    }

    /// Mount a component with lifecycle data delivered exactly once on explicit pop.
    pub fn push(&mut self, component: T, on_pop: L) -> EntryId {
        let id = EntryId(self.next_id);
        self.next_id += 1;
        self.children.push(Entry {
            id,
            component,
            on_pop: Some(on_pop),
        });
        id
    }

    /// Unmount one child. At root this is a no-op, returning `None`.
    pub fn pop(&mut self) -> Option<Entry<T, L>> {
        self.children.pop()
    }

    /// Reveal a mounted entry, returning removed children in top-to-bottom order.
    /// An unknown identity leaves the stack unchanged and returns `None`.
    /// Owners must preflight any dirty-screen confirmation before calling this.
    pub fn pop_to(&mut self, id: EntryId) -> Option<Vec<Entry<T, L>>> {
        if id == self.root.id {
            return Some(self.pop_to_root());
        }
        let index = self.children.iter().position(|entry| entry.id == id)?;
        Some(
            self.children
                .split_off(index + 1)
                .into_iter()
                .rev()
                .collect(),
        )
    }

    /// Unmount every child, returning lifecycle data in top-to-bottom pop order.
    pub fn pop_to_root(&mut self) -> Vec<Entry<T, L>> {
        self.children.drain(..).rev().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_cannot_underflow() {
        let mut navigation = Navigation::<_, ()>::new("Home");
        assert!(navigation.pop().is_none());
        assert!(navigation.pop_to_root().is_empty());
        assert_eq!(navigation.depth(), 1);
        assert_eq!(navigation.current().id, navigation.root().id);
        assert_eq!(navigation.root().component, "Home");
    }

    #[test]
    fn pop_returns_component_and_lifecycle_once_and_reveals_previous_entry() {
        let mut navigation = Navigation::new("Home");
        let chat = navigation.push("Chat", "restore Home");
        navigation.push("Settings", "restore Chat");
        assert_eq!(navigation.depth(), 3);
        let popped = navigation.pop().unwrap();
        assert_eq!(popped.component, "Settings");
        assert_eq!(popped.on_pop, Some("restore Chat"));
        assert_eq!(navigation.current().id, chat);
        assert_eq!(navigation.pop().unwrap().on_pop, Some("restore Home"));
        assert!(navigation.pop().is_none());
    }

    #[test]
    fn pop_to_root_returns_lifecycles_in_pop_order_and_ids_are_not_reused() {
        let mut navigation = Navigation::new("Home");
        let first = navigation.push("Chat", 1);
        navigation.push("Settings", 2);
        let popped = navigation.pop_to_root();
        assert_eq!(
            popped.iter().map(|entry| entry.on_pop).collect::<Vec<_>>(),
            vec![Some(2), Some(1)]
        );
        assert!(navigation.is_root());
        assert_ne!(navigation.push("Chat", 3), first);
    }

    #[test]
    fn covered_component_keeps_owned_state() {
        let mut navigation = Navigation::new(String::from("Home"));
        navigation.push(String::from("unsaved draft"), ());
        navigation.push(String::from("Settings"), ());
        navigation.pop();
        assert_eq!(navigation.current().component, "unsaved draft");
    }
    #[test]
    fn pop_to_reveals_existing_entry_and_returns_all_removed_lifecycles() {
        let mut navigation = Navigation::new("Home");
        let settings = navigation.push("Settings", 1);
        navigation.push("Editor", 2);
        navigation.push("Detail", 3);
        let removed = navigation.pop_to(settings).unwrap();
        assert_eq!(
            removed.iter().map(|entry| entry.on_pop).collect::<Vec<_>>(),
            vec![Some(3), Some(2)]
        );
        assert_eq!(navigation.current().id, settings);
        assert_eq!(
            navigation
                .entries()
                .map(|entry| entry.component)
                .collect::<Vec<_>>(),
            vec!["Home", "Settings"]
        );
        assert!(navigation.pop_to(settings).unwrap().is_empty());
    }

    #[test]
    fn pop_to_unknown_identity_is_atomic_and_leaves_stack_unchanged() {
        let mut navigation = Navigation::new("Home");
        let settings = navigation.push("Settings", ());
        assert!(navigation.pop_to(EntryId(999)).is_none());
        assert_eq!(navigation.depth(), 2);
        assert_eq!(navigation.current().id, settings);
    }
    #[test]
    fn lifecycle_updates_preserve_root_routes_and_entry_identities() {
        let mut navigation = Navigation::new("Home");
        navigation.push("Manager", "deleted control");
        navigation.push("Settings", "surviving control");
        let before = navigation
            .entries()
            .map(|entry| (entry.id, entry.component))
            .collect::<Vec<_>>();
        for lifecycle in navigation.lifecycles_mut() {
            if *lifecycle == "deleted control" {
                *lifecycle = "Home input";
            }
        }
        assert_eq!(
            navigation
                .entries()
                .map(|entry| (entry.id, entry.component))
                .collect::<Vec<_>>(),
            before
        );
        assert!(navigation.root().on_pop.is_none());
        assert_eq!(navigation.pop().unwrap().on_pop, Some("surviving control"));
        assert_eq!(navigation.pop().unwrap().on_pop, Some("Home input"));
        assert!(navigation.is_root());
    }
}
