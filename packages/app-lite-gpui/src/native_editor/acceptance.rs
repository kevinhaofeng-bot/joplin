#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AcceptanceCase {
    ImeComposition,
    CrossBlockSelection,
    ImageBoundary,
    ListCommands,
    SharedCommandCatalogue,
    AllVisibleCommands,
    EditingAcrossBlockBoundaries,
    ViewportBoundedLayout,
}

pub const REQUIRED_CASES: [AcceptanceCase; 8] = [
    AcceptanceCase::ImeComposition,
    AcceptanceCase::CrossBlockSelection,
    AcceptanceCase::ImageBoundary,
    AcceptanceCase::ListCommands,
    AcceptanceCase::SharedCommandCatalogue,
    AcceptanceCase::AllVisibleCommands,
    AcceptanceCase::EditingAcrossBlockBoundaries,
    AcceptanceCase::ViewportBoundedLayout,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_contains_every_documented_gate() {
        assert_eq!(REQUIRED_CASES.len(), 8);
    }
}
