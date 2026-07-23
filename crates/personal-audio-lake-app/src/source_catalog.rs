//! Outer composition bindings for optional device-source adapters.
//!
//! The view model receives these as ordinary presentation data. Inward-facing contracts and the
//! lakehouse never know which concrete executable implements a selected adapter.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SourceAdapterDescriptor {
    pub id: &'static str,
    pub executable: &'static str,
    pub name: &'static str,
    pub description: &'static str,
}

const SOURCE_ADAPTERS: [SourceAdapterDescriptor; 1] = [SourceAdapterDescriptor {
    id: "pocket",
    executable: "personal-audio-lake-pocket",
    name: "Pocket recorder (Compatibility)",
    description: "Pair and collect recordings from a supported Pocket recorder over Bluetooth.",
}];

pub(crate) fn all() -> &'static [SourceAdapterDescriptor] {
    &SOURCE_ADAPTERS
}

pub(crate) fn find(id: &str) -> Option<SourceAdapterDescriptor> {
    SOURCE_ADAPTERS
        .iter()
        .copied()
        .find(|descriptor| descriptor.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_ids_are_unique_and_bind_to_executables() {
        for (index, descriptor) in all().iter().enumerate() {
            assert!(!descriptor.id.trim().is_empty());
            assert!(!descriptor.executable.trim().is_empty());
            assert!(!descriptor.name.trim().is_empty());
            assert!(!descriptor.description.trim().is_empty());
            assert!(
                !all()[index + 1..]
                    .iter()
                    .any(|candidate| candidate.id == descriptor.id)
            );
            assert_eq!(find(descriptor.id), Some(*descriptor));
        }
        assert_eq!(find("unknown"), None);
    }
}
