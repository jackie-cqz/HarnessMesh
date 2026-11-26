//! The READMEs list the agents HarnessMesh looks for; a catalog row without
//! one there is an agent nobody is told about.
use hmesh_core::CATALOG;

#[test]
fn every_agent_with_a_command_is_in_the_readme_tables() {
    for (name, readme) in [
        ("README.md", include_str!("../../../README.md")),
        ("README_CN.md", include_str!("../../../README_CN.md")),
    ] {
        for spec in CATALOG {
            let Some(command) = spec.command else {
                continue;
            };
            let row = format!("| {} | `{command}", spec.label);
            assert!(readme.contains(&row), "{name} has no row starting {row:?}");
        }
    }
}
