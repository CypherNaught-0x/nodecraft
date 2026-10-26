use nodecraft_graph::{Change, Changeset, Project};
use nodecraft_types::ProjectId;
use proptest::prelude::*;

proptest! {
    #[test]
    fn identity_changes_round_trip_and_invert_exactly(old in any::<[u8; 16]>(), new in any::<[u8; 16]>()) {
        let original = Project::new(ProjectId::from_bytes(old), "Identity").unwrap();
        let change = Changeset::new(vec![Change::SetProjectId { id: ProjectId::from_bytes(new) }]);
        let wire = serde_json::to_string(&change).unwrap();
        let decoded: Changeset = serde_json::from_str(&wire).unwrap();
        let mut project = original.clone();
        let inverse = decoded.apply(&mut project).unwrap();
        prop_assert_eq!(project.id(), ProjectId::from_bytes(new));
        let redo = inverse.apply(&mut project).unwrap();
        prop_assert_eq!(&project, &original);
        redo.apply(&mut project).unwrap();
        prop_assert_eq!(project.id(), ProjectId::from_bytes(new));
    }
}
